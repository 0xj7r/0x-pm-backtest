//! `shadow` subcommand: run the validated pm-alpha fade LIVE in LOG-ONLY mode.
//!
//! Zero orders, zero capital. This module contains NO order-placement code by
//! construction: it has no signer, no CLOB REST client, and never sends
//! anything on a websocket except subscriptions and pings. It mirrors the
//! entry semantics of `pm_alpha::harness::replay::execute` (edge vs the real
//! Up/Down touch asks, first threshold crossing per market, 1s cadence) and
//! logs WOULD_ENTER / QUOTE_PROBE / WOULD_EXIT / SUMMARY records as JSONL.

use anyhow::{Context, Result};
use pm_alpha::{AlphaModel, AlphaModelConfig, ExoState, MarketMeta, Token};
use pm_types::{SpotHistory, SpotTick};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

/// Spot ticks retained in the rolling buffer (>= 2h required for vol3600).
const SPOT_KEEP_SECS: i64 = 7_800;
/// Mirror of the harness entry deadline (`stop_before_close_s` default).
const STOP_BEFORE_CLOSE_S: i64 = 90;
/// Rolling cap on receipt-minus-exchange latency samples.
const LATENCY_SAMPLE_CAP: usize = 4_096;

#[derive(Debug, Clone)]
pub struct ShadowArgs {
    pub slug_prefix: String,
    pub edge_threshold: f64,
    pub vol_lookback_s: u32,
    pub exit_after_s: u32,
    pub latency_probe_ms: u64,
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Up,
    Down,
}

impl Side {
    fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq)]
pub struct Touch {
    pub price: f64,
    pub size: f64,
}

/// One JSONL record. `type` is the discriminant so downstream analysis can
/// `jq 'select(.type == "would_enter")'`.
#[derive(Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LogEvent {
    WouldEnter {
        ts_utc: String,
        slug: String,
        side: &'static str,
        p_exo: f64,
        touch_price: f64,
        touch_size: f64,
        edge: f64,
        strike: f64,
        strike_source: &'static str,
        sigma_bar_bps: f64,
    },
    QuoteProbe {
        ts_utc: String,
        slug: String,
        side: &'static str,
        entry_touch_price: f64,
        /// Same-or-better price still available on our side's ask ladder.
        still_quoted: bool,
        current_touch: Option<Touch>,
        /// Displayed size at prices <= our entry touch (top-5 ladder).
        remaining_size: f64,
    },
    WouldExit {
        ts_utc: String,
        slug: String,
        side: &'static str,
        entry_touch_price: f64,
        /// Best bid on our side's token at exit time.
        exit_touch: Option<Touch>,
        /// Side-oriented: exit_bid_for_our_side - entry_touch_price.
        mark_pnl_per_share: Option<f64>,
    },
    Summary {
        ts_utc: String,
        n_active_markets: usize,
        n_entries_total: u64,
        probe_still_quoted_rate: Option<f64>,
        mean_mark_pnl_per_share: Option<f64>,
        binance_feed_age_ms: Option<i64>,
        book_feed_age_ms: Option<i64>,
        median_binance_receipt_minus_exchange_ms: Option<i64>,
        median_book_receipt_minus_exchange_ms: Option<i64>,
    },
}

fn ts_utc(now_ns: i64) -> String {
    chrono::DateTime::from_timestamp_nanos(now_ns)
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn median(samples: &VecDeque<i64>) -> Option<i64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted: Vec<i64> = samples.iter().copied().collect();
    sorted.sort_unstable();
    Some(sorted[sorted.len() / 2])
}

/// Price ladder for one token, keyed by integer price (price * 1e5) so f64
/// never acts as a map key. Top-5 is enforced on read, not on write, so a
/// `price_change` that removes the touch correctly reveals the level behind.
#[derive(Debug, Default, Clone)]
pub struct Ladder {
    bids: BTreeMap<i64, f64>,
    asks: BTreeMap<i64, f64>,
    pub last_exchange_ms: Option<i64>,
    pub last_receipt_ms: Option<i64>,
}

fn price_key(price: f64) -> i64 {
    (price * 100_000.0).round() as i64
}

fn key_price(key: i64) -> f64 {
    key as f64 / 100_000.0
}

impl Ladder {
    pub fn best_ask(&self) -> Option<Touch> {
        self.asks.iter().next().map(|(k, s)| Touch {
            price: key_price(*k),
            size: *s,
        })
    }

    pub fn best_bid(&self) -> Option<Touch> {
        self.bids.iter().next_back().map(|(k, s)| Touch {
            price: key_price(*k),
            size: *s,
        })
    }

    /// Displayed ask size at prices same-or-better than `limit_price`,
    /// looking only at the top 5 ask levels.
    pub fn ask_size_at_or_below(&self, limit_price: f64) -> f64 {
        let limit = price_key(limit_price);
        self.asks
            .iter()
            .take(5)
            .filter(|(k, _)| **k <= limit)
            .map(|(_, s)| *s)
            .sum()
    }
}

/// One discovered btc-updown-5m window.
#[derive(Debug, Clone)]
pub struct MarketWindow {
    pub slug: String,
    pub open_ts_s: i64,
    pub close_ts_s: i64,
    pub up_token: String,
    pub down_token: String,
    /// True strike from Gamma/crypto-price (`openPrice`); None until present.
    pub gamma_strike: Option<f64>,
    /// First threshold crossing only: one shadow entry per market.
    pub entered: bool,
}

/// A logged WOULD_ENTER awaiting its quote probe and mark-to-book exit.
#[derive(Debug, Clone)]
struct PendingTrade {
    slug: String,
    side: Side,
    token: String,
    entry_touch_price: f64,
    probe_due_ns: i64,
    probe_done: bool,
    exit_due_ns: i64,
    exit_done: bool,
}

#[derive(Debug, Default)]
struct SummaryStats {
    entries_total: u64,
    probes_total: u64,
    probes_quoted: u64,
    mark_pnl_sum: f64,
    mark_pnl_count: u64,
}

/// All shadow decision/bookkeeping state. Pure with respect to I/O: feeds
/// push events in, the runner polls decisions/probes/exits out. Unit tests
/// drive it with synthetic events and injected clocks.
pub struct ShadowCore {
    cfg: ShadowConfig,
    model: AlphaModel,
    spot: VecDeque<SpotTick>,
    spot_last_receipt_ms: Option<i64>,
    spot_receipt_deltas_ms: VecDeque<i64>,
    books: HashMap<String, Ladder>,
    book_last_receipt_ms: Option<i64>,
    book_receipt_deltas_ms: VecDeque<i64>,
    markets: HashMap<String, MarketWindow>,
    pending: Vec<PendingTrade>,
    stats: SummaryStats,
}

#[derive(Debug, Clone)]
pub struct ShadowConfig {
    pub edge_threshold: f64,
    pub vol_lookback_s: u32,
    pub exit_after_s: u32,
    pub latency_probe_ms: u64,
}

impl ShadowCore {
    pub fn new(cfg: ShadowConfig) -> Self {
        let model = AlphaModel {
            cfg: AlphaModelConfig {
                vol_lookback_s: cfg.vol_lookback_s,
                vol_sample_dt_s: 1,
                momentum_lookback_s: 0,
                momentum_weight: 1.0,
            },
            calibrator: None,
        };
        Self {
            cfg,
            model,
            spot: VecDeque::new(),
            spot_last_receipt_ms: None,
            spot_receipt_deltas_ms: VecDeque::new(),
            books: HashMap::new(),
            book_last_receipt_ms: None,
            book_receipt_deltas_ms: VecDeque::new(),
            markets: HashMap::new(),
            pending: Vec::new(),
            stats: SummaryStats::default(),
        }
    }

    // Feed ingestion

    pub fn push_spot(
        &mut self,
        exchange_ms: i64,
        receipt_ms: i64,
        price: f64,
        quantity: f64,
        is_buyer_maker: bool,
    ) {
        if !(price.is_finite() && price > 0.0) {
            return;
        }
        self.spot.push_back(SpotTick {
            ts_ns: exchange_ms * 1_000_000,
            price,
            quantity: quantity as f32,
            is_buyer_maker,
        });
        self.spot_last_receipt_ms = Some(receipt_ms);
        push_capped(&mut self.spot_receipt_deltas_ms, receipt_ms - exchange_ms);
        let cutoff_ns = (exchange_ms - SPOT_KEEP_SECS * 1_000) * 1_000_000;
        while self.spot.front().is_some_and(|t| t.ts_ns < cutoff_ns) {
            self.spot.pop_front();
        }
    }

    pub fn spot_history(&self) -> SpotHistory {
        SpotHistory::new(self.spot.iter().copied().collect())
    }

    pub fn apply_book_snapshot(
        &mut self,
        token: &str,
        bids: &[(f64, f64)],
        asks: &[(f64, f64)],
        exchange_ms: Option<i64>,
        receipt_ms: i64,
    ) {
        let ladder = self.books.entry(token.to_string()).or_default();
        ladder.bids = bids
            .iter()
            .filter(|(p, s)| *p > 0.0 && *p < 1.0 && *s > 0.0)
            .map(|(p, s)| (price_key(*p), *s))
            .collect();
        ladder.asks = asks
            .iter()
            .filter(|(p, s)| *p > 0.0 && *p < 1.0 && *s > 0.0)
            .map(|(p, s)| (price_key(*p), *s))
            .collect();
        self.record_book_receipt(token, exchange_ms, receipt_ms);
    }

    /// Polymarket `price_change`: `side` is BUY (bid level) or SELL (ask
    /// level); `size` is the new aggregate size at `price` (0 removes it).
    pub fn apply_price_change(
        &mut self,
        token: &str,
        is_buy_side: bool,
        price: f64,
        size: f64,
        exchange_ms: Option<i64>,
        receipt_ms: i64,
    ) {
        if !(price > 0.0 && price < 1.0 && size.is_finite() && size >= 0.0) {
            return;
        }
        let ladder = self.books.entry(token.to_string()).or_default();
        let side = if is_buy_side {
            &mut ladder.bids
        } else {
            &mut ladder.asks
        };
        if size <= 0.0 {
            side.remove(&price_key(price));
        } else {
            side.insert(price_key(price), size);
        }
        self.record_book_receipt(token, exchange_ms, receipt_ms);
    }

    fn record_book_receipt(&mut self, token: &str, exchange_ms: Option<i64>, receipt_ms: i64) {
        if let Some(ladder) = self.books.get_mut(token) {
            ladder.last_exchange_ms = exchange_ms.or(ladder.last_exchange_ms);
            ladder.last_receipt_ms = Some(receipt_ms);
        }
        self.book_last_receipt_ms = Some(receipt_ms);
        if let Some(ex) = exchange_ms {
            push_capped(&mut self.book_receipt_deltas_ms, receipt_ms - ex);
        }
    }

    /// Insert a discovered market or refresh its strike. Never resets
    /// `entered` for a market we already acted on.
    pub fn upsert_market(&mut self, market: MarketWindow) {
        match self.markets.get_mut(&market.slug) {
            Some(existing) => {
                if existing.gamma_strike.is_none() {
                    existing.gamma_strike = market.gamma_strike;
                }
            }
            None => {
                self.markets.insert(market.slug.clone(), market);
            }
        }
    }

    /// Drop windows that closed more than 10 minutes ago, plus their books.
    pub fn prune(&mut self, now_ns: i64) {
        let cutoff_s = now_ns / 1_000_000_000 - 600;
        let dead: Vec<String> = self
            .markets
            .values()
            .filter(|m| m.close_ts_s < cutoff_s)
            .map(|m| m.slug.clone())
            .collect();
        for slug in dead {
            if let Some(m) = self.markets.remove(&slug) {
                self.books.remove(&m.up_token);
                self.books.remove(&m.down_token);
            }
        }
    }

    pub fn subscribed_tokens(&self) -> Vec<String> {
        let mut tokens: Vec<String> = self
            .markets
            .values()
            .flat_map(|m| [m.up_token.clone(), m.down_token.clone()])
            .collect();
        tokens.sort();
        tokens.dedup();
        tokens
    }
}

fn push_capped(buf: &mut VecDeque<i64>, value: i64) {
    buf.push_back(value);
    while buf.len() > LATENCY_SAMPLE_CAP {
        buf.pop_front();
    }
}

impl ShadowCore {
    /// One decision pass over all active windows, mirroring the harness:
    /// belief from ExoState, edge per side vs the REAL touch asks, enter on
    /// the first crossing of `edge_threshold`. Call at ~1s cadence.
    pub fn decide(&mut self, now_ns: i64) -> Vec<LogEvent> {
        let spot = self.spot_history();
        let mut out = Vec::new();
        let mut entries: Vec<PendingTrade> = Vec::new();

        for m in self.markets.values_mut() {
            let open_ns = m.open_ts_s * 1_000_000_000;
            let close_ns = m.close_ts_s * 1_000_000_000;
            let deadline_ns = close_ns - STOP_BEFORE_CLOSE_S * 1_000_000_000;
            if m.entered || now_ns < open_ns || now_ns >= deadline_ns {
                continue;
            }
            let Some((strike, strike_source)) = ({
                if let Some(s) = m.gamma_strike {
                    Some((s, "gamma"))
                } else {
                    spot.price_at_or_before(open_ns).map(|p| (p, "binance_proxy"))
                }
            }) else {
                continue;
            };
            let Some(token) = Token::from_slug(&m.slug) else {
                continue;
            };
            let state = ExoState {
                spot: &spot,
                perp: None,
                market: MarketMeta {
                    token,
                    window_secs: (m.close_ts_s - m.open_ts_s).max(1) as u32,
                    open_ts_ns: open_ns,
                    close_ts_ns: close_ns,
                    strike,
                },
                now_ns,
            };
            let Some(ev) = self.model.evaluate(&state, false) else {
                continue;
            };
            let (Some(up_ask), Some(down_ask)) = (
                self.books.get(&m.up_token).and_then(Ladder::best_ask),
                self.books.get(&m.down_token).and_then(Ladder::best_ask),
            ) else {
                continue;
            };

            // Same side selection as harness execute(): ties go to Up/Yes.
            let edge_yes = ev.p - up_ask.price;
            let edge_no = (1.0 - ev.p) - down_ask.price;
            let (side, edge, touch) = if edge_yes >= edge_no {
                (Side::Up, edge_yes, up_ask)
            } else {
                (Side::Down, edge_no, down_ask)
            };
            if edge < self.cfg.edge_threshold {
                continue;
            }

            m.entered = true;
            self.stats.entries_total += 1;
            out.push(LogEvent::WouldEnter {
                ts_utc: ts_utc(now_ns),
                slug: m.slug.clone(),
                side: side.as_str(),
                p_exo: ev.p,
                touch_price: touch.price,
                touch_size: touch.size,
                edge,
                strike,
                strike_source,
                sigma_bar_bps: ev.raw.sigma_bar_bps,
            });
            entries.push(PendingTrade {
                slug: m.slug.clone(),
                side,
                token: match side {
                    Side::Up => m.up_token.clone(),
                    Side::Down => m.down_token.clone(),
                },
                entry_touch_price: touch.price,
                probe_due_ns: now_ns + self.cfg.latency_probe_ms as i64 * 1_000_000,
                probe_done: false,
                // Harness exits require a tick at or before close; clamp.
                exit_due_ns: (now_ns + self.cfg.exit_after_s as i64 * 1_000_000_000)
                    .min(close_ns),
                exit_done: false,
            });
        }
        self.pending.extend(entries);
        out
    }

    /// Emit due QUOTE_PROBE / WOULD_EXIT records. Call at fine cadence
    /// (10-50ms) so the probe lands close to entry + latency_probe_ms.
    pub fn poll_due(&mut self, now_ns: i64) -> Vec<LogEvent> {
        let mut out = Vec::new();
        for p in &mut self.pending {
            if !p.probe_done && now_ns >= p.probe_due_ns {
                p.probe_done = true;
                let ladder = self.books.get(&p.token);
                let current_touch = ladder.and_then(Ladder::best_ask);
                let remaining_size = ladder
                    .map(|l| l.ask_size_at_or_below(p.entry_touch_price))
                    .unwrap_or(0.0);
                let still_quoted = remaining_size > 0.0;
                self.stats.probes_total += 1;
                if still_quoted {
                    self.stats.probes_quoted += 1;
                }
                out.push(LogEvent::QuoteProbe {
                    ts_utc: ts_utc(now_ns),
                    slug: p.slug.clone(),
                    side: p.side.as_str(),
                    entry_touch_price: p.entry_touch_price,
                    still_quoted,
                    current_touch,
                    remaining_size,
                });
            }
            if !p.exit_done && now_ns >= p.exit_due_ns {
                p.exit_done = true;
                let exit_touch = self.books.get(&p.token).and_then(Ladder::best_bid);
                let mark_pnl_per_share = exit_touch.map(|t| t.price - p.entry_touch_price);
                if let Some(pnl) = mark_pnl_per_share {
                    self.stats.mark_pnl_sum += pnl;
                    self.stats.mark_pnl_count += 1;
                }
                out.push(LogEvent::WouldExit {
                    ts_utc: ts_utc(now_ns),
                    slug: p.slug.clone(),
                    side: p.side.as_str(),
                    entry_touch_price: p.entry_touch_price,
                    exit_touch,
                    mark_pnl_per_share,
                });
            }
        }
        self.pending.retain(|p| !(p.probe_done && p.exit_done));
        out
    }

    pub fn summary(&self, now_ns: i64, now_local_ms: i64) -> LogEvent {
        let now_s = now_ns / 1_000_000_000;
        let n_active_markets = self
            .markets
            .values()
            .filter(|m| m.open_ts_s <= now_s && now_s < m.close_ts_s)
            .count();
        let s = &self.stats;
        LogEvent::Summary {
            ts_utc: ts_utc(now_ns),
            n_active_markets,
            n_entries_total: s.entries_total,
            probe_still_quoted_rate: (s.probes_total > 0)
                .then(|| s.probes_quoted as f64 / s.probes_total as f64),
            mean_mark_pnl_per_share: (s.mark_pnl_count > 0)
                .then(|| s.mark_pnl_sum / s.mark_pnl_count as f64),
            binance_feed_age_ms: self.spot_last_receipt_ms.map(|t| now_local_ms - t),
            book_feed_age_ms: self.book_last_receipt_ms.map(|t| now_local_ms - t),
            median_binance_receipt_minus_exchange_ms: median(&self.spot_receipt_deltas_ms),
            median_book_receipt_minus_exchange_ms: median(&self.book_receipt_deltas_ms),
        }
    }
}

/// JSONL sink: one line per record, flushed on every write (records are
/// infrequent; durability beats throughput here).
struct Logger {
    file: std::io::BufWriter<std::fs::File>,
}

impl Logger {
    fn create(out_dir: &std::path::Path) -> Result<(Self, PathBuf)> {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("creating out dir {}", out_dir.display()))?;
        let name = format!(
            "shadow-{}.jsonl",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        );
        let path = out_dir.join(name);
        let file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("creating shadow log {}", path.display()))?;
        Ok((
            Self {
                file: std::io::BufWriter::new(file),
            },
            path,
        ))
    }

    fn write(&mut self, event: &LogEvent) -> Result<()> {
        use std::io::Write;
        serde_json::to_writer(&mut self.file, event)?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

fn now_unix_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

fn now_unix_ms() -> i64 {
    now_unix_ns() / 1_000_000
}

pub async fn run_shadow(args: ShadowArgs) -> Result<()> {
    let (mut logger, log_path) = Logger::create(&args.out_dir)?;
    tracing::info!(log = %log_path.display(), "shadow mode: LOG ONLY, zero orders");

    let core = std::sync::Arc::new(std::sync::Mutex::new(ShadowCore::new(ShadowConfig {
        edge_threshold: args.edge_threshold,
        vol_lookback_s: args.vol_lookback_s,
        exit_after_s: args.exit_after_s,
        latency_probe_ms: args.latency_probe_ms,
    })));

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let (assets_tx, assets_rx) = tokio::sync::watch::channel(Vec::<String>::new());

    let spot_task = tokio::spawn(feeds::binance_spot_feed(core.clone(), shutdown_rx.clone()));
    let discovery_task = tokio::spawn(feeds::gamma_discovery_feed(
        core.clone(),
        args.slug_prefix.clone(),
        assets_tx,
        shutdown_rx.clone(),
    ));
    let book_task = tokio::spawn(feeds::polymarket_book_feed(
        core.clone(),
        assets_rx,
        shutdown_rx.clone(),
    ));

    // 10ms poll keeps the latency probe honest (~±10ms of the target);
    // decisions run on the harness's 1s cadence; summaries every 60s.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut next_decide_ns = 0i64;
    let mut next_summary_ns = now_unix_ns() + 60_000_000_000;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT: flushing shadow log and exiting");
                break;
            }
            _ = tick.tick() => {
                let now_ns = now_unix_ns();
                let mut events = Vec::new();
                {
                    let mut core = core.lock().expect("shadow core poisoned");
                    events.extend(core.poll_due(now_ns));
                    if now_ns >= next_decide_ns {
                        next_decide_ns = now_ns + 1_000_000_000;
                        core.prune(now_ns);
                        events.extend(core.decide(now_ns));
                    }
                    if now_ns >= next_summary_ns {
                        next_summary_ns = now_ns + 60_000_000_000;
                        events.push(core.summary(now_ns, now_unix_ms()));
                    }
                }
                for event in &events {
                    tracing::info!(event = %serde_json::to_string(event).unwrap_or_default(), "shadow");
                    logger.write(event)?;
                }
            }
        }
    }

    // Final summary so a short run still leaves a measurement on disk.
    {
        let core = core.lock().expect("shadow core poisoned");
        logger.write(&core.summary(now_unix_ns(), now_unix_ms()))?;
    }
    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        futures::future::join_all([spot_task, discovery_task, book_task]),
    )
    .await;
    Ok(())
}

/// Live data feeds. Read-only consumers of public endpoints: the only
/// outbound payloads are websocket subscriptions and pings.
mod feeds {
    use super::{MarketWindow, ShadowCore, now_unix_ms};
    use anyhow::{Context, Result};
    use futures::{SinkExt, StreamExt};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::watch;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@aggTrade";
    const PM_BOOK_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    const GAMMA_MARKETS_URL: &str = "https://gamma-api.polymarket.com/markets";
    const DISCOVERY_INTERVAL: Duration = Duration::from_secs(20);
    const STALE_TIMEOUT: Duration = Duration::from_secs(45);
    const MAX_BACKOFF: Duration = Duration::from_secs(30);

    type Core = Arc<Mutex<ShadowCore>>;

    async fn backoff_sleep(backoff: &mut Duration) {
        tokio::time::sleep(*backoff).await;
        *backoff = (*backoff * 2).min(MAX_BACKOFF);
    }

    fn value_f64(v: Option<&Value>) -> Option<f64> {
        match v? {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    fn value_i64(v: Option<&Value>) -> Option<i64> {
        match v? {
            Value::Number(n) => n.as_i64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    // Binance spot

    pub async fn binance_spot_feed(core: Core, mut shutdown: watch::Receiver<bool>) {
        let mut backoff = Duration::from_secs(1);
        while !*shutdown.borrow() {
            match binance_once(&core, &mut shutdown).await {
                Ok(()) => break,
                Err(error) => {
                    tracing::warn!(?error, backoff_ms = backoff.as_millis() as u64,
                        "binance spot feed failed; reconnecting");
                    backoff_sleep(&mut backoff).await;
                }
            }
        }
    }

    async fn binance_once(core: &Core, shutdown: &mut watch::Receiver<bool>) -> Result<()> {
        let (stream, _) = connect_async(BINANCE_WS_URL)
            .await
            .context("connecting binance spot ws")?;
        tracing::info!("binance spot websocket connected");
        let (mut write, mut read) = stream.split();
        let mut pings = tokio::time::interval(Duration::from_secs(15));
        pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_frame = tokio::time::Instant::now();

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(());
                }
                _ = pings.tick() => {
                    if last_frame.elapsed() > STALE_TIMEOUT {
                        anyhow::bail!("binance spot ws stale (no frames)");
                    }
                    write.send(Message::Ping(Vec::new().into())).await
                        .context("binance ping")?;
                }
                frame = read.next() => {
                    last_frame = tokio::time::Instant::now();
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            // One malformed message must never kill the loop.
                            if let Err(error) = handle_binance_text(core, &text) {
                                tracing::warn!(?error, "skipping malformed binance message");
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            write.send(Message::Pong(payload)).await.ok();
                        }
                        Some(Ok(Message::Close(_))) => anyhow::bail!("binance ws closed by remote"),
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Err(error).context("binance ws frame error"),
                        None => anyhow::bail!("binance ws stream ended"),
                    }
                }
            }
        }
    }

    fn handle_binance_text(core: &Core, text: &str) -> Result<()> {
        let payload: Value = serde_json::from_str(text).context("decode binance payload")?;
        if payload.get("e").and_then(Value::as_str) != Some("aggTrade") {
            return Ok(());
        }
        let (Some(price), Some(qty)) = (
            value_f64(payload.get("p")),
            value_f64(payload.get("q")),
        ) else {
            anyhow::bail!("aggTrade missing price/quantity");
        };
        // Exchange event time: the stream's T field (trade time, ms).
        let exchange_ms = value_i64(payload.get("T"))
            .or_else(|| value_i64(payload.get("E")))
            .context("aggTrade missing T/E timestamp")?;
        let is_buyer_maker = payload.get("m").and_then(Value::as_bool).unwrap_or(false);
        core.lock()
            .expect("shadow core poisoned")
            .push_spot(exchange_ms, now_unix_ms(), price, qty, is_buyer_maker);
        Ok(())
    }

    // Gamma market discovery

    /// Poll Gamma every ~20s for the current and next 5m windows; push
    /// discovered markets into the core and publish the token subscription
    /// set for the book feed.
    pub async fn gamma_discovery_feed(
        core: Core,
        slug_prefix: String,
        assets_tx: watch::Sender<Vec<String>>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let client = reqwest::Client::new();
        let mut ticker = tokio::time::interval(DISCOVERY_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        while !*shutdown.borrow() {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = ticker.tick() => {
                    let now_s = now_unix_ms() / 1_000;
                    let current_open = now_s - now_s.rem_euclid(300);
                    for open_ts in [current_open, current_open + 300] {
                        let slug = format!("{slug_prefix}{open_ts}");
                        match fetch_gamma_market(&client, &slug).await {
                            Ok(Some(market)) => {
                                core.lock().expect("shadow core poisoned").upsert_market(market);
                            }
                            Ok(None) => {
                                tracing::debug!(slug, "gamma returned no market yet");
                            }
                            Err(error) => {
                                tracing::warn!(?error, slug, "gamma discovery fetch failed");
                            }
                        }
                    }
                    let tokens = core.lock().expect("shadow core poisoned").subscribed_tokens();
                    if *assets_tx.borrow() != tokens {
                        let _ = assets_tx.send(tokens);
                    }
                }
            }
        }
    }

    async fn fetch_gamma_market(
        client: &reqwest::Client,
        slug: &str,
    ) -> Result<Option<MarketWindow>> {
        let payload = client
            .get(GAMMA_MARKETS_URL)
            .query(&[("slug", slug)])
            .header("User-Agent", "pm-app-shadow/1.0")
            .header("Accept", "application/json")
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        let Some(items) = payload.as_array() else {
            return Ok(None);
        };
        Ok(items.iter().find_map(|item| parse_gamma_market(item, slug)))
    }

    /// Parse one Gamma market row: `clobTokenIds` is a JSON-encoded array of
    /// the two token ids, ordered to match `outcomes` (["Up","Down"]).
    /// `openPrice`/`priceToBeat` is the TRUE strike; it may only appear
    /// shortly after the window opens.
    pub(super) fn parse_gamma_market(item: &Value, slug: &str) -> Option<MarketWindow> {
        if item.get("slug").and_then(Value::as_str) != Some(slug) {
            return None;
        }
        let open_ts_s: i64 = slug.rsplit('-').next()?.parse().ok()?;
        let tokens = parse_json_string_list(item.get("clobTokenIds"));
        let outcomes = parse_json_string_list(item.get("outcomes"));
        let (up_token, down_token) = order_up_down(&tokens, &outcomes)?;
        let gamma_strike = ["openPrice", "open_price", "priceToBeat", "price_to_beat"]
            .iter()
            .find_map(|k| value_f64(item.get(*k)))
            .filter(|s| s.is_finite() && *s > 0.0);
        Some(MarketWindow {
            slug: slug.to_string(),
            open_ts_s,
            close_ts_s: open_ts_s + 300,
            up_token,
            down_token,
            gamma_strike,
            entered: false,
        })
    }

    /// Gamma encodes lists as JSON strings ("[\"a\",\"b\"]") or real arrays.
    fn parse_json_string_list(value: Option<&Value>) -> Vec<String> {
        match value {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            Some(Value::String(raw)) => serde_json::from_str::<Value>(raw)
                .ok()
                .map(|v| parse_json_string_list(Some(&v)))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn order_up_down(tokens: &[String], outcomes: &[String]) -> Option<(String, String)> {
        if tokens.len() < 2 {
            return None;
        }
        if outcomes.len() == tokens.len() {
            let mut up = None;
            let mut down = None;
            for (token, outcome) in tokens.iter().zip(outcomes) {
                match outcome.trim().to_ascii_lowercase().as_str() {
                    "up" | "yes" => up = Some(token.clone()),
                    "down" | "no" => down = Some(token.clone()),
                    _ => {}
                }
            }
            if let (Some(up), Some(down)) = (up, down) {
                return Some((up, down));
            }
        }
        // Gamma convention for these families: first listed outcome is Up.
        Some((tokens[0].clone(), tokens[1].clone()))
    }

    // Polymarket book websocket

    /// Maintain top-of-book ladders for all subscribed tokens. Reconnects
    /// (with backoff) on errors and whenever the subscription set changes.
    pub async fn polymarket_book_feed(
        core: Core,
        mut assets_rx: watch::Receiver<Vec<String>>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut backoff = Duration::from_secs(1);
        while !*shutdown.borrow() {
            let assets = assets_rx.borrow_and_update().clone();
            if assets.is_empty() {
                tokio::select! {
                    _ = shutdown.changed() => break,
                    changed = assets_rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                    }
                }
                continue;
            }
            match book_once(&core, &assets, &mut assets_rx, &mut shutdown).await {
                Ok(BookExit::Shutdown) => break,
                Ok(BookExit::AssetsChanged) => {
                    backoff = Duration::from_secs(1);
                    tracing::info!("book subscription set changed; resubscribing");
                }
                Err(error) => {
                    tracing::warn!(?error, backoff_ms = backoff.as_millis() as u64,
                        "polymarket book feed failed; reconnecting");
                    backoff_sleep(&mut backoff).await;
                }
            }
        }
    }

    enum BookExit {
        Shutdown,
        AssetsChanged,
    }

    async fn book_once(
        core: &Core,
        assets: &[String],
        assets_rx: &mut watch::Receiver<Vec<String>>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<BookExit> {
        let (stream, _) = connect_async(PM_BOOK_WS_URL)
            .await
            .context("connecting polymarket book ws")?;
        tracing::info!(n_assets = assets.len(), "polymarket book websocket connected");
        let (mut write, mut read) = stream.split();
        let subscribe = serde_json::json!({
            "assets_ids": assets,
            "type": "market",
        });
        write
            .send(Message::Text(subscribe.to_string().into()))
            .await
            .context("subscribing polymarket book ws")?;

        let mut pings = tokio::time::interval(Duration::from_secs(10));
        pings.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_frame = tokio::time::Instant::now();

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(BookExit::Shutdown);
                }
                _ = assets_rx.changed() => {
                    let _ = write.send(Message::Close(None)).await;
                    return Ok(BookExit::AssetsChanged);
                }
                _ = pings.tick() => {
                    if last_frame.elapsed() > STALE_TIMEOUT {
                        anyhow::bail!("polymarket book ws stale (no frames)");
                    }
                    write.send(Message::Text("PING".to_string().into())).await
                        .context("polymarket book ping")?;
                }
                frame = read.next() => {
                    last_frame = tokio::time::Instant::now();
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            // One malformed message must never kill the loop.
                            if let Err(error) = handle_book_text(core, &text) {
                                tracing::warn!(?error, "skipping malformed book message");
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            write.send(Message::Pong(payload)).await.ok();
                        }
                        Some(Ok(Message::Close(_))) => anyhow::bail!("book ws closed by remote"),
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Err(error).context("book ws frame error"),
                        None => anyhow::bail!("book ws stream ended"),
                    }
                }
            }
        }
    }

    fn handle_book_text(core: &Core, text: &str) -> Result<()> {
        if text == "PONG" {
            return Ok(());
        }
        let receipt_ms = now_unix_ms();
        let payload: Value = serde_json::from_str(text).context("decode book payload")?;
        match payload {
            Value::Array(items) => {
                for item in items {
                    handle_book_event(core, &item, receipt_ms);
                }
            }
            Value::Object(_) => handle_book_event(core, &payload, receipt_ms),
            _ => {}
        }
        Ok(())
    }

    fn handle_book_event(core: &Core, event: &Value, receipt_ms: i64) {
        let event_type = event
            .get("event_type")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                if event.get("bids").is_some() || event.get("asks").is_some() {
                    "book"
                } else {
                    "unknown"
                }
            });
        // The event's own exchange timestamp (ms), distinct from receipt.
        let exchange_ms = value_i64(event.get("timestamp"))
            .or_else(|| value_i64(event.get("t")))
            .or_else(|| value_i64(event.get("ts")));
        match event_type {
            "book" => {
                let Some(asset_id) = event.get("asset_id").and_then(Value::as_str) else {
                    return;
                };
                let bids = parse_levels(event.get("bids"));
                let asks = parse_levels(event.get("asks"));
                core.lock()
                    .expect("shadow core poisoned")
                    .apply_book_snapshot(asset_id, &bids, &asks, exchange_ms, receipt_ms);
            }
            "price_change" => {
                let changes = event
                    .get("price_changes")
                    .or_else(|| event.get("pc"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let mut core = core.lock().expect("shadow core poisoned");
                for change in &changes {
                    let Some(asset_id) = change
                        .get("asset_id")
                        .or_else(|| change.get("a"))
                        .and_then(Value::as_str)
                    else {
                        continue;
                    };
                    let (Some(price), Some(size)) = (
                        value_f64(change.get("price")),
                        value_f64(change.get("size")),
                    ) else {
                        continue;
                    };
                    let Some(side) = change.get("side").and_then(Value::as_str) else {
                        continue;
                    };
                    let is_buy = side.eq_ignore_ascii_case("buy");
                    let change_exchange_ms = value_i64(change.get("timestamp"))
                        .or_else(|| value_i64(change.get("t")))
                        .or(exchange_ms);
                    core.apply_price_change(
                        asset_id,
                        is_buy,
                        price,
                        size,
                        change_exchange_ms,
                        receipt_ms,
                    );
                }
            }
            _ => {}
        }
    }

    fn parse_levels(value: Option<&Value>) -> Vec<(f64, f64)> {
        let Some(levels) = value.and_then(Value::as_array) else {
            return Vec::new();
        };
        levels
            .iter()
            .filter_map(|level| {
                Some((
                    value_f64(level.get("price"))?,
                    value_f64(level.get("size"))?,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: i64 = 1_000_000_000;

    fn cfg() -> ShadowConfig {
        ShadowConfig {
            edge_threshold: 0.16,
            vol_lookback_s: 1800,
            exit_after_s: 30,
            latency_probe_ms: 150,
        }
    }

    /// Wavy spot tape: 0..2000s around 100k, enough history for the vol
    /// estimator. Receipt is exchange + 25ms so the median delta is known.
    fn core_with_spot() -> ShadowCore {
        let mut core = ShadowCore::new(cfg());
        let mut price = 100_000.0;
        for s in 0..2000i64 {
            core.push_spot(s * 1_000, s * 1_000 + 25, price, 1.0, false);
            price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
        }
        core
    }

    fn market(strike: Option<f64>) -> MarketWindow {
        MarketWindow {
            slug: "btc-updown-5m-1800".to_string(),
            open_ts_s: 1800,
            close_ts_s: 2100,
            up_token: "up-tok".to_string(),
            down_token: "down-tok".to_string(),
            gamma_strike: strike,
            entered: false,
        }
    }

    fn set_books(core: &mut ShadowCore, up_ask: f64, down_ask: f64) {
        core.apply_book_snapshot(
            "up-tok",
            &[(1.0 - up_ask - 0.02, 100.0)],
            &[(up_ask, 50.0), (up_ask + 0.02, 80.0)],
            Some(1_899_000),
            1_899_040,
        );
        core.apply_book_snapshot(
            "down-tok",
            &[(1.0 - down_ask - 0.02, 60.0)],
            &[(down_ask, 70.0)],
            Some(1_899_000),
            1_899_040,
        );
    }

    #[test]
    fn ladder_tracks_snapshots_and_price_changes() {
        let mut core = ShadowCore::new(cfg());
        core.apply_book_snapshot(
            "tok",
            &[(0.40, 10.0), (0.42, 5.0)],
            &[(0.50, 7.0), (0.55, 9.0)],
            Some(1_000),
            1_030,
        );
        let ladder = core.books.get("tok").unwrap();
        assert_eq!(ladder.best_bid().unwrap().price, 0.42);
        assert_eq!(ladder.best_ask().unwrap(), Touch { price: 0.50, size: 7.0 });

        // price_change: improve the ask, then remove it again.
        core.apply_price_change("tok", false, 0.48, 3.0, Some(1_100), 1_120);
        assert_eq!(
            core.books.get("tok").unwrap().best_ask().unwrap(),
            Touch { price: 0.48, size: 3.0 }
        );
        core.apply_price_change("tok", false, 0.48, 0.0, Some(1_200), 1_210);
        assert_eq!(core.books.get("tok").unwrap().best_ask().unwrap().price, 0.50);
        // BUY side updates bids.
        core.apply_price_change("tok", true, 0.45, 4.0, Some(1_300), 1_315);
        assert_eq!(core.books.get("tok").unwrap().best_bid().unwrap().price, 0.45);
        // Size available at-or-below a limit.
        let ladder = core.books.get("tok").unwrap();
        assert_eq!(ladder.ask_size_at_or_below(0.50), 7.0);
        assert_eq!(ladder.ask_size_at_or_below(0.56), 16.0);
        assert_eq!(ladder.ask_size_at_or_below(0.40), 0.0);
    }

    #[test]
    fn enters_up_side_once_on_threshold_crossing() {
        let mut core = core_with_spot();
        core.upsert_market(market(Some(99_000.0))); // strike far below: p_up ~ 1
        set_books(&mut core, 0.50, 0.50);

        let events = core.decide(1900 * NS);
        assert_eq!(events.len(), 1);
        match &events[0] {
            LogEvent::WouldEnter { side, edge, strike, strike_source, touch_price, touch_size, p_exo, .. } => {
                assert_eq!(*side, "up");
                assert_eq!(*strike, 99_000.0);
                assert_eq!(*strike_source, "gamma");
                assert_eq!(*touch_price, 0.50);
                assert_eq!(*touch_size, 50.0);
                assert!(*p_exo > 0.9, "p_exo={p_exo}");
                assert!(*edge > 0.16);
            }
            other => panic!("expected WouldEnter, got {other:?}"),
        }
        // First crossing only: no duplicate entry on later passes.
        assert!(core.decide(1901 * NS).is_empty());
        assert_eq!(core.stats.entries_total, 1);
    }

    #[test]
    fn enters_down_side_using_real_down_book() {
        let mut core = core_with_spot();
        core.upsert_market(market(Some(101_000.0))); // strike far above: p_up ~ 0
        set_books(&mut core, 0.50, 0.50);

        let events = core.decide(1900 * NS);
        assert_eq!(events.len(), 1);
        match &events[0] {
            LogEvent::WouldEnter { side, p_exo, .. } => {
                assert_eq!(*side, "down");
                assert!(*p_exo < 0.1, "p_exo={p_exo}");
            }
            other => panic!("expected WouldEnter, got {other:?}"),
        }
    }

    #[test]
    fn no_entry_without_strike_and_proxy_used_when_spot_covers_open() {
        // Spot history starting AFTER the open: no proxy strike available.
        let mut late = ShadowCore::new(cfg());
        let mut price = 100_000.0;
        for s in 1850..3900i64 {
            late.push_spot(s * 1_000, s * 1_000 + 25, price, 1.0, false);
            price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
        }
        late.upsert_market(market(None));
        set_books(&mut late, 0.10, 0.95);
        assert!(late.decide(3000 * NS).is_empty(), "no strike -> stand down");

        // Full history: proxy = last Binance trade at-or-before open.
        let mut core = core_with_spot();
        core.upsert_market(market(None));
        set_books(&mut core, 0.10, 0.95); // cheap up ask so the fade fires
        let events = core.decide(1900 * NS);
        assert_eq!(events.len(), 1);
        match &events[0] {
            LogEvent::WouldEnter { strike_source, strike, .. } => {
                assert_eq!(*strike_source, "binance_proxy");
                assert!((*strike - 100_000.0).abs() / 100_000.0 < 0.01);
            }
            other => panic!("expected WouldEnter, got {other:?}"),
        }
    }

    #[test]
    fn probe_and_exit_bookkeeping() {
        let mut core = core_with_spot();
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        let entry_ns = 1900 * NS;
        assert_eq!(core.decide(entry_ns).len(), 1);

        // Nothing due before the probe time.
        assert!(core.poll_due(entry_ns + 100_000_000).is_empty());

        // Quote pulled before the probe: ask worsens past our entry touch.
        core.apply_book_snapshot(
            "up-tok",
            &[(0.55, 20.0)],
            &[(0.60, 10.0)],
            Some(1_900_100),
            1_900_120,
        );
        let probe = core.poll_due(entry_ns + 150_000_000);
        assert_eq!(probe.len(), 1);
        match &probe[0] {
            LogEvent::QuoteProbe { still_quoted, current_touch, remaining_size, .. } => {
                assert!(!*still_quoted);
                assert_eq!(current_touch.unwrap().price, 0.60);
                assert_eq!(*remaining_size, 0.0);
            }
            other => panic!("expected QuoteProbe, got {other:?}"),
        }

        // Exit marks against the side bid 30s after entry.
        let exit = core.poll_due(entry_ns + 30 * NS);
        assert_eq!(exit.len(), 1);
        match &exit[0] {
            LogEvent::WouldExit { exit_touch, mark_pnl_per_share, .. } => {
                assert_eq!(exit_touch.unwrap().price, 0.55);
                assert!((mark_pnl_per_share.unwrap() - 0.05).abs() < 1e-12);
            }
            other => panic!("expected WouldExit, got {other:?}"),
        }
        assert!(core.pending.is_empty(), "completed trades are dropped");

        match core.summary(entry_ns + 31 * NS, 1_931_000) {
            LogEvent::Summary {
                n_active_markets,
                n_entries_total,
                probe_still_quoted_rate,
                mean_mark_pnl_per_share,
                median_binance_receipt_minus_exchange_ms,
                ..
            } => {
                assert_eq!(n_active_markets, 1);
                assert_eq!(n_entries_total, 1);
                assert_eq!(probe_still_quoted_rate, Some(0.0));
                assert!((mean_mark_pnl_per_share.unwrap() - 0.05).abs() < 1e-12);
                assert_eq!(median_binance_receipt_minus_exchange_ms, Some(25));
            }
            other => panic!("expected Summary, got {other:?}"),
        }
    }

    #[test]
    fn probe_still_quoted_when_same_or_better_price_remains() {
        let mut core = core_with_spot();
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        let entry_ns = 1900 * NS;
        assert_eq!(core.decide(entry_ns).len(), 1);
        // Better price appears: still quoted, remaining size counts <= 0.50.
        core.apply_price_change("up-tok", false, 0.49, 12.0, Some(1_900_050), 1_900_060);
        let probe = core.poll_due(entry_ns + 150_000_000);
        match &probe[0] {
            LogEvent::QuoteProbe { still_quoted, current_touch, remaining_size, .. } => {
                assert!(*still_quoted);
                assert_eq!(current_touch.unwrap().price, 0.49);
                assert_eq!(*remaining_size, 62.0); // 12 @ 0.49 + 50 @ 0.50
            }
            other => panic!("expected QuoteProbe, got {other:?}"),
        }
    }

    #[test]
    fn exit_is_clamped_to_market_close() {
        let mut core = core_with_spot();
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        // Entry 15s before close: exit_after_s=30 would land past close.
        let entry_ns = 2085 * NS;
        assert_eq!(core.decide(entry_ns).len(), 1);
        assert!(core.poll_due(2099 * NS).iter().all(|e| matches!(e, LogEvent::QuoteProbe { .. })));
        let exit = core.poll_due(2100 * NS);
        assert_eq!(exit.len(), 1);
        assert!(matches!(exit[0], LogEvent::WouldExit { .. }));
    }

    #[test]
    fn no_entry_inside_stop_before_close_window() {
        let mut core = core_with_spot();
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        assert!(core.decide(2095 * NS).is_empty(), "deadline is close - 10s");
        assert!(core.decide(1700 * NS).is_empty(), "not open yet");
    }

    #[test]
    fn upsert_refreshes_strike_but_keeps_entered_flag() {
        let mut core = core_with_spot();
        core.upsert_market(market(None));
        set_books(&mut core, 0.10, 0.95);
        assert_eq!(core.decide(1900 * NS).len(), 1);
        // Re-discovery now carries the true strike; entered must survive.
        core.upsert_market(market(Some(100_123.0)));
        let m = core.markets.get("btc-updown-5m-1800").unwrap();
        assert!(m.entered);
        assert_eq!(m.gamma_strike, Some(100_123.0));
        assert!(core.decide(1901 * NS).is_empty());
    }

    #[test]
    fn parse_gamma_market_orders_tokens_and_reads_strike() {
        let item = serde_json::json!({
            "slug": "btc-updown-5m-1777750200",
            "outcomes": "[\"Down\", \"Up\"]",
            "clobTokenIds": "[\"down-token\", \"up-token\"]",
            "openPrice": "104250.5"
        });
        let m = feeds::parse_gamma_market(&item, "btc-updown-5m-1777750200").unwrap();
        assert_eq!(m.up_token, "up-token");
        assert_eq!(m.down_token, "down-token");
        assert_eq!(m.gamma_strike, Some(104_250.5));
        assert_eq!(m.open_ts_s, 1_777_750_200);
        assert_eq!(m.close_ts_s, 1_777_750_500);

        let no_strike = serde_json::json!({
            "slug": "btc-updown-5m-1777750200",
            "outcomes": ["Up", "Down"],
            "clobTokenIds": ["u", "d"],
        });
        let m = feeds::parse_gamma_market(&no_strike, "btc-updown-5m-1777750200").unwrap();
        assert_eq!((m.up_token.as_str(), m.down_token.as_str()), ("u", "d"));
        assert_eq!(m.gamma_strike, None);

        assert!(feeds::parse_gamma_market(&item, "btc-updown-5m-1").is_none());
    }

    #[test]
    fn summary_with_no_data_is_all_none() {
        let core = ShadowCore::new(cfg());
        match core.summary(0, 0) {
            LogEvent::Summary {
                probe_still_quoted_rate,
                mean_mark_pnl_per_share,
                binance_feed_age_ms,
                book_feed_age_ms,
                median_binance_receipt_minus_exchange_ms,
                median_book_receipt_minus_exchange_ms,
                ..
            } => {
                assert_eq!(probe_still_quoted_rate, None);
                assert_eq!(mean_mark_pnl_per_share, None);
                assert_eq!(binance_feed_age_ms, None);
                assert_eq!(book_feed_age_ms, None);
                assert_eq!(median_binance_receipt_minus_exchange_ms, None);
                assert_eq!(median_book_receipt_minus_exchange_ms, None);
            }
            other => panic!("expected Summary, got {other:?}"),
        }
    }

    #[test]
    fn spot_buffer_prunes_beyond_two_hours() {
        let mut core = ShadowCore::new(cfg());
        for s in 0..9000i64 {
            core.push_spot(s * 1_000, s * 1_000 + 5, 100_000.0, 1.0, false);
        }
        let history = core.spot_history();
        let first = history.samples().first().unwrap().ts_ns / NS;
        assert!(first >= 8999 - SPOT_KEEP_SECS && first > 0);
    }
}
