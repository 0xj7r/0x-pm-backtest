//! `shadow` subcommand: run a `pm_strategy::Strategy` LIVE in LOG-ONLY mode.
//!
//! Zero orders, zero capital. This module contains NO order-placement code by
//! construction: it has no signer, no CLOB REST client, and never sends
//! anything on a websocket except subscriptions and pings.
//!
//! The engine is strategy-agnostic. It owns the feeds, the rolling books, the
//! belief model, the entry/probe/exit/resolution bookkeeping and the JSONL
//! stream; the strategy owns the decision. Each decide pass hands every live
//! market's book to `Strategy::on_event` as a `ReplayEvent` (the same type the
//! backtest replays) and turns any returned buy order into a WOULD_ENTER,
//! after which the engine's QUOTE_PROBE / WOULD_EXIT / RESOLUTION machinery
//! measures what that entry would have realized.
//!
//! With `NoopStrategy` (the only shipped strategy) the stream is well-formed
//! and entry-free: config, summaries and venue lead-lag records only.

mod tape;
pub use tape::{BookLevel, TapeEvent, TapeStore};

use anyhow::{Context, Result};
use pm_alpha::{
    calibrator::{exo_features, EXO_FEATURES},
    dir_features, harness::spot_ret_bps, regime, AlphaModel, AlphaModelConfig, ExoState,
    MarketMeta, PerpState, Token, VolEstimator, DIR_FEATURES,
};
use pm_strategy::{
    regime::WhipsawRiskSnapshot, Ctx, NoopStrategy, Side as StratSide, Strategy,
};
use pm_types::{MarketId, ReplayEvent, ReplayFlags, SpotHistory, SpotTick, TradeHistory};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;

/// Spot ticks retained in the rolling buffer (>= 2h required for vol3600).
const SPOT_KEEP_SECS: i64 = 7_800;
/// Engine entry deadline: the twin never opens inside the last 90s of a
/// window, so every logged entry has room for its probe and exit measurement.
const STOP_BEFORE_CLOSE_S: i64 = 90;
/// Passive-exit probe: how long the measure-only resting ask waits for a
/// crossing bid before converting against the book (matches the harness
/// midtimeout variant).
const PASSIVE_EXIT_TIMEOUT_S: i64 = 60;
/// Rolling cap on receipt-minus-exchange latency samples.
const LATENCY_SAMPLE_CAP: usize = 4_096;
/// OI poll cadence (Binance metrics are 5-minute).
const OI_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);
/// Funding events are 8h; poll often enough to catch the next print.
const FUNDING_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1800);
/// ~50h of 5-minute OI rows (enough for oi_delta_30m + warmup).
const OI_SERIES_CAP: usize = 600;
/// ~40 days of 8h funding events.
const FUNDING_SERIES_CAP: usize = 120;

#[derive(Debug, Clone, PartialEq)]
pub struct ShadowArgs {
    pub slug_prefix: String,
    /// Strategy to drive the twin (see [`strategy_from_name`]). Only `noop`
    /// ships, so the default stream is entry-free by construction.
    pub strategy: String,
    pub vol_lookback_s: u32,
    pub exit_after_s: u32,
    pub latency_probe_ms: u64,
    pub out_dir: PathBuf,
    /// Weight on the basis-adjusted perp last in the effective-spot blend
    /// (0 disables the futures feed entirely).
    pub perp_price_weight: f64,
    /// Vol estimator: "realized" (rolling) or "ewma".
    pub vol_estimator: String,
    /// EWMA half-life seconds (only used when vol_estimator == "ewma").
    pub ewma_halflife_s: f64,
    /// Decision evaluation cadence in ms (default 1000 = harness-matched;
    /// 100 = fast mode). Does not change decision logic, only when it runs.
    pub decide_interval_ms: u64,
    /// Evaluate decisions on the next poll tick after any input event
    /// (spot/perp/book/oi/funding), floored by 20ms spacing;
    /// `decide_interval_ms` becomes the fallback heartbeat. Default off.
    pub decide_on_event: bool,
}

/// Engine defaults for the live twin.
///
/// Everything here is infrastructure: which markets to watch, which belief
/// model to run, how often to decide, how the probes are timed. There is no
/// entry threshold, no gate and no sizing rule, because those belong to the
/// strategy. The pm-app clap defaults are pinned to this function so the CLI
/// and the library cannot drift.
pub fn default_shadow_args(out_dir: PathBuf) -> ShadowArgs {
    ShadowArgs {
        slug_prefix: "btc-updown-5m-".to_string(),
        strategy: "noop".to_string(),
        vol_lookback_s: 3600,
        exit_after_s: 0,
        latency_probe_ms: 150,
        out_dir,
        perp_price_weight: 0.75,
        vol_estimator: "realized".to_string(),
        ewma_halflife_s: 600.0,
        decide_interval_ms: 1000,
        decide_on_event: false,
    }
}

/// Strategies the live twin will drive, by name.
///
/// The fixture strategy is deliberately absent: it is golden-replay plumbing
/// that keys its one-shot latch off a replayed tape, and pointing it at live
/// feeds would produce entries that mean nothing. A live twin runs deployable
/// strategies only, and there are none yet.
pub fn strategy_from_name(name: &str) -> Result<Box<dyn Strategy + Send>> {
    match name {
        "noop" => Ok(Box::new(NoopStrategy)),
        other => anyhow::bail!(
            "unknown shadow strategy {other:?}; the only strategy the live twin \
             will drive is `noop` (no deployable strategy exists yet)"
        ),
    }
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
    /// Startup config: the FIRST event of every stream's JSONL, so a reader
    /// can derive the run's invocation from the stream itself and refuse to
    /// compare mismatched configs (deep-review F2).
    ///
    /// `config_fingerprint` covers the whole resolved [`ShadowConfig`],
    /// including the belief-model params that sit outside any gate: the
    /// fingerprint must cover the model, not just the decision layer.
    /// `strategy` names what produced the decisions; the strategy's own
    /// parameters are its to publish once a strategy has any.
    Config {
        ts_utc: String,
        slug_prefix: String,
        out_dir: String,
        strategy: String,
        decide_interval_ms: u64,
        decide_on_event: bool,
        perp_price_weight: f64,
        vol_lookback_s: u32,
        vol_estimator: String,
        config_fingerprint: String,
    },
    WouldEnter {
        ts_utc: String,
        slug: String,
        side: &'static str,
        /// Belief P(up) from the exogenous model (always, regardless of side).
        p_exo: f64,
        /// Belief on the entered side: `p_exo` for Up, `1 - p_exo` for Down.
        p_side: f64,
        /// Belief P(down) = `1 - p_exo` (decision-layer telemetry).
        p_down: f64,
        /// Edge if buying UP: `p_exo - yes_ask`.
        edge_up: f64,
        /// Edge if buying DOWN: `(1 - p_exo) - no_ask`.
        edge_down: f64,
        /// Trailing spot simple returns, bps (exogenous trend hints).
        #[serde(skip_serializing_if = "Option::is_none")]
        spot_ret_10s_bps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spot_ret_30s_bps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spot_ret_60s_bps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spot_ret_120s_bps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spot_ret_300s_bps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spot_ret_600s_bps: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        spot_ret_900s_bps: Option<f64>,
        /// Belief on entered side minus touch ask (book disagreement proxy).
        model_book_gap: f64,
        /// Seconds since window open at entry.
        secs_from_open: u32,
        /// Seconds the belief has held its current side (since last
        /// sign(p_exo - 0.5) flip, or first belief). Dwell-gate telemetry.
        #[serde(skip_serializing_if = "Option::is_none")]
        belief_dwell_s: Option<f64>,
        /// Spot vs strike in bps: `(S - K) / K * 10_000`.
        delta_bps: f64,
        /// `pm_alpha::calibrator::exo_features` vector (16); see `EXO_FEATURE_NAMES`.
        exo_features: Vec<f32>,
        /// `pm_alpha::directional::dir_features` vector (14); see `DIR_FEATURE_NAMES`.
        dir_features: Vec<f32>,
        /// CEX path-shape regime from `pm_alpha::regime` (not used in belief).
        #[serde(skip_serializing_if = "Option::is_none")]
        regime: Option<&'static str>,
        /// Side-oriented Binance spot flow (matches reversal discovery script).
        binance_flow_imbal_5s: f64,
        binance_flow_imbal_15s: f64,
        binance_flow_imbal_30s: f64,
        binance_adverse_vol_5s: f64,
        binance_adverse_vol_15s: f64,
        binance_adverse_vol_30s: f64,
        /// Perp-minus-spot basis change over 60s, bps (decision-layer only).
        basis_d60_bps: f64,
        touch_price: f64,
        touch_size: f64,
        edge: f64,
        /// Marketable IOC cap from `decide_entry`: `p_side - min_marginal_edge`.
        marketable_limit_price: f64,
        strike: f64,
        strike_source: &'static str,
        sigma_bar_bps: f64,
        /// "fade" (sell-side mirror of the harness) or "late_fav" (the
        /// hold-to-expiry favourite lane). Extra field; ingest tolerates it.
        lane: &'static str,
        /// 1-based entry index within the market (re-entry ladders only).
        clip: u32,
        /// Venue token to buy (so a JSONL-tailing executor needs no gamma lookup).
        token_id: String,
        /// Clip notional from frozen decide config.
        target_notional: f64,
        /// Market close (unix s) for redeem scheduling.
        close_ts_s: i64,
        condition_id: Option<String>,
        up_index_set: u64,
        down_index_set: u64,
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
        /// Realistic laddered entry at +latency: walking the current top-5
        /// asks for the clip notional (avg price, shares filled).
        ladder_avg_price: Option<f64>,
        ladder_shares: Option<f64>,
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
        /// Realistic laddered round trip: entry from the probe-time walk,
        /// exit selling those shares into the current top-5 bids. Unsold
        /// remainder (thin bids) rides to resolution.
        ladder_entry_avg: Option<f64>,
        ladder_exit_avg: Option<f64>,
        ladder_shares_sold: Option<f64>,
        ladder_mark_pnl_usd: Option<f64>,
    },
    /// Measure-only alternative to the taker exit: an ask resting at the
    /// side mid from the exit walk. `filled` fills the whole clip at `level`
    /// with NO taker fee; a timeout converts against the then-current bids
    /// (taker fee applies, unsold remainder rides); close without either
    /// rides fully to resolution (join by slug+side for the settle).
    PassiveExit {
        ts_utc: String,
        slug: String,
        side: &'static str,
        level: f64,
        filled: bool,
        entry_avg: f64,
        shares: f64,
        /// Proceeds-vs-entry for the sold portion; None when everything
        /// rides to settlement.
        pnl_usd: Option<f64>,
        fallback_exit_avg: Option<f64>,
        fallback_shares_sold: Option<f64>,
    },
    Resolution {
        ts_utc: String,
        slug: String,
        side: &'static str,
        won: bool,
        /// Settlement if held to resolution: 1-entry when won, -entry lost.
        settle_pnl_per_share: f64,
        /// Laddered-fill settlement for shares NOT sold at the exit walk.
        ladder_settle_pnl_usd: Option<f64>,
    },
    /// Measure-only cross-venue telemetry: how far (ms) this venue's price
    /// series leads our Binance ARRIVAL series (positive = venue first).
    VenueLeadLag {
        ts_utc: String,
        venue: &'static str,
        median_receipt_minus_exchange_ms: Option<i64>,
        best_lead_ms: i64,
        corr: f64,
        n_samples: usize,
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

    /// Best bid/ask midpoint; None until both sides are quoted.
    pub fn mid(&self) -> Option<f64> {
        Some((self.best_bid()?.price + self.best_ask()?.price) / 2.0)
    }

    /// Top-`TAPE_DEPTH` ladder in `ReplayEvent` form: bids best-first
    /// (descending), asks best-first (ascending), zero-padded when the book is
    /// thinner than the tape depth.
    fn tape_levels(&self) -> ([pm_types::BookLevel; pm_types::TAPE_DEPTH], [pm_types::BookLevel; pm_types::TAPE_DEPTH]) {
        let mut bids = [pm_types::BookLevel::default(); pm_types::TAPE_DEPTH];
        let mut asks = [pm_types::BookLevel::default(); pm_types::TAPE_DEPTH];
        for (slot, (k, size)) in bids.iter_mut().zip(self.bids.iter().rev()) {
            *slot = pm_types::BookLevel { price: key_price(*k) as f32, size: *size as f32 };
        }
        for (slot, (k, size)) in asks.iter_mut().zip(self.asks.iter()) {
            *slot = pm_types::BookLevel { price: key_price(*k) as f32, size: *size as f32 };
        }
        (bids, asks)
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

    /// Walk the top-5 asks for `notional` dollars, the harness `fill()`
    /// semantics. Returns (avg_price, shares) for whatever depth exists.
    pub fn fill_buy(&self, notional: f64) -> Option<(f64, f64)> {
        let mut remaining = notional;
        let mut cost = 0.0;
        let mut shares = 0.0;
        for (k, size) in self.asks.iter().take(5) {
            if remaining <= 1e-9 {
                break;
            }
            let price = key_price(*k);
            if price <= 0.0 || price >= 1.0 || *size <= 0.0 {
                continue;
            }
            let take = remaining.min(price * size);
            cost += take;
            shares += take / price;
            remaining -= take;
        }
        (shares > 1e-9).then(|| (cost / shares, shares))
    }

    /// Walk the top-5 bids selling `shares`; unsold remainder is the
    /// caller's to settle. Returns (avg_price, shares_sold).
    pub fn fill_sell(&self, shares: f64) -> Option<(f64, f64)> {
        let mut remaining = shares;
        let mut proceeds = 0.0;
        let mut sold = 0.0;
        for (k, size) in self.bids.iter().rev().take(5) {
            if remaining <= 1e-9 {
                break;
            }
            let price = key_price(*k);
            if price <= 0.0 || price >= 1.0 || *size <= 0.0 {
                continue;
            }
            let qty = remaining.min(*size);
            proceeds += qty * price;
            sold += qty;
            remaining -= qty;
        }
        (sold > 1e-9).then(|| (proceeds / sold, sold))
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
    /// CTF binary index set per outcome (1 << position in clobTokenIds).
    pub up_index_set: u64,
    pub down_index_set: u64,
    /// CTF condition id for on-chain redemption; None until Gamma supplies it.
    pub condition_id: Option<String>,
    /// True strike from Gamma/crypto-price (`openPrice`); None until present.
    pub gamma_strike: Option<f64>,
    /// Whether this market has taken at least one entry.
    pub entered: bool,
    /// Entries taken so far. The engine does not cap this: how often to enter
    /// a market is the strategy's decision, not the twin's.
    pub n_clips: u32,
    /// An entry decided but not yet confirmed filled by live execution
    /// (`defer_entry_commit`); `commit_entry` clears or keeps it.
    pub pending_commit: bool,
    /// Belief-dwell telemetry: last tick when sign(p_exo - 0.5) flipped (or
    /// first belief). Logged on would_enter; NOT a decision input.
    pub belief_flip_ns: Option<i64>,
    /// Sign of the last observed belief (p_exo > 0.5).
    pub belief_up: Option<bool>,
    /// Stable per-market id handed to the strategy as `ReplayEvent.market_id`.
    /// Assigned by [`ShadowCore::upsert_market`]; the caller's value is
    /// ignored, so feed code never has to invent one.
    pub id: MarketId,
    /// Observed `max(yes_mid) - min(yes_mid)` so far (live-safe: only ticks
    /// already seen). Feeds `Ctx::market_yes_range_so_far`.
    pub yes_mid_lo: f32,
    pub yes_mid_hi: f32,
    /// Decide passes this market has been handed to the strategy. Feeds
    /// `Ctx::events_seen`, which is per-market in the backtest runner too.
    pub events_seen: u64,
}

/// Rolling per-venue price prints on the local receipt clock, plus
/// exchange-timestamp deltas where the venue provides event times.
#[derive(Debug, Default)]
pub struct VenueBuf {
    ticks: VecDeque<(i64, f64)>, // (receipt_ms, price)
    exch_deltas: VecDeque<i64>,
}

/// Venue tick retention (ms) for the lead-lag estimator window.
const VENUE_KEEP_MS: i64 = 700_000;

impl VenueBuf {
    fn push(&mut self, receipt_ms: i64, price: f64, exchange_ms: Option<i64>) {
        self.ticks.push_back((receipt_ms, price));
        while let Some(&(t, _)) = self.ticks.front() {
            if receipt_ms - t <= VENUE_KEEP_MS {
                break;
            }
            self.ticks.pop_front();
        }
        if let Some(e) = exchange_ms {
            self.exch_deltas.push_back(receipt_ms - e);
            if self.exch_deltas.len() > LATENCY_SAMPLE_CAP {
                self.exch_deltas.pop_front();
            }
        }
    }
}

/// Sample a venue series on a fixed grid (forward-filled last price).
fn grid_returns(ticks: &VecDeque<(i64, f64)>, start_ms: i64, step_ms: i64, n: usize) -> Vec<f64> {
    let mut prices = Vec::with_capacity(n);
    let mut it = ticks.iter().peekable();
    let mut last: Option<f64> = None;
    for k in 0..n {
        let t = start_ms + k as i64 * step_ms;
        while let Some(&&(ts, p)) = it.peek() {
            if ts <= t {
                last = Some(p);
                it.next();
            } else {
                break;
            }
        }
        prices.push(last.unwrap_or(0.0));
    }
    prices
        .windows(2)
        .map(|w| if w[0] > 0.0 && w[1] > 0.0 { (w[1] / w[0]).ln() } else { 0.0 })
        .collect()
}

/// Best lead (ms) of `venue` over `reference` on the receipt clock:
/// max-|corr| lag of 100ms-grid log-returns over the trailing 10 minutes.
/// Positive = venue prints first. None until both series have signal.
pub fn lead_lag_ms(
    venue: &VenueBuf,
    reference: &VenueBuf,
    now_ms: i64,
) -> Option<(i64, f64, usize)> {
    const STEP: i64 = 100;
    const SPAN: usize = 6_000; // 10 min of 100ms cells
    const MAX_LAG_CELLS: i64 = 20; // +-2s
    let start = now_ms - (SPAN as i64) * STEP;
    let v = grid_returns(&venue.ticks, start, STEP, SPAN + 1);
    let r = grid_returns(&reference.ticks, start, STEP, SPAN + 1);
    let n = v.len().min(r.len());
    let energy = |s: &[f64]| s.iter().map(|x| x * x).sum::<f64>();
    let (ev, er) = (energy(&v[..n]), energy(&r[..n]));
    if ev <= 0.0 || er <= 0.0 {
        return None;
    }
    let mut best = (0i64, 0.0f64);
    for lag in -MAX_LAG_CELLS..=MAX_LAG_CELLS {
        // corr( venue[t], reference[t + lag] ): positive lag = venue leads.
        let mut dot = 0.0;
        for t in 0..n {
            let rt = t as i64 + lag;
            if rt >= 0 && (rt as usize) < n {
                dot += v[t] * r[rt as usize];
            }
        }
        let corr = dot / (ev.sqrt() * er.sqrt());
        if corr.abs() > best.1.abs() {
            best = (lag * STEP, corr);
        }
    }
    Some((best.0, best.1, n))
}

/// An entry awaiting official resolution (crypto-price API, post-close).
/// Keyed by `entry_id` (not slug+side): re-entry can put two same-side
/// entries on one market, and each settles independently.
#[derive(Debug, Clone)]
pub struct ResolutionWatch {
    pub entry_id: u64,
    pub slug: String,
    pub side: Side,
    pub entry_touch_price: f64,
    pub open_ts_s: i64,
    pub close_ts_s: i64,
    attempts: u32,
    next_attempt_ns: i64,
    /// Laddered-fill bookkeeping: avg cost from the probe-time walk and the
    /// shares still unsold after the exit walk (they settle at resolution).
    ladder_avg_cost: Option<f64>,
    ladder_unsold: f64,
}

/// A logged WOULD_ENTER awaiting its quote probe and mark-to-book exit.
#[derive(Debug, Clone)]
struct PendingTrade {
    entry_id: u64,
    slug: String,
    side: Side,
    token: String,
    entry_touch_price: f64,
    /// Notional the strategy asked for, used by the probe's laddered walk.
    target_notional: f64,
    probe_due_ns: i64,
    probe_done: bool,
    exit_due_ns: i64,
    exit_done: bool,
    /// Realistic laddered fill captured at probe time (+latency): walking
    /// the then-current top-5 asks for the full clip notional.
    ladder_avg_cost: Option<f64>,
    ladder_shares: Option<f64>,
    /// Measure-only passive-exit probe, armed at the taker-exit walk: an ask
    /// resting at the then-current side mid. Filled when a later bid crosses
    /// the level, converted against the book at the timeout, or left to ride
    /// to settlement if the market closes first.
    passive_level: Option<f64>,
    passive_deadline_ns: i64,
    passive_done: bool,
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
/// An executable entry derived from a `WouldEnter` decision: the engine's
/// decision plus the venue token to buy. Live execution consumes these over a
/// channel; the decision itself (slug/side/p_exo/touch_price) is produced by the
/// The belief on the entered side: `p_exo` is always P(up), so a Down entry
/// reads the complement. Entry telemetry only; the strategy decides the side.
pub fn p_side_for_entry(p_exo: f64, side: &str) -> f64 {
    if side == "up" {
        p_exo
    } else {
        1.0 - p_exo
    }
}

#[derive(Debug, Clone)]
pub struct ExecIntent {
    pub slug: String,
    pub side: String,
    pub token_id: String,
    pub p_exo: f64,
    /// Belief on the entered side (not always `p_exo`).
    pub p_side: f64,
    pub touch_price: f64,
    pub marketable_limit_price: f64,
    pub target_notional: f64,
    pub hold_to_redemption: bool,
    pub clip: u32,
    pub edge: f64,
    pub sigma_bar_bps: f64,
    pub strike: f64,
    pub close_ts_s: i64,
    pub condition_id: Option<String>,
    pub up_index_set: u64,
    pub down_index_set: u64,
}

/// The live twin's engine state.
///
/// One `ShadowCore` drives ONE strategy instance across every concurrently
/// open market, unlike the backtest runner which gives each market its own
/// instance. A strategy that keeps per-market state must therefore key it by
/// `ReplayEvent::market_id`; the shipped `NoopStrategy` keeps none.
pub struct ShadowCore {
    cfg: ShadowConfig,
    strategy: Box<dyn Strategy + Send>,
    model: AlphaModel,
    spot: VecDeque<SpotTick>,
    spot_last_receipt_ms: Option<i64>,
    spot_receipt_deltas_ms: VecDeque<i64>,
    books: HashMap<String, Ladder>,
    book_last_receipt_ms: Option<i64>,
    book_receipt_deltas_ms: VecDeque<i64>,
    markets: HashMap<String, MarketWindow>,
    pending: Vec<PendingTrade>,
    resolutions: Vec<ResolutionWatch>,
    next_entry_id: u64,
    next_market_id: u32,
    stats: SummaryStats,
    /// Measure-only cross-venue buffers: Binance on its ARRIVAL clock as the
    /// reference, plus each candidate fast-trigger venue.
    vbuf_binance: VenueBuf,
    vbuf_kraken: VenueBuf,
    vbuf_coinbase: VenueBuf,
    /// Binance futures prints (perp-led state input; empty when disabled).
    perp_buf: VecDeque<SpotTick>,
    /// 5-minute open interest (ts_ns, contracts); populated from Binance REST.
    oi_series: Vec<(i64, f64)>,
    /// Funding events (ts_ns, rate); populated from Binance REST.
    funding_series: Vec<(i64, f64)>,
    /// Set whenever a decision input mutates (spot/perp/book/oi/funding);
    /// read only by the event-driven decide trigger, cleared after a decide.
    inputs_dirty: bool,
}

/// Engine configuration for the live twin.
///
/// Infrastructure only: which belief model to run, how the measure-only
/// probes are timed, and which strategy is driving. Entry thresholds, gates,
/// re-entry rules and clip sizing are deliberately absent — those were the
/// dead fade's parameters and they belong to a strategy's own config, which
/// travels with the strategy rather than with the twin that hosts it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ShadowConfig {
    /// Name of the driving strategy, for the stream's config record and the
    /// fingerprint (swapping strategies must change the fingerprint).
    pub strategy: String,
    pub vol_lookback_s: u32,
    pub exit_after_s: u32,
    pub latency_probe_ms: u64,
    pub perp_price_weight: f64,
    pub vol_estimator: VolEstimator,
}

/// 16-hex sha256 prefix of the canonical (sorted-keys) JSON of a resolved
/// config. Mirrors `pm_backtest::fingerprint::config_fingerprint`; duplicated
/// here because pm-shadow intentionally does not depend on pm-backtest.
fn config_fingerprint<T: serde::Serialize>(cfg: &T) -> String {
    fn sort_value(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                let mut entries: Vec<(String, serde_json::Value)> =
                    map.iter_mut().map(|(k, v)| (k.clone(), v.take())).collect();
                for (_, v) in &mut entries {
                    sort_value(v);
                }
                entries.sort_by(|a, b| a.0.cmp(&b.0));
                map.clear();
                for (k, v) in entries {
                    map.insert(k, v);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    sort_value(item);
                }
            }
            _ => {}
        }
    }

    let mut value = serde_json::to_value(cfg).unwrap_or(serde_json::Value::Null);
    sort_value(&mut value);
    let canon = serde_json::to_string(&value).unwrap_or_default();
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(canon.as_bytes());
    let digest = hasher.finalize();
    digest
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()[..16]
        .to_string()
}

/// Build `ShadowConfig` from CLI/runtime args.
pub fn shadow_config_from_args(args: &ShadowArgs) -> ShadowConfig {
    ShadowConfig {
        strategy: args.strategy.clone(),
        vol_lookback_s: args.vol_lookback_s,
        exit_after_s: args.exit_after_s,
        latency_probe_ms: args.latency_probe_ms,
        perp_price_weight: args.perp_price_weight,
        vol_estimator: match args.vol_estimator.as_str() {
            "ewma" => VolEstimator::Ewma {
                halflife_s: args.ewma_halflife_s,
            },
            _ => VolEstimator::Realized,
        },
    }
}

impl ShadowCore {
    /// Build a core driving `strategy`.
    ///
    /// `Box<dyn Strategy>` rather than a generic parameter: the core lives
    /// behind `Arc<Mutex<_>>` and is threaded through every feed task, so a
    /// type parameter would propagate across the whole `feeds` module for no
    /// benefit. One virtual call per market per decide pass (1 Hz) is free.
    /// `+ Send` because the core is shared across tokio feed tasks.
    pub fn new(cfg: ShadowConfig, strategy: Box<dyn Strategy + Send>) -> Self {
        let model = AlphaModel {
            cfg: AlphaModelConfig {
                vol_lookback_s: cfg.vol_lookback_s,
                vol_sample_dt_s: 1,
                momentum_lookback_s: 0,
                momentum_weight: 1.0,
                perp_price_weight: cfg.perp_price_weight,
                vol_estimator: cfg.vol_estimator,
                ..AlphaModelConfig::default()
            },
            calibrator: None,
            dir_model: None,
        };
        Self {
            cfg,
            strategy,
            model,
            spot: VecDeque::new(),
            spot_last_receipt_ms: None,
            spot_receipt_deltas_ms: VecDeque::new(),
            books: HashMap::new(),
            book_last_receipt_ms: None,
            book_receipt_deltas_ms: VecDeque::new(),
            markets: HashMap::new(),
            pending: Vec::new(),
            resolutions: Vec::new(),
            next_entry_id: 0,
            next_market_id: 0,
            vbuf_binance: VenueBuf::default(),
            vbuf_kraken: VenueBuf::default(),
            vbuf_coinbase: VenueBuf::default(),
            perp_buf: VecDeque::new(),
            oi_series: Vec::new(),
            funding_series: Vec::new(),
            stats: SummaryStats::default(),
            inputs_dirty: false,
        }
    }

    pub fn inputs_dirty(&self) -> bool {
        self.inputs_dirty
    }

    pub fn clear_inputs_dirty(&mut self) {
        self.inputs_dirty = false;
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
        self.vbuf_binance.push(receipt_ms, price, Some(exchange_ms));
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
        self.inputs_dirty = true;
    }

    pub fn spot_history(&self) -> SpotHistory {
        SpotHistory::new(self.spot.iter().copied().collect())
    }

    /// Record a Binance futures print (perp-led state; measure parity with
    /// the harness PerpState including live OI/funding from REST polls).
    pub fn push_perp(
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
        let _ = receipt_ms;
        self.perp_buf.push_back(SpotTick {
            ts_ns: exchange_ms * 1_000_000,
            price,
            quantity: quantity as f32,
            is_buyer_maker,
        });
        while let Some(front) = self.perp_buf.front() {
            if exchange_ms * 1_000_000 - front.ts_ns <= SPOT_KEEP_SECS * 1_000_000_000 {
                break;
            }
            self.perp_buf.pop_front();
        }
        self.inputs_dirty = true;
    }

    /// Returns true when the series actually mutated (finite, non-stale point).
    fn push_series_point(buf: &mut Vec<(i64, f64)>, ts_ns: i64, value: f64, cap: usize) -> bool {
        if !value.is_finite() {
            return false;
        }
        if let Some((last_ts, last_v)) = buf.last_mut() {
            if *last_ts == ts_ns {
                *last_v = value;
                return true;
            }
            if *last_ts > ts_ns {
                return false;
            }
        }
        buf.push((ts_ns, value));
        if buf.len() > cap {
            let drop = buf.len() - cap;
            buf.drain(0..drop);
        }
        true
    }

    pub fn push_oi(&mut self, exchange_ms: i64, oi: f64) {
        if Self::push_series_point(&mut self.oi_series, exchange_ms * 1_000_000, oi, OI_SERIES_CAP)
        {
            self.inputs_dirty = true;
        }
    }

    pub fn push_funding(&mut self, exchange_ms: i64, rate: f64) {
        if Self::push_series_point(
            &mut self.funding_series,
            exchange_ms * 1_000_000,
            rate,
            FUNDING_SERIES_CAP,
        ) {
            self.inputs_dirty = true;
        }
    }

    fn perp_state(&self) -> Option<pm_alpha::PerpState> {
        if self.cfg.perp_price_weight == 0.0 || self.perp_buf.is_empty() {
            return None;
        }
        Some(pm_alpha::PerpState {
            trades: SpotHistory::new(self.perp_buf.iter().copied().collect()),
            oi: self.oi_series.clone(),
            funding: self.funding_series.clone(),
        })
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
        self.inputs_dirty = true;
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
        self.inputs_dirty = true;
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
    pub fn upsert_market(&mut self, mut market: MarketWindow) {
        match self.markets.get_mut(&market.slug) {
            Some(existing) => {
                if existing.gamma_strike.is_none() {
                    existing.gamma_strike = market.gamma_strike;
                }
                if existing.condition_id.is_none() {
                    existing.condition_id = market.condition_id;
                }
            }
            None => {
                market.id = MarketId(self.next_market_id);
                self.next_market_id = self.next_market_id.saturating_add(1);
                self.markets.insert(market.slug.clone(), market);
            }
        }
    }

    /// Apply a deferred entry commit after live execution fills (keep pending
    /// on a miss so the next fill attempt still commits).
    ///
    /// Only the engine's own bookkeeping is deferred. The strategy has already
    /// advanced its internal state by the time the order is submitted and the
    /// trait has no rollback hook, so a strategy whose entries must be
    /// exactly-once has to reconcile that itself. Nothing shipped does.
    pub fn commit_entry(&mut self, slug: &str, filled: bool) {
        let Some(m) = self.markets.get_mut(slug) else {
            return;
        };
        if !m.pending_commit || !filled {
            return;
        }
        m.pending_commit = false;
        m.n_clips = m.n_clips.saturating_add(1);
        m.entered = true;
    }

    fn basis_mom_60s_bps(perp: Option<&PerpState>, spot: &SpotHistory, now_ns: i64) -> f64 {
        match perp {
            Some(p) => match (
                p.basis_frac(spot, now_ns),
                p.basis_frac(spot, now_ns - 60 * 1_000_000_000),
            ) {
                (Some(a), Some(b)) => (a - b) * 1e4,
                _ => 0.0,
            },
            None => 0.0,
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

/// Side-oriented Binance spot flow features for discovery (logging only).
#[derive(Debug, Clone, Copy)]
struct BinanceFlowTelemetry {
    flow_imbal_5s: f64,
    flow_imbal_15s: f64,
    flow_imbal_30s: f64,
    adverse_vol_5s: f64,
    adverse_vol_15s: f64,
    adverse_vol_30s: f64,
    basis_d60_bps: f64,
}

impl BinanceFlowTelemetry {
    fn compute(spot: &SpotHistory, now_ns: i64, side: Side, basis_d60_bps: f64) -> Self {
        let is_buy_yes = matches!(side, Side::Up);
        let f5 = spot.signed_flow_and_adverse(now_ns, 5_000_000_000, is_buy_yes);
        let f15 = spot.signed_flow_and_adverse(now_ns, 15_000_000_000, is_buy_yes);
        let f30 = spot.signed_flow_and_adverse(now_ns, 30_000_000_000, is_buy_yes);
        Self {
            flow_imbal_5s: f5.imbalance,
            flow_imbal_15s: f15.imbalance,
            flow_imbal_30s: f30.imbalance,
            adverse_vol_5s: f5.adverse_volume,
            adverse_vol_15s: f15.adverse_volume,
            adverse_vol_30s: f30.adverse_volume,
            basis_d60_bps,
        }
    }
}

/// Decision-layer context for cluster/trend monitoring (does not affect belief).
fn entry_decision_telemetry(
    state: &ExoState<'_>,
    belief: &pm_alpha::Belief,
    vol_lookback_s: u32,
    spot: &SpotHistory,
    now_ns: i64,
    open_ns: i64,
    p_exo: f64,
    p_side: f64,
    touch_price: f64,
    yes_ask: f64,
    no_ask: f64,
) -> (
    f64,
    f64,
    f64,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    f64,
    u32,
    f64,
    Vec<f32>,
    Vec<f32>,
    Option<&'static str>,
) {
    let p_down = 1.0 - p_exo;
    let edge_up = p_exo - yes_ask;
    let edge_down = p_down - no_ask;
    let spot_ret_10s_bps = spot_ret_bps(spot, now_ns, 10);
    let spot_ret_30s_bps = spot_ret_bps(spot, now_ns, 30);
    let spot_ret_60s_bps = spot_ret_bps(spot, now_ns, 60);
    let spot_ret_120s_bps = spot_ret_bps(spot, now_ns, 120);
    let spot_ret_300s_bps = spot_ret_bps(spot, now_ns, 300);
    let spot_ret_600s_bps = spot_ret_bps(spot, now_ns, 600);
    let spot_ret_900s_bps = spot_ret_bps(spot, now_ns, 900);
    let regime = regime::classify(spot, now_ns).map(|r| r.as_str());
    let secs_from_open = ((now_ns.saturating_sub(open_ns)) / 1_000_000_000).max(0) as u32;
    let model_book_gap = p_side - touch_price;
    let delta_bps = state
        .spot_now()
        .filter(|s| s.is_finite() && state.market.strike > 0.0)
        .map(|s| ((s - state.market.strike) / state.market.strike * 10_000.0).clamp(-300.0, 300.0))
        .unwrap_or(0.0);
    let exo = exo_features(state, belief, vol_lookback_s).values.to_vec();
    debug_assert_eq!(exo.len(), EXO_FEATURES);
    let dir = dir_features(state, belief.sigma_bar_bps).values.to_vec();
    debug_assert_eq!(dir.len(), DIR_FEATURES);
    (
        p_down,
        edge_up,
        edge_down,
        spot_ret_10s_bps,
        spot_ret_30s_bps,
        spot_ret_60s_bps,
        spot_ret_120s_bps,
        spot_ret_300s_bps,
        spot_ret_600s_bps,
        spot_ret_900s_bps,
        model_book_gap,
        secs_from_open,
        delta_bps,
        exo,
        dir,
        regime,
    )
}

impl ShadowCore {
    /// One decision pass over all active windows, mirroring the harness:
    /// belief from ExoState, edge per side vs the REAL touch asks, enter on
    /// the first crossing of `edge_threshold`. Call at ~1s cadence.
    /// The venue token for a decided (slug, side). Used by live execution to
    /// turn a `WouldEnter` into an order; None if the market isn't tracked.
    pub fn token_for(&self, slug: &str, side: &str) -> Option<String> {
        let m = self.markets.get(slug)?;
        Some(if side == "up" {
            m.up_token.clone()
        } else {
            m.down_token.clone()
        })
    }

    /// One decision pass over every live window.
    ///
    /// The engine derives the belief and the book snapshot; the strategy makes
    /// the call. Each open market is handed to `Strategy::on_event` as a
    /// `ReplayEvent` built from its Up (YES) ladder, exactly the type the
    /// backtest replays, and every returned BUY becomes a WOULD_ENTER that the
    /// probe/exit/resolution machinery then measures.
    ///
    /// `on_event` rather than `on_event_scored`: the scored variant overrides
    /// the backtest runner's `pm_model` evaluation, and this twin has no
    /// `pm_model` gate to override. Its belief is pm-alpha's, computed here as
    /// entry telemetry.
    ///
    /// Sell orders are ignored (with a warning): the stream models an entry
    /// and the exit the engine measures for it, so there is no strategy-held
    /// position for a sell to close.
    pub fn decide(&mut self, now_ns: i64, defer_entry_commit: bool) -> Vec<LogEvent> {
        let spot = self.spot_history();
        let perp = self.perp_state();
        // Warm-up gate: with a partially-filled buffer (post-restart) the
        // vol estimate runs on a truncated window and produces off-model
        // beliefs the full-history replay would never hold. Stand down
        // until the buffer spans the vol lookback.
        match (self.spot.front(), self.spot.back()) {
            (Some(first), Some(last))
                if last.ts_ns - first.ts_ns
                    >= self.cfg.vol_lookback_s as i64 * 1_000_000_000 => {}
            _ => return Vec::new(),
        }
        let whipsaw = if spot.is_empty() {
            WhipsawRiskSnapshot::default()
        } else {
            WhipsawRiskSnapshot::from_history(now_ns, &spot)
        };
        // The twin has no Polymarket trade tape; strategies that need one must
        // wait for a feed rather than silently read an empty history as calm.
        let trades = TradeHistory::default();
        let mut out = Vec::new();
        let mut entries: Vec<PendingTrade> = Vec::new();

        for m in self.markets.values_mut() {
            let open_ns = m.open_ts_s * 1_000_000_000;
            let close_ns = m.close_ts_s * 1_000_000_000;
            // Engine safety rail, not a strategy parameter: the twin never
            // opens inside the last 90s, because an entry it cannot probe and
            // measure before close is a record with nothing behind it.
            let deadline_ns = close_ns - STOP_BEFORE_CLOSE_S * 1_000_000_000;
            if now_ns < open_ns || now_ns >= deadline_ns || m.pending_commit {
                continue;
            }
            // The strike must share the belief state's price basis: gamma's
            // openPrice is a BTC/USD-index level ~14bps off Binance BTC/USDT
            // and injects a phantom edge if mixed with Binance state. Gamma is
            // the fallback only when the spot buffer can't cover the open.
            let Some((strike, strike_source)) = ({
                if let Some(p) = spot.price_at_or_before(open_ns) {
                    Some((p, "binance_proxy"))
                } else {
                    m.gamma_strike.map(|s| (s, "gamma"))
                }
            }) else {
                continue;
            };
            let Some(token) = Token::from_slug(&m.slug) else {
                continue;
            };
            let state = ExoState {
                spot: &spot,
                perp: perp.as_ref(),
                ref_spot: None,
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
            // Belief-dwell telemetry (logged only; never a decision input).
            let up_now = ev.p > 0.5;
            if m.belief_up != Some(up_now) {
                m.belief_up = Some(up_now);
                m.belief_flip_ns = Some(now_ns);
            }
            let (Some(up_book), Some(down_book)) =
                (self.books.get(&m.up_token), self.books.get(&m.down_token))
            else {
                continue;
            };
            let (Some(up_ask), Some(down_ask)) = (up_book.best_ask(), down_book.best_ask()) else {
                continue;
            };

            let (bids, asks) = up_book.tape_levels();
            let yes_mid = up_book
                .mid()
                .unwrap_or((up_ask.price + (1.0 - down_ask.price)) / 2.0);
            let event = ReplayEvent {
                ts_ns: now_ns,
                market_id: m.id,
                yes_mid: yes_mid as f32,
                yes_bid: up_book.best_bid().map(|t| t.price).unwrap_or(0.0) as f32,
                yes_ask: up_ask.price as f32,
                // Live books carry no traded-volume field; the tape's is a
                // per-event print size the feed does not give us.
                volume: 0.0,
                bids,
                asks,
                spot_price: state.spot_now().unwrap_or(0.0) as f32,
                flags: ReplayFlags::BOOK_UPDATE,
            };
            m.events_seen = m.events_seen.saturating_add(1);
            m.yes_mid_lo = m.yes_mid_lo.min(event.yes_mid);
            m.yes_mid_hi = m.yes_mid_hi.max(event.yes_mid);
            let ctx = Ctx {
                events_seen: m.events_seen,
                // The twin holds no capital by construction.
                cash_usdc: 0.0,
                market_yes_range_so_far: (m.yes_mid_hi - m.yes_mid_lo).max(0.0),
                regime_path_efficiency: whipsaw.path_efficiency,
                regime_reversal_pressure: whipsaw.reversal_pressure,
                regime_sign_flip_rate: whipsaw.sign_flip_rate,
                regime_realized_vol_180s_bps: whipsaw.realized_vol_180s_bps,
                market_close_ns: close_ns,
            };
            let output = self.strategy.on_event(&event, &ctx, &spot, &trades);
            if output.orders.is_empty() {
                continue;
            }

            let mut clip_index = m.n_clips;
            let mut took_entry = false;
            for req in output.orders {
                let side = match req.side {
                    StratSide::BuyYes => Side::Up,
                    StratSide::BuyNo => Side::Down,
                    StratSide::SellYes | StratSide::SellNo => {
                        tracing::warn!(
                            slug = %m.slug,
                            tag = req.tag,
                            "shadow ignored a sell order: the twin logs entries and \
                             measures their exits, it holds no strategy position"
                        );
                        continue;
                    }
                };
                let touch = match side {
                    Side::Up => up_ask,
                    Side::Down => down_ask,
                };
                let p_side = p_side_for_entry(ev.p, side.as_str());
                let edge = p_side - touch.price;
                // `limit_price` is quoted in YES terms; the NO leg inverts it.
                // A market order is marketable at the touch by definition.
                let marketable_limit = match (req.limit_price, side) {
                    (Some(lp), Side::Up) => lp as f64,
                    (Some(lp), Side::Down) => 1.0 - lp as f64,
                    (None, _) => touch.price,
                };
                let target_notional = req.shares * touch.price;
                clip_index = clip_index.saturating_add(1);
                took_entry = true;

                let entry_id = self.next_entry_id;
                self.next_entry_id += 1;
                self.stats.entries_total += 1;
                let (
                    p_down,
                    edge_up,
                    edge_down,
                    spot_ret_10s_bps,
                    spot_ret_30s_bps,
                    spot_ret_60s_bps,
                    spot_ret_120s_bps,
                    spot_ret_300s_bps,
                    spot_ret_600s_bps,
                    spot_ret_900s_bps,
                    model_book_gap,
                    secs_from_open,
                    delta_bps,
                    exo_features,
                    dir_features,
                    regime,
                ) = entry_decision_telemetry(
                    &state,
                    &ev.raw,
                    self.cfg.vol_lookback_s,
                    &spot,
                    now_ns,
                    open_ns,
                    ev.p,
                    p_side,
                    touch.price,
                    up_ask.price,
                    down_ask.price,
                );
                let token_id = match side {
                    Side::Up => m.up_token.clone(),
                    Side::Down => m.down_token.clone(),
                };
                let basis_d60 = Self::basis_mom_60s_bps(perp.as_ref(), &spot, now_ns);
                let flow = BinanceFlowTelemetry::compute(&spot, now_ns, side, basis_d60);
                out.push(LogEvent::WouldEnter {
                    ts_utc: ts_utc(now_ns),
                    slug: m.slug.clone(),
                    side: side.as_str(),
                    p_exo: ev.p,
                    p_side,
                    p_down,
                    edge_up,
                    edge_down,
                    spot_ret_10s_bps,
                    spot_ret_30s_bps,
                    spot_ret_60s_bps,
                    spot_ret_120s_bps,
                    spot_ret_300s_bps,
                    spot_ret_600s_bps,
                    spot_ret_900s_bps,
                    model_book_gap,
                    secs_from_open,
                    belief_dwell_s: m
                        .belief_flip_ns
                        .map(|f| (now_ns.saturating_sub(f)) as f64 / 1e9),
                    delta_bps,
                    exo_features,
                    dir_features,
                    regime,
                    binance_flow_imbal_5s: flow.flow_imbal_5s,
                    binance_flow_imbal_15s: flow.flow_imbal_15s,
                    binance_flow_imbal_30s: flow.flow_imbal_30s,
                    binance_adverse_vol_5s: flow.adverse_vol_5s,
                    binance_adverse_vol_15s: flow.adverse_vol_15s,
                    binance_adverse_vol_30s: flow.adverse_vol_30s,
                    basis_d60_bps: flow.basis_d60_bps,
                    touch_price: touch.price,
                    touch_size: touch.size,
                    edge,
                    marketable_limit_price: marketable_limit,
                    strike,
                    strike_source,
                    sigma_bar_bps: ev.raw.sigma_bar_bps,
                    lane: req.tag,
                    clip: clip_index,
                    token_id: token_id.clone(),
                    target_notional,
                    close_ts_s: m.close_ts_s,
                    condition_id: m.condition_id.clone(),
                    up_index_set: m.up_index_set,
                    down_index_set: m.down_index_set,
                });
                entries.push(PendingTrade {
                    entry_id,
                    slug: m.slug.clone(),
                    side,
                    token: token_id,
                    entry_touch_price: touch.price,
                    target_notional,
                    probe_due_ns: now_ns + self.cfg.latency_probe_ms as i64 * 1_000_000,
                    probe_done: false,
                    // Harness exits require a tick at or before close; clamp.
                    exit_due_ns: (now_ns + self.cfg.exit_after_s as i64 * 1_000_000_000)
                        .min(close_ns),
                    // exit_after_s == 0 HOLDS to expiry: the exit is pre-marked
                    // done so no WouldExit is ever emitted; the ResolutionWatch
                    // settles the full laddered position.
                    exit_done: self.cfg.exit_after_s == 0,
                    ladder_avg_cost: None,
                    ladder_shares: None,
                    passive_level: None,
                    passive_deadline_ns: 0,
                    passive_done: true,
                });
                self.resolutions.push(ResolutionWatch {
                    entry_id,
                    slug: m.slug.clone(),
                    side,
                    entry_touch_price: touch.price,
                    open_ts_s: m.open_ts_s,
                    close_ts_s: m.close_ts_s,
                    attempts: 0,
                    next_attempt_ns: close_ns + 15_000_000_000,
                    ladder_avg_cost: None,
                    ladder_unsold: 0.0,
                });
            }
            if took_entry {
                if defer_entry_commit {
                    m.pending_commit = true;
                } else {
                    m.n_clips = clip_index;
                    m.entered = true;
                }
            }
        }
        self.pending.extend(entries);
        out
    }

    /// Record a print from a candidate fast-trigger venue (measure-only;
    /// never enters the belief or the spot history).
    pub fn push_venue(
        &mut self,
        venue: &'static str,
        receipt_ms: i64,
        price: f64,
        exchange_ms: Option<i64>,
    ) {
        if !(price.is_finite() && price > 0.0) {
            return;
        }
        match venue {
            "kraken" => self.vbuf_kraken.push(receipt_ms, price, exchange_ms),
            "coinbase" => self.vbuf_coinbase.push(receipt_ms, price, exchange_ms),
            _ => {}
        }
    }

    /// Per-venue lead-lag telemetry vs the Binance arrival series.
    pub fn venue_events(&self, now_ns: i64, now_ms: i64) -> Vec<LogEvent> {
        [("kraken", &self.vbuf_kraken), ("coinbase", &self.vbuf_coinbase)]
            .into_iter()
            .filter_map(|(venue, buf)| {
                let (best_lead_ms, corr, n_samples) =
                    lead_lag_ms(buf, &self.vbuf_binance, now_ms)?;
                Some(LogEvent::VenueLeadLag {
                    ts_utc: ts_utc(now_ns),
                    venue,
                    median_receipt_minus_exchange_ms: median(&buf.exch_deltas),
                    best_lead_ms,
                    corr,
                    n_samples,
                })
            })
            .collect()
    }

    /// Resolution lookups due now; bumps each returned watch's retry clock
    /// (60s apart, 10 attempts max — exhausted watches are dropped).
    pub fn resolutions_due(&mut self, now_ns: i64) -> Vec<ResolutionWatch> {
        self.resolutions.retain(|w| w.attempts < 10);
        let mut due = Vec::new();
        for w in &mut self.resolutions {
            if now_ns >= w.next_attempt_ns {
                w.attempts += 1;
                w.next_attempt_ns = now_ns + 60_000_000_000;
                due.push(w.clone());
            }
        }
        due
    }

    /// Record an official outcome for a watched entry and emit the event.
    /// Keyed by `entry_id`: with re-entry, one market/side can carry several
    /// watches (one per clip) and each settles independently.
    pub fn apply_resolution(&mut self, entry_id: u64, won: bool, now_ns: i64) -> Option<LogEvent> {
        let idx = self
            .resolutions
            .iter()
            .position(|w| w.entry_id == entry_id)?;
        let w = self.resolutions.swap_remove(idx);
        // The trait's `market_mid` is YES-terms; a Down entry's touch is the
        // NO price, so invert it back. `resolved_yes` is the market's outcome,
        // which is our win only when we were on the Up side.
        let entry_yes_price = match w.side {
            Side::Up => w.entry_touch_price,
            Side::Down => 1.0 - w.entry_touch_price,
        };
        let resolved_yes = won == matches!(w.side, Side::Up);
        self.strategy
            .on_market_resolved(entry_yes_price as f32, resolved_yes);
        let settle = if won {
            1.0 - w.entry_touch_price
        } else {
            -w.entry_touch_price
        };
        let ladder_settle_pnl_usd = w.ladder_avg_cost.map(|avg| {
            let per_share = if won { 1.0 - avg } else { -avg };
            w.ladder_unsold * per_share
        });
        Some(LogEvent::Resolution {
            ts_utc: ts_utc(now_ns),
            side: w.side.as_str(),
            slug: w.slug,
            won,
            settle_pnl_per_share: settle,
            ladder_settle_pnl_usd,
        })
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
                let ladder_fill = ladder.and_then(|l| l.fill_buy(p.target_notional));
                if let Some((avg, shares)) = ladder_fill {
                    p.ladder_avg_cost = Some(avg);
                    p.ladder_shares = Some(shares);
                    if let Some(w) = self
                        .resolutions
                        .iter_mut()
                        .find(|w| w.entry_id == p.entry_id)
                    {
                        w.ladder_avg_cost = Some(avg);
                        w.ladder_unsold = shares; // until the exit walk sells
                    }
                }
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
                    ladder_avg_price: ladder_fill.map(|(a, _)| a),
                    ladder_shares: ladder_fill.map(|(_, s)| s),
                });
            }
            if !p.exit_done && now_ns >= p.exit_due_ns {
                p.exit_done = true;
                let ladder = self.books.get(&p.token);
                let exit_touch = ladder.and_then(Ladder::best_bid);
                let mark_pnl_per_share = exit_touch.map(|t| t.price - p.entry_touch_price);
                if let Some(pnl) = mark_pnl_per_share {
                    self.stats.mark_pnl_sum += pnl;
                    self.stats.mark_pnl_count += 1;
                }
                let mut ladder_exit_avg = None;
                let mut ladder_shares_sold = None;
                let mut ladder_mark_pnl_usd = None;
                if let (Some(avg), Some(shares)) = (p.ladder_avg_cost, p.ladder_shares) {
                    let sale = ladder.and_then(|l| l.fill_sell(shares));
                    if let Some((sell_avg, sold)) = sale {
                        ladder_exit_avg = Some(sell_avg);
                        ladder_shares_sold = Some(sold);
                        ladder_mark_pnl_usd = Some(sold * (sell_avg - avg));
                        if let Some(w) = self
                            .resolutions
                            .iter_mut()
                            .find(|w| w.entry_id == p.entry_id)
                        {
                            w.ladder_unsold = (shares - sold).max(0.0);
                        }
                    }
                }
                out.push(LogEvent::WouldExit {
                    ts_utc: ts_utc(now_ns),
                    slug: p.slug.clone(),
                    side: p.side.as_str(),
                    entry_touch_price: p.entry_touch_price,
                    exit_touch,
                    mark_pnl_per_share,
                    ladder_entry_avg: p.ladder_avg_cost,
                    ladder_exit_avg,
                    ladder_shares_sold,
                    ladder_mark_pnl_usd,
                });
                if p.ladder_avg_cost.is_some()
                    && p.ladder_shares.is_some()
                    && let Some(mid) = ladder.and_then(|l| {
                        let b = l.best_bid()?.price;
                        let a = l.best_ask()?.price;
                        Some(0.5 * (b + a))
                    })
                {
                    p.passive_level = Some(mid);
                    p.passive_deadline_ns =
                        now_ns + PASSIVE_EXIT_TIMEOUT_S * 1_000_000_000;
                    p.passive_done = false;
                }
            }
            if !p.passive_done
                && let (Some(level), Some(avg), Some(shares)) =
                    (p.passive_level, p.ladder_avg_cost, p.ladder_shares)
            {
                let ladder = self.books.get(&p.token);
                let close_ns = self
                    .markets
                    .get(&p.slug)
                    .map(|m| m.close_ts_s * 1_000_000_000)
                    .unwrap_or(i64::MAX);
                let crossed = ladder
                    .and_then(Ladder::best_bid)
                    .is_some_and(|b| b.price >= level - 1e-9);
                if crossed {
                    p.passive_done = true;
                    out.push(LogEvent::PassiveExit {
                        ts_utc: ts_utc(now_ns),
                        slug: p.slug.clone(),
                        side: p.side.as_str(),
                        level,
                        filled: true,
                        entry_avg: avg,
                        shares,
                        pnl_usd: Some(shares * (level - avg)),
                        fallback_exit_avg: None,
                        fallback_shares_sold: None,
                    });
                } else if now_ns >= close_ns {
                    p.passive_done = true;
                    out.push(LogEvent::PassiveExit {
                        ts_utc: ts_utc(now_ns),
                        slug: p.slug.clone(),
                        side: p.side.as_str(),
                        level,
                        filled: false,
                        entry_avg: avg,
                        shares,
                        pnl_usd: None,
                        fallback_exit_avg: None,
                        fallback_shares_sold: None,
                    });
                } else if now_ns >= p.passive_deadline_ns {
                    p.passive_done = true;
                    let sale = ladder.and_then(|l| l.fill_sell(shares));
                    out.push(LogEvent::PassiveExit {
                        ts_utc: ts_utc(now_ns),
                        slug: p.slug.clone(),
                        side: p.side.as_str(),
                        level,
                        filled: false,
                        entry_avg: avg,
                        shares,
                        pnl_usd: sale.map(|(px, sold)| sold * (px - avg)),
                        fallback_exit_avg: sale.map(|(px, _)| px),
                        fallback_shares_sold: sale.map(|(_, sold)| sold),
                    });
                }
            }
        }
        self.pending
            .retain(|p| !(p.probe_done && p.exit_done && p.passive_done));
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

/// Live-execution feedback: apply (or restore) a deferred entry commit.
#[derive(Debug, Clone)]
pub struct EntryCommit {
    pub slug: String,
    pub filled: bool,
}

pub async fn run_shadow(args: ShadowArgs) -> Result<()> {
    run_shadow_with_sink(args, None, None).await
}

/// Decision cadence in ns, clamped to >= 20ms so the 10ms poll loop keeps
/// at least one non-decide tick between decisions.
fn decide_interval_ns(ms: u64) -> i64 {
    (ms.max(20) as i64).saturating_mul(1_000_000)
}

/// Minimum spacing between event-triggered decisions (decide-on-event mode).
const EVENT_DECIDE_MIN_SPACING_NS: i64 = 20_000_000;

/// Decide-trigger predicate: cadence-due fires in either mode; a dirty input
/// additionally fires in event mode once the 20ms spacing floor is met.
fn should_decide(
    now_ns: i64,
    next_decide_ns: i64,
    dirty: bool,
    on_event: bool,
    last_decide_ns: i64,
) -> bool {
    now_ns >= next_decide_ns
        || (on_event
            && dirty
            && now_ns >= last_decide_ns.saturating_add(EVENT_DECIDE_MIN_SPACING_NS))
}

/// Same proven shadow engine + feeds + decide loop as `run_shadow`, but each
/// `WouldEnter` decision is also emitted as an `ExecIntent` over `intent_tx` for
/// live execution. The decision path is UNCHANGED — live trading driven off this
/// channel makes byte-identical entry decisions to shadow/backtest. `None` =
/// pure log-only shadow (the CLI path). Send [`EntryCommit`] on `commit_tx` after
/// each submit attempt so deferred entry state stays consistent.
pub async fn run_shadow_with_sink(
    args: ShadowArgs,
    intent_tx: Option<tokio::sync::mpsc::UnboundedSender<ExecIntent>>,
    mut commit_rx: Option<tokio::sync::mpsc::UnboundedReceiver<EntryCommit>>,
) -> Result<()> {
    let (mut logger, log_path) = Logger::create(&args.out_dir)?;
    let shadow_cfg = shadow_config_from_args(&args);
    let config_fingerprint = config_fingerprint(&shadow_cfg);
    tracing::info!(
        log = %log_path.display(),
        config_fingerprint = %config_fingerprint,
        "shadow mode: LOG ONLY, zero orders"
    );

    let defer_entry_commit = intent_tx.is_some();
    let strategy = strategy_from_name(&args.strategy)?;
    let core = std::sync::Arc::new(std::sync::Mutex::new(ShadowCore::new(
        shadow_cfg, strategy,
    )));

    // FIRST event of every stream: the resolved config, so a reader derives
    // the run's invocation from the log itself (F2) and any clap-default
    // drift is visible in the stream, not just the launcher.
    let config_event = LogEvent::Config {
        ts_utc: ts_utc(now_unix_ns()),
        slug_prefix: args.slug_prefix.clone(),
        out_dir: args.out_dir.display().to_string(),
        strategy: args.strategy.clone(),
        decide_interval_ms: args.decide_interval_ms,
        decide_on_event: args.decide_on_event,
        perp_price_weight: args.perp_price_weight,
        vol_lookback_s: args.vol_lookback_s,
        vol_estimator: args.vol_estimator.clone(),
        config_fingerprint: config_fingerprint.clone(),
    };
    tracing::info!(
        event = %serde_json::to_string(&config_event).unwrap_or_default(),
        "shadow"
    );
    logger.write(&config_event)?;

    bootstrap::warm_buffers(core.clone(), args.perp_price_weight).await?;

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
    let (resolution_tx, mut resolution_rx) =
        tokio::sync::mpsc::unbounded_channel::<LogEvent>();
    let marker_task = tokio::spawn(feeds::resolution_marker_feed(
        core.clone(),
        resolution_tx,
        shutdown_rx.clone(),
    ));
    let kraken_task = tokio::spawn(feeds::venue_feed(
        "kraken",
        core.clone(),
        shutdown_rx.clone(),
    ));
    let coinbase_task = tokio::spawn(feeds::venue_feed(
        "coinbase",
        core.clone(),
        shutdown_rx.clone(),
    ));
    let perp_task = if args.perp_price_weight != 0.0 {
        tokio::spawn(feeds::binance_perp_feed(core.clone(), shutdown_rx.clone()))
    } else {
        tokio::spawn(async {})
    };
    let perp_metrics_task = if args.perp_price_weight != 0.0 {
        tokio::spawn(feeds::perp_metrics_poll(core.clone(), shutdown_rx.clone()))
    } else {
        tokio::spawn(async {})
    };

    // 10ms poll keeps the latency probe honest (~±10ms of the target);
    // decisions run on the configured cadence (default 1s = harness-matched);
    // summaries every 60s.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let decide_interval_ns = decide_interval_ns(args.decide_interval_ms);
    let decide_on_event = args.decide_on_event;
    let mut next_decide_ns = 0i64;
    let mut last_decide_ns = i64::MIN;
    let mut next_summary_ns = now_unix_ns() + 60_000_000_000;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("SIGINT: flushing shadow log and exiting");
                break;
            }
            Some(event) = resolution_rx.recv() => {
                tracing::info!(event = %serde_json::to_string(&event).unwrap_or_default(), "shadow");
                logger.write(&event)?;
            }
            _ = tick.tick() => {
                let now_ns = now_unix_ns();
                if let Some(rx) = commit_rx.as_mut() {
                    while let Ok(commit) = rx.try_recv() {
                        core.lock()
                            .expect("shadow core poisoned")
                            .commit_entry(&commit.slug, commit.filled);
                    }
                }
                let mut events = Vec::new();
                {
                    let mut core = core.lock().expect("shadow core poisoned");
                    events.extend(core.poll_due(now_ns));
                    let dirty = decide_on_event && core.inputs_dirty();
                    if should_decide(now_ns, next_decide_ns, dirty, decide_on_event, last_decide_ns) {
                        next_decide_ns = now_ns + decide_interval_ns;
                        if decide_on_event {
                            core.clear_inputs_dirty();
                            last_decide_ns = now_ns;
                        }
                        core.prune(now_ns);
                        events.extend(core.decide(now_ns, defer_entry_commit));
                    }
                    if now_ns >= next_summary_ns {
                        next_summary_ns = now_ns + 60_000_000_000;
                        let now_ms = now_unix_ms();
                        events.push(core.summary(now_ns, now_ms));
                        events.extend(core.venue_events(now_ns, now_ms));
                    }
                }
                for event in &events {
                    tracing::info!(event = %serde_json::to_string(event).unwrap_or_default(), "shadow");
                    logger.write(event)?;
                }
                // Live-execution seam: emit each entry decision as an ExecIntent.
                // The decision is the engine's UNCHANGED WouldEnter; only the
                // venue token is attached. No-op when no sink is attached.
                if let Some(tx) = &intent_tx {
                    let core = core.lock().expect("shadow core poisoned");
                    for event in &events {
                        if let LogEvent::WouldEnter {
                            slug,
                            side,
                            p_exo,
                            p_side,
                            touch_price,
                            marketable_limit_price,
                            target_notional,
                            clip,
                            edge,
                            sigma_bar_bps,
                            strike,
                            ..
                        } = event
                        {
                            if let Some(m) = core.markets.get(slug) {
                                let token_id = if *side == "up" {
                                    m.up_token.clone()
                                } else {
                                    m.down_token.clone()
                                };
                                let _ = tx.send(ExecIntent {
                                    slug: slug.clone(),
                                    side: (*side).to_string(),
                                    token_id,
                                    p_exo: *p_exo,
                                    p_side: *p_side,
                                    touch_price: *touch_price,
                                    marketable_limit_price: *marketable_limit_price,
                                    target_notional: *target_notional,
                                    hold_to_redemption: core.cfg.exit_after_s == 0,
                                    clip: *clip,
                                    edge: *edge,
                                    sigma_bar_bps: *sigma_bar_bps,
                                    strike: *strike,
                                    close_ts_s: m.close_ts_s,
                                    condition_id: m.condition_id.clone(),
                                    up_index_set: m.up_index_set,
                                    down_index_set: m.down_index_set,
                                });
                            }
                        }
                    }
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
        futures::future::join_all([
            spot_task,
            discovery_task,
            book_task,
            marker_task,
            kraken_task,
            coinbase_task,
            perp_task,
            perp_metrics_task,
        ]),
    )
    .await;
    Ok(())
}

/// Pre-warm spot/perp buffers from Binance 1m klines so vol3600 clears on restart.
mod bootstrap {
    use super::ShadowCore;
    use anyhow::Context;
    use std::sync::{Arc, Mutex};

    type Core = Arc<Mutex<ShadowCore>>;

    pub async fn warm_buffers(core: Core, perp_weight: f64) -> anyhow::Result<()> {
        // The realized-vol estimator samples spot on a 1-SECOND grid
        // (vol_sample_dt_s=1). 1m klines give one price per 60s, so the 1s grid
        // becomes a step function (59 zero-returns + 1 jump) and vol is badly
        // under-estimated until live ticks replace it -> off-model first hour.
        // Backfill SPOT at 1s granularity (the level the estimator actually
        // uses) so the bootstrap vol matches the live tick-sampled vol and the
        // engine has parity from t=0.
        match warm_spot_1s(&core, 3600).await {
            Ok(n) => tracing::info!(klines = n, "spot buffer pre-warmed from Binance 1s klines"),
            Err(e) => tracing::warn!(error = %e, "spot 1s klines pre-warm failed; live warm-up fallback"),
        }
        if perp_weight > 0.0 {
            match warm_klines(
                &core,
                "https://fapi.binance.com/fapi/v1/klines?symbol=BTCUSDT&interval=1m&limit=70",
                true,
            )
            .await
            {
                Ok(n) => tracing::info!(klines = n, "perp buffer pre-warmed from Binance futures klines"),
                Err(e) => tracing::warn!(error = %e, "perp klines pre-warm failed; live warm-up fallback"),
            }
            match warm_perp_metrics(&core).await {
                Ok((oi, funding)) => {
                    tracing::info!(oi_rows = oi, funding_rows = funding, "perp OI/funding pre-warmed");
                }
                Err(e) => tracing::warn!(error = %e, "perp OI/funding pre-warm failed; poll will backfill"),
            }
        }
        Ok(())
    }

    async fn warm_perp_metrics(core: &Core) -> anyhow::Result<(usize, usize)> {
        let client = reqwest::Client::new();
        let oi_url = "https://fapi.binance.com/futures/data/openInterestHist?symbol=BTCUSDT&period=5m&limit=500";
        let funding_url = "https://fapi.binance.com/fapi/v1/fundingRate?symbol=BTCUSDT&limit=100";
        let oi_body: serde_json::Value = client.get(oi_url).send().await?.json().await?;
        let funding_body: serde_json::Value = client.get(funding_url).send().await?.json().await?;
        let mut core = core.lock().expect("shadow core poisoned");
        let mut oi_n = 0usize;
        if let Some(rows) = oi_body.as_array() {
            for row in rows {
                let ts_ms = row.get("timestamp").and_then(|v| v.as_i64());
                let oi = row
                    .get("sumOpenInterest")
                    .and_then(parse_json_f64);
                if let (Some(ts), Some(oi)) = (ts_ms, oi) {
                    core.push_oi(ts, oi);
                    oi_n += 1;
                }
            }
        }
        let mut funding_n = 0usize;
        if let Some(rows) = funding_body.as_array() {
            for row in rows {
                let ts_ms = row
                    .get("fundingTime")
                    .and_then(|v| v.as_i64());
                let rate = row
                    .get("fundingRate")
                    .and_then(parse_json_f64);
                if let (Some(ts), Some(rate)) = (ts_ms, rate) {
                    core.push_funding(ts, rate);
                    funding_n += 1;
                }
            }
        }
        Ok((oi_n, funding_n))
    }

    fn parse_json_f64(v: &serde_json::Value) -> Option<f64> {
        match v {
            serde_json::Value::Number(n) => n.as_f64(),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    async fn warm_klines(core: &Core, url: &str, perp: bool) -> anyhow::Result<usize> {
        let body: serde_json::Value = reqwest::get(url).await?.json().await?;
        let arr = body
            .as_array()
            .context("binance klines response was not an array")?;
        let mut core = core.lock().expect("shadow core poisoned");
        let mut n = 0usize;
        for k in arr {
            let close_ms = k.get(6).and_then(serde_json::Value::as_i64);
            let close_px = k
                .get(4)
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse::<f64>().ok());
            if let (Some(ms), Some(px)) = (close_ms, close_px) {
                if perp {
                    core.push_perp(ms, ms, px, 0.0, false);
                } else {
                    core.push_spot(ms, ms, px, 0.0, false);
                }
                n += 1;
            }
        }
        Ok(n)
    }

    /// Backfill the spot buffer with ~`seconds` of 1s klines (Binance caps at
    /// 1000/req, so paginate by close-time). Each close is pushed on its
    /// close-time, giving the vol estimator a real 1s price path == what the
    /// live tick feed produces on the 1s sample grid.
    async fn warm_spot_1s(core: &Core, seconds: i64) -> anyhow::Result<usize> {
        let now_ms = super::now_unix_ms();
        let mut start = now_ms - seconds * 1000;
        let mut total = 0usize;
        let client = reqwest::Client::new();
        while start < now_ms {
            let url = format!(
                "https://api.binance.com/api/v3/klines?symbol=BTCUSDT&interval=1s&startTime={}&limit=1000",
                start
            );
            let body: serde_json::Value = client.get(&url).send().await?.json().await?;
            let arr = match body.as_array() {
                Some(a) if !a.is_empty() => a,
                _ => break,
            };
            let mut last_close = start;
            {
                let mut core_g = core.lock().expect("shadow core poisoned");
                for k in arr {
                    let close_ms = k.get(6).and_then(serde_json::Value::as_i64);
                    let close_px = k
                        .get(4)
                        .and_then(serde_json::Value::as_str)
                        .and_then(|s| s.parse::<f64>().ok());
                    if let (Some(ms), Some(px)) = (close_ms, close_px) {
                        core_g.push_spot(ms, ms, px, 0.0, false);
                        last_close = ms;
                        total += 1;
                    }
                }
            }
            if last_close <= start {
                break;
            }
            start = last_close + 1;
        }
        Ok(total)
    }
}

/// Live data feeds. Read-only consumers of public endpoints: the only
/// outbound payloads are websocket subscriptions and pings.
mod feeds {
    use super::{MarketId, MarketWindow, ShadowCore, now_unix_ms};
    use anyhow::{Context, Result};
    use futures::{SinkExt, StreamExt};
    use serde_json::Value;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::watch;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    // Raw trade stream (not aggTrade: aggregation adds publish delay).
    const BINANCE_WS_URL: &str = "wss://stream.binance.com:9443/ws/btcusdt@trade";
    /// Parallel Binance connections; first arrival wins, dedup by trade id.
    const BINANCE_CONNS: usize = 3;
    const PM_BOOK_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    const CRYPTO_PRICE_URL: &str = "https://polymarket.com/api/crypto/crypto-price";
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

    /// First-arrival dedup across the parallel connections, keyed by trade
    /// id. Bounded ring so memory stays flat.
    #[derive(Default)]
    pub struct TradeDedup {
        seen: std::collections::HashSet<i64>,
        order: std::collections::VecDeque<i64>,
    }

    impl TradeDedup {
        pub fn first_arrival(&mut self, trade_id: i64) -> bool {
            if !self.seen.insert(trade_id) {
                return false;
            }
            self.order.push_back(trade_id);
            if self.order.len() > 8192
                && let Some(old) = self.order.pop_front()
            {
                self.seen.remove(&old);
            }
            true
        }
    }

    pub async fn binance_spot_feed(core: Core, shutdown: watch::Receiver<bool>) {
        let dedup = Arc::new(Mutex::new(TradeDedup::default()));
        let conns: Vec<_> = (0..BINANCE_CONNS)
            .map(|idx| {
                let core = core.clone();
                let dedup = dedup.clone();
                let shutdown = shutdown.clone();
                tokio::spawn(binance_conn(idx, core, dedup, shutdown))
            })
            .collect();
        futures::future::join_all(conns).await;
    }

    async fn binance_conn(
        idx: usize,
        core: Core,
        dedup: Arc<Mutex<TradeDedup>>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut backoff = Duration::from_secs(1);
        while !*shutdown.borrow() {
            match binance_once(&core, &dedup, &mut shutdown).await {
                Ok(()) => break,
                Err(error) => {
                    tracing::warn!(?error, conn = idx, backoff_ms = backoff.as_millis() as u64,
                        "binance spot conn failed; reconnecting");
                    backoff_sleep(&mut backoff).await;
                }
            }
        }
    }

    async fn binance_once(
        core: &Core,
        dedup: &Arc<Mutex<TradeDedup>>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<()> {
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
                            if let Err(error) = handle_binance_text(core, dedup, &text) {
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

    fn handle_binance_text(core: &Core, dedup: &Arc<Mutex<TradeDedup>>, text: &str) -> Result<()> {
        let payload: Value = serde_json::from_str(text).context("decode binance payload")?;
        if payload.get("e").and_then(Value::as_str) != Some("trade") {
            return Ok(());
        }
        let trade_id = value_i64(payload.get("t")).context("trade missing id")?;
        let (Some(price), Some(qty)) = (
            value_f64(payload.get("p")),
            value_f64(payload.get("q")),
        ) else {
            anyhow::bail!("trade missing price/quantity");
        };
        // Exchange event time: the stream's T field (trade time, ms).
        let exchange_ms = value_i64(payload.get("T"))
            .or_else(|| value_i64(payload.get("E")))
            .context("trade missing T/E timestamp")?;
        let is_buyer_maker = payload.get("m").and_then(Value::as_bool).unwrap_or(false);
        let receipt_ms = now_unix_ms();
        if !dedup.lock().expect("dedup poisoned").first_arrival(trade_id) {
            return Ok(()); // a faster sibling connection already delivered it
        }
        core.lock()
            .expect("shadow core poisoned")
            .push_spot(exchange_ms, receipt_ms, price, qty, is_buyer_maker);
        Ok(())
    }

    // Binance futures (perp-led state input)

    const BINANCE_FUT_WS_URL: &str = "wss://fstream.binance.com/ws/btcusdt@aggTrade";

    pub async fn binance_perp_feed(core: Core, mut shutdown: watch::Receiver<bool>) {
        let mut backoff = Duration::from_secs(1);
        while !*shutdown.borrow() {
            match perp_once(&core, &mut shutdown).await {
                Ok(()) => break,
                Err(error) => {
                    tracing::warn!(?error, backoff_ms = backoff.as_millis() as u64,
                        "binance perp feed failed; reconnecting");
                    backoff_sleep(&mut backoff).await;
                }
            }
        }
    }

    async fn perp_once(core: &Core, shutdown: &mut watch::Receiver<bool>) -> Result<()> {
        let (stream, _) = connect_async(BINANCE_FUT_WS_URL)
            .await
            .context("connecting binance futures ws")?;
        tracing::info!("binance futures websocket connected");
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
                        anyhow::bail!("binance futures ws stale (no frames)");
                    }
                    write.send(Message::Ping(Vec::new().into())).await
                        .context("binance futures ping")?;
                }
                frame = read.next() => {
                    last_frame = tokio::time::Instant::now();
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            if let Err(error) = handle_perp_text(core, &text) {
                                tracing::warn!(?error, "skipping malformed futures message");
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            write.send(Message::Pong(payload)).await.ok();
                        }
                        Some(Ok(Message::Close(_))) => anyhow::bail!("futures ws closed by remote"),
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Err(error).context("futures ws frame error"),
                        None => anyhow::bail!("futures ws stream ended"),
                    }
                }
            }
        }
    }

    fn handle_perp_text(core: &Core, text: &str) -> Result<()> {
        let payload: Value = serde_json::from_str(text).context("decode futures payload")?;
        if payload.get("e").and_then(Value::as_str) != Some("aggTrade") {
            return Ok(());
        }
        let (Some(price), Some(qty)) = (
            value_f64(payload.get("p")),
            value_f64(payload.get("q")),
        ) else {
            anyhow::bail!("futures aggTrade missing price/quantity");
        };
        let exchange_ms = value_i64(payload.get("T"))
            .or_else(|| value_i64(payload.get("E")))
            .context("futures aggTrade missing T/E timestamp")?;
        let is_buyer_maker = payload.get("m").and_then(Value::as_bool).unwrap_or(false);
        core.lock()
            .expect("shadow core poisoned")
            .push_perp(exchange_ms, now_unix_ms(), price, qty, is_buyer_maker);
        Ok(())
    }

    pub async fn perp_metrics_poll(core: Core, mut shutdown: watch::Receiver<bool>) {
        let client = reqwest::Client::new();
        let mut oi_tick = tokio::time::interval(super::OI_POLL_INTERVAL);
        oi_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut funding_tick = tokio::time::interval(super::FUNDING_POLL_INTERVAL);
        funding_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First poll immediately so gaps after bootstrap are small.
        oi_tick.tick().await;
        funding_tick.tick().await;
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = oi_tick.tick() => {
                    if let Err(error) = poll_oi(&client, &core).await {
                        tracing::warn!(?error, "perp OI poll failed");
                    }
                }
                _ = funding_tick.tick() => {
                    if let Err(error) = poll_funding(&client, &core).await {
                        tracing::warn!(?error, "perp funding poll failed");
                    }
                }
            }
        }
    }

    async fn poll_oi(client: &reqwest::Client, core: &Core) -> Result<()> {
        let url = "https://fapi.binance.com/futures/data/openInterestHist?symbol=BTCUSDT&period=5m&limit=2";
        let body: Value = client.get(url).send().await?.json().await?;
        let rows = body.as_array().context("OI hist response not array")?;
        let mut core = core.lock().expect("shadow core poisoned");
        for row in rows {
            let ts_ms = value_i64(row.get("timestamp"));
            let oi = row.get("sumOpenInterest").and_then(parse_value_f64);
            if let (Some(ts), Some(oi)) = (ts_ms, oi) {
                core.push_oi(ts, oi);
            }
        }
        Ok(())
    }

    async fn poll_funding(client: &reqwest::Client, core: &Core) -> Result<()> {
        let url = "https://fapi.binance.com/fapi/v1/fundingRate?symbol=BTCUSDT&limit=5";
        let body: Value = client.get(url).send().await?.json().await?;
        let rows = body.as_array().context("funding response not array")?;
        let mut core = core.lock().expect("shadow core poisoned");
        for row in rows {
            let ts_ms = value_i64(row.get("fundingTime"));
            let rate = row.get("fundingRate").and_then(parse_value_f64);
            if let (Some(ts), Some(rate)) = (ts_ms, rate) {
                core.push_funding(ts, rate);
            }
        }
        Ok(())
    }

    fn parse_value_f64(v: &Value) -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }

    // Cross-venue measure-only feeds (Kraken, Coinbase)

    const KRAKEN_WS_URL: &str = "wss://ws.kraken.com/v2";
    const COINBASE_WS_URL: &str = "wss://ws-feed.exchange.coinbase.com";

    fn iso_ms(v: Option<&Value>) -> Option<i64> {
        let s = v?.as_str()?;
        chrono::DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|t| t.timestamp_millis())
    }

    pub async fn venue_feed(
        venue: &'static str,
        core: Core,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut backoff = Duration::from_secs(1);
        while !*shutdown.borrow() {
            match venue_once(venue, &core, &mut shutdown).await {
                Ok(()) => break,
                Err(error) => {
                    tracing::warn!(?error, venue, backoff_ms = backoff.as_millis() as u64,
                        "venue feed failed; reconnecting");
                    backoff_sleep(&mut backoff).await;
                }
            }
        }
    }

    async fn venue_once(
        venue: &'static str,
        core: &Core,
        shutdown: &mut watch::Receiver<bool>,
    ) -> Result<()> {
        let (url, subscribe) = match venue {
            "kraken" => (
                KRAKEN_WS_URL,
                serde_json::json!({
                    "method": "subscribe",
                    "params": {"channel": "trade", "symbol": ["BTC/USD"]}
                }),
            ),
            "coinbase" => (
                COINBASE_WS_URL,
                serde_json::json!({
                    "type": "subscribe",
                    "product_ids": ["BTC-USD"],
                    "channels": ["matches"]
                }),
            ),
            other => anyhow::bail!("unknown venue {other}"),
        };
        let (stream, _) = connect_async(url)
            .await
            .with_context(|| format!("connecting {venue} ws"))?;
        tracing::info!(venue, "venue websocket connected");
        let (mut write, mut read) = stream.split();
        write
            .send(Message::Text(subscribe.to_string().into()))
            .await
            .with_context(|| format!("{venue} subscribe"))?;
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
                        anyhow::bail!("{venue} ws stale (no frames)");
                    }
                    write.send(Message::Ping(Vec::new().into())).await
                        .with_context(|| format!("{venue} ping"))?;
                }
                frame = read.next() => {
                    last_frame = tokio::time::Instant::now();
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            if let Err(error) = handle_venue_text(venue, core, &text) {
                                tracing::warn!(?error, venue, "skipping malformed venue message");
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            write.send(Message::Pong(payload)).await.ok();
                        }
                        Some(Ok(Message::Close(_))) => anyhow::bail!("{venue} ws closed by remote"),
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Err(error).context("venue ws frame error"),
                        None => anyhow::bail!("{venue} ws stream ended"),
                    }
                }
            }
        }
    }

    fn handle_venue_text(venue: &'static str, core: &Core, text: &str) -> Result<()> {
        let payload: Value = serde_json::from_str(text).context("decode venue payload")?;
        let receipt_ms = now_unix_ms();
        match venue {
            "kraken" => {
                if payload.get("channel").and_then(Value::as_str) != Some("trade") {
                    return Ok(()); // status/heartbeat/subscription acks
                }
                let Some(data) = payload.get("data").and_then(Value::as_array) else {
                    return Ok(());
                };
                let mut core = core.lock().expect("shadow core poisoned");
                for t in data {
                    if let Some(price) = value_f64(t.get("price")) {
                        core.push_venue(venue, receipt_ms, price, iso_ms(t.get("timestamp")));
                    }
                }
            }
            "coinbase" => {
                let kind = payload.get("type").and_then(Value::as_str);
                if kind != Some("match") && kind != Some("last_match") {
                    return Ok(());
                }
                if let Some(price) = value_f64(payload.get("price")) {
                    core.lock()
                        .expect("shadow core poisoned")
                        .push_venue(venue, receipt_ms, price, iso_ms(payload.get("time")));
                }
            }
            _ => {}
        }
        Ok(())
    }

    // Resolution marker

    /// Settle watched entries against the official crypto-price API once
    /// their windows complete; emits Resolution events over the channel.
    pub async fn resolution_marker_feed(
        core: Core,
        events: tokio::sync::mpsc::UnboundedSender<super::LogEvent>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let client = reqwest::Client::new();
        let mut tick = tokio::time::interval(Duration::from_secs(20));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.changed() => return,
                _ = tick.tick() => {}
            }
            let now_ns = super::now_unix_ns();
            let due = core.lock().expect("shadow core poisoned").resolutions_due(now_ns);
            for w in due {
                match fetch_resolution(&client, &w).await {
                    Ok(Some(won)) => {
                        let ev = core
                            .lock()
                            .expect("shadow core poisoned")
                            .apply_resolution(w.entry_id, won, super::now_unix_ns());
                        if let Some(ev) = ev {
                            let _ = events.send(ev);
                        }
                    }
                    Ok(None) => {} // not completed yet; the watch retries
                    Err(error) => {
                        tracing::warn!(?error, slug = %w.slug, "resolution lookup failed")
                    }
                }
            }
        }
    }

    async fn fetch_resolution(
        client: &reqwest::Client,
        w: &super::ResolutionWatch,
    ) -> Result<Option<bool>> {
        let secs = w.close_ts_s - w.open_ts_s;
        let variant = match secs {
            300 => "fiveminute",
            900 => "fifteen",
            3600 => "hourly",
            14400 => "fourhour",
            _ => anyhow::bail!("unsupported window {secs}s"),
        };
        let symbol = w.slug.split('-').next().unwrap_or("btc").to_uppercase();
        let url = format!(
            "{CRYPTO_PRICE_URL}?symbol={symbol}&eventStartTime={}&variant={variant}&endDate={}",
            w.open_ts_s, w.close_ts_s
        );
        let v: Value = client
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if !v.get("completed").and_then(Value::as_bool).unwrap_or(false) {
            return Ok(None);
        }
        let (Some(open), Some(close)) = (value_f64(v.get("openPrice")), value_f64(v.get("closePrice")))
        else {
            return Ok(None);
        };
        let up_won = close > open;
        Ok(Some(match w.side {
            super::Side::Up => up_won,
            super::Side::Down => !up_won,
        }))
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
                    let win = window_secs_from_slug(&slug_prefix);
                    let current_open = now_s - now_s.rem_euclid(win);
                    for open_ts in [current_open, current_open + win] {
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

    /// Window length from an updown slug ("btc-updown-5m-<open>" -> 300).
    /// Matches the harness's slug-derived duration semantics.
    pub(super) fn window_secs_from_slug(slug: &str) -> i64 {
        for (tag, secs) in [("-5m-", 300), ("-15m-", 900), ("-1h-", 3600), ("-4h-", 14400)] {
            if slug.contains(tag) {
                return secs;
            }
        }
        300
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
        let up_index_set: u64 = if up_token == tokens[0] { 1 } else { 2 };
        let down_index_set: u64 = if down_token == tokens[0] { 1 } else { 2 };
        let condition_id = item
            .get("conditionId")
            .or_else(|| item.get("condition_id"))
            .and_then(Value::as_str)
            .map(str::to_string);
        Some(MarketWindow {
            slug: slug.to_string(),
            open_ts_s,
            close_ts_s: open_ts_s + window_secs_from_slug(slug),
            up_token,
            down_token,
            up_index_set,
            down_index_set,
            condition_id,
            gamma_strike,
            entered: false,
            n_clips: 0,
            pending_commit: false,
            belief_flip_ns: None,
            belief_up: None,
            // Overwritten with a real id by `ShadowCore::upsert_market`.
            id: MarketId(0),
            yes_mid_lo: f32::INFINITY,
            yes_mid_hi: f32::NEG_INFINITY,
            events_seen: 0,
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
    use pm_strategy::{OrderRequest, StrategyOutput};
    use std::collections::HashSet;

    const NS: i64 = 1_000_000_000;

    /// Engine-test strategy: a fixed buy per market, so the tests exercise the
    /// twin's entry/probe/exit/settle machinery rather than any decision rule.
    /// The shipped `NoopStrategy` would make every one of these tests vacuous.
    struct StubEntry {
        side: StratSide,
        shares: f64,
        limit_price: Option<f32>,
        once: bool,
        seen: HashSet<u32>,
    }

    impl StubEntry {
        fn buy_yes() -> Box<dyn Strategy + Send> {
            Box::new(Self {
                side: StratSide::BuyYes,
                shares: 100.0,
                limit_price: None,
                once: true,
                seen: HashSet::new(),
            })
        }
        fn buy_no_limited(limit_price: f32) -> Box<dyn Strategy + Send> {
            Box::new(Self {
                side: StratSide::BuyNo,
                shares: 100.0,
                limit_price: Some(limit_price),
                once: true,
                seen: HashSet::new(),
            })
        }
        fn sell_yes() -> Box<dyn Strategy + Send> {
            Box::new(Self {
                side: StratSide::SellYes,
                shares: 100.0,
                limit_price: None,
                once: true,
                seen: HashSet::new(),
            })
        }
        fn buy_yes_every_pass() -> Box<dyn Strategy + Send> {
            Box::new(Self {
                side: StratSide::BuyYes,
                shares: 100.0,
                limit_price: None,
                once: false,
                seen: HashSet::new(),
            })
        }
    }

    impl Strategy for StubEntry {
        fn on_event(
            &mut self,
            event: &ReplayEvent,
            _ctx: &Ctx,
            _spot: &SpotHistory,
            _trades: &TradeHistory,
        ) -> StrategyOutput {
            if self.once && !self.seen.insert(event.market_id.0) {
                return StrategyOutput::hold();
            }
            StrategyOutput::one(OrderRequest {
                side: self.side,
                shares: self.shares,
                max_depth: 5,
                limit_price: self.limit_price,
                tag: "stub_entry",
            })
        }
    }

    /// Records every `Ctx` it is handed, for the context-plumbing test.
    #[derive(Default)]
    struct CtxRecorder {
        seen: std::sync::Arc<std::sync::Mutex<Vec<(u32, Ctx)>>>,
    }

    impl Strategy for CtxRecorder {
        fn on_event(
            &mut self,
            event: &ReplayEvent,
            ctx: &Ctx,
            _spot: &SpotHistory,
            _trades: &TradeHistory,
        ) -> StrategyOutput {
            self.seen
                .lock()
                .expect("recorder poisoned")
                .push((event.market_id.0, *ctx));
            StrategyOutput::hold()
        }
    }

    #[test]
    fn p_side_for_entry_reads_the_complement_on_down() {
        // Live log shape: ENTER DOWN p_up=0.319 touch=0.51.
        assert!((p_side_for_entry(0.319, "down") - 0.681).abs() < 1e-9);
        assert!((p_side_for_entry(0.319, "up") - 0.319).abs() < 1e-9);
    }

    #[test]
    fn default_shadow_args_carry_no_strategy_parameters() {
        let args = default_shadow_args(PathBuf::from("shadow-final"));
        assert_eq!(args.strategy, "noop");
        assert_eq!(args.vol_lookback_s, 3600);
        assert_eq!(args.exit_after_s, 0);
        assert_eq!(args.latency_probe_ms, 150);
        assert!((args.perp_price_weight - 0.75).abs() < f64::EPSILON);
        assert_eq!(args.vol_estimator, "realized");
        assert_eq!(args.decide_interval_ms, 1000);
        assert!(!args.decide_on_event);
        // The config the engine actually runs is these values and nothing
        // else: no threshold, no gate, no clip size to drift against a
        // strategy's own config.
        let cfg = shadow_config_from_args(&args);
        let json = serde_json::to_value(&cfg).unwrap();
        let mut keys: Vec<&str> = json.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "exit_after_s",
                "latency_probe_ms",
                "perp_price_weight",
                "strategy",
                "vol_estimator",
                "vol_lookback_s",
            ]
        );
    }

    #[test]
    fn strategy_from_name_accepts_noop_and_refuses_everything_else() {
        assert!(strategy_from_name("noop").is_ok());
        let err = match strategy_from_name("fixture") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("fixture must not be drivable live"),
        };
        assert!(err.contains("fixture"), "{err}");
        assert!(err.contains("noop"), "{err}");
        assert!(matches!(strategy_from_name("exo_fade"), Err(_)));
    }

    #[test]
    fn ewma_args_resolve_to_the_ewma_estimator() {
        let mut args = default_shadow_args(PathBuf::from("out"));
        args.vol_estimator = "ewma".to_string();
        args.ewma_halflife_s = 900.0;
        match shadow_config_from_args(&args).vol_estimator {
            VolEstimator::Ewma { halflife_s } => assert_eq!(halflife_s, 900.0),
            other => panic!("expected ewma, got {other:?}"),
        }
    }

    #[test]
    fn should_decide_trigger_predicate() {
        let s = 1_000_000_000i64; // 1s in ns
        let ms = 1_000_000i64;

        // Cadence-due fires in either mode, dirty or clean.
        assert!(should_decide(10 * s, 10 * s, false, false, i64::MIN));
        assert!(should_decide(10 * s, 10 * s, false, true, i64::MIN));
        assert!(should_decide(10 * s, 9 * s, true, false, 9 * s));
        assert!(should_decide(10 * s, 9 * s, true, true, 9 * s));

        // Dirty + spacing met fires only in event mode.
        let last = 10 * s;
        let now = last + 20 * ms; // exactly at the 20ms floor
        assert!(should_decide(now, 11 * s, true, true, last));
        assert!(!should_decide(now, 11 * s, true, false, last));

        // Dirty but spacing not met does not fire.
        let too_soon = last + 19 * ms;
        assert!(!should_decide(too_soon, 11 * s, true, true, last));

        // Clean and idle does not fire before the heartbeat, either mode.
        assert!(!should_decide(now, 11 * s, false, true, last));
        assert!(!should_decide(now, 11 * s, false, false, last));
    }

    #[test]
    fn decide_interval_ns_clamps_to_poll_floor() {
        assert_eq!(decide_interval_ns(0), 20_000_000);
        assert_eq!(decide_interval_ns(10), 20_000_000);
        assert_eq!(decide_interval_ns(20), 20_000_000);
        assert_eq!(decide_interval_ns(100), 100_000_000);
        assert_eq!(decide_interval_ns(1000), 1_000_000_000);
    }

    #[test]
    fn config_event_serializes_to_contract_shape() {
        let args = default_shadow_args(PathBuf::from("shadow-final"));
        let cfg = shadow_config_from_args(&args);
        let event = LogEvent::Config {
            ts_utc: "2026-07-10T00:00:00.000Z".to_string(),
            slug_prefix: args.slug_prefix.clone(),
            out_dir: args.out_dir.display().to_string(),
            strategy: args.strategy.clone(),
            decide_interval_ms: args.decide_interval_ms,
            decide_on_event: args.decide_on_event,
            perp_price_weight: args.perp_price_weight,
            vol_lookback_s: args.vol_lookback_s,
            vol_estimator: args.vol_estimator.clone(),
            config_fingerprint: config_fingerprint(&cfg),
        };
        let line = serde_json::to_string(&event).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["type"], "config");
        assert_eq!(v["slug_prefix"], "btc-updown-5m-");
        assert_eq!(v["out_dir"], "shadow-final");
        assert_eq!(v["strategy"], "noop");
        assert_eq!(v["decide_interval_ms"], 1000);
        assert_eq!(v["decide_on_event"], false);
        // Plain name, not a Debug-quoted one: readers match on it.
        assert_eq!(v["vol_estimator"], "realized");
        assert_eq!(v["config_fingerprint"].as_str().unwrap().len(), 16);
    }

    #[test]
    fn config_fingerprint_changes_when_the_strategy_changes() {
        let args = default_shadow_args(PathBuf::from("out"));
        let base = config_fingerprint(&shadow_config_from_args(&args));
        let mut other = args.clone();
        other.strategy = "some_future_strategy".to_string();
        assert_ne!(base, config_fingerprint(&shadow_config_from_args(&other)));
    }

    fn cfg() -> ShadowConfig {
        ShadowConfig {
            strategy: "stub".to_string(),
            vol_lookback_s: 1800,
            exit_after_s: 30,
            latency_probe_ms: 150,
            perp_price_weight: 0.0,
            vol_estimator: VolEstimator::Realized,
        }
    }

    /// Wavy spot tape: 0..2000s around 100k, enough history for the vol
    /// estimator. Receipt is exchange + 25ms so the median delta is known.
    fn core_with_spot() -> ShadowCore {
        let mut core = ShadowCore::new(cfg(), StubEntry::buy_yes());
        let mut price = 100_000.0;
        for s in 0..2000i64 {
            core.push_spot(s * 1_000, s * 1_000 + 25, price, 1.0, false);
            price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
        }
        core
    }

    /// Tape that plateaus at `pre_open` through the 1800s open (so the
    /// Binance-proxy strike is exactly `pre_open`), then trades wavy ~100k.
    fn core_with_spot_strike(pre_open: f64) -> ShadowCore {
        core_with_spot_strike_driven_by(pre_open, StubEntry::buy_yes())
    }

    fn core_with_spot_strike_driven_by(
        pre_open: f64,
        strategy: Box<dyn Strategy + Send>,
    ) -> ShadowCore {
        let mut core = ShadowCore::new(cfg(), strategy);
        let mut price = 100_000.0;
        for s in 0..2000i64 {
            if s <= 1800 {
                core.push_spot(s * 1_000, s * 1_000 + 25, pre_open, 1.0, false);
            } else {
                core.push_spot(s * 1_000, s * 1_000 + 25, price, 1.0, false);
                price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
            }
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
            up_index_set: 1,
            down_index_set: 2,
            condition_id: None,
            gamma_strike: strike,
            entered: false,
            n_clips: 0,
            pending_commit: false,
            belief_flip_ns: None,
            belief_up: None,
            id: MarketId(0),
            yes_mid_lo: f32::INFINITY,
            yes_mid_hi: f32::NEG_INFINITY,
            events_seen: 0,
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
        let mut core = ShadowCore::new(cfg(), StubEntry::buy_yes());
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
    fn tape_levels_are_best_first_and_zero_padded() {
        let mut core = ShadowCore::new(cfg(), StubEntry::buy_yes());
        core.apply_book_snapshot(
            "tok",
            &[(0.40, 10.0), (0.42, 5.0)],
            &[(0.50, 7.0), (0.55, 9.0)],
            Some(1_000),
            1_030,
        );
        let (bids, asks) = core.books.get("tok").unwrap().tape_levels();
        assert_eq!(bids[0].price, 0.42);
        assert_eq!(bids[1].price, 0.40);
        assert_eq!(bids[2], pm_types::BookLevel::default());
        assert_eq!(asks[0].price, 0.50);
        assert_eq!(asks[0].size, 7.0);
        assert_eq!(asks[1].price, 0.55);
        assert_eq!(asks[2], pm_types::BookLevel::default());
    }

    #[test]
    fn noop_strategy_produces_an_entry_free_stream() {
        let mut core = ShadowCore::new(cfg(), Box::new(NoopStrategy));
        let mut price = 100_000.0;
        for s in 0..2000i64 {
            core.push_spot(s * 1_000, s * 1_000 + 25, price, 1.0, false);
            price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
        }
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.10, 0.95);
        // Wide-open dislocation, full warm buffer, inside the window: the only
        // reason nothing enters is that the strategy asks for nothing.
        assert!(core.decide(1900 * NS, false).is_empty());
        assert!(core.poll_due(1901 * NS).is_empty());
        assert_eq!(core.stats.entries_total, 0);
        // The stream still carries a well-formed summary.
        match core.summary(1901 * NS, 1_901_000) {
            LogEvent::Summary { n_active_markets, n_entries_total, .. } => {
                assert_eq!(n_active_markets, 1);
                assert_eq!(n_entries_total, 0);
            }
            other => panic!("expected Summary, got {other:?}"),
        }
    }

    #[test]
    fn sell_orders_are_ignored() {
        let mut core = core_with_spot_strike_driven_by(99_000.0, StubEntry::sell_yes());
        core.upsert_market(market(None));
        set_books(&mut core, 0.50, 0.50);
        assert!(core.decide(1900 * NS, false).is_empty(), "the twin holds no position to sell");
        assert_eq!(core.stats.entries_total, 0);
        assert!(core.pending.is_empty());
    }

    #[test]
    fn entry_from_a_buy_yes_order_carries_the_belief_telemetry() {
        let mut core = core_with_spot_strike(99_000.0); // proxy strike far below: p_up ~ 1
        core.upsert_market(market(None));
        set_books(&mut core, 0.50, 0.50);

        let events = core.decide(1900 * NS, false);
        assert_eq!(events.len(), 1);
        match &events[0] {
            LogEvent::WouldEnter {
                side,
                edge,
                edge_up,
                edge_down,
                p_exo,
                p_down,
                strike,
                strike_source,
                touch_price,
                touch_size,
                lane,
                clip,
                target_notional,
                marketable_limit_price,
                ..
            } => {
                assert_eq!(*side, "up");
                assert_eq!(*strike, 99_000.0);
                assert_eq!(*strike_source, "binance_proxy");
                assert_eq!(*touch_price, 0.50);
                assert_eq!(*touch_size, 50.0);
                assert!(*p_exo > 0.9, "p_exo={p_exo}");
                assert!(*edge > 0.16);
                assert!((*p_down - (1.0 - p_exo)).abs() < 1e-9);
                assert!((*edge_up - *edge).abs() < 1e-9);
                assert!(*edge_down < *edge_up);
                // The order's own attribution and sizing reach the record.
                assert_eq!(*lane, "stub_entry");
                assert_eq!(*clip, 1);
                assert!((*target_notional - 50.0).abs() < 1e-9, "100 sh @ 0.50");
                // Market order: marketable at the touch by definition.
                assert_eq!(*marketable_limit_price, 0.50);
            }
            other => panic!("expected WouldEnter, got {other:?}"),
        }
        if let LogEvent::WouldEnter {
            model_book_gap,
            secs_from_open,
            delta_bps,
            exo_features,
            dir_features,
            spot_ret_300s_bps,
            ..
        } = &events[0]
        {
            assert!(*model_book_gap > 0.35);
            assert_eq!(*secs_from_open, 100);
            assert!(*delta_bps > 0.0);
            assert_eq!(exo_features.len(), EXO_FEATURES);
            assert_eq!(dir_features.len(), DIR_FEATURES);
            assert!(spot_ret_300s_bps.is_some());
        } else {
            panic!("expected WouldEnter");
        }
        // The stub enters once per market; later passes add nothing.
        assert!(core.decide(1901 * NS, false).is_empty());
        assert_eq!(core.stats.entries_total, 1);
    }

    #[test]
    fn buy_no_prices_against_the_down_book_and_inverts_the_limit() {
        // limit 0.30 in YES terms is a 0.70 cap on the NO leg.
        let mut core =
            core_with_spot_strike_driven_by(101_000.0, StubEntry::buy_no_limited(0.30));
        core.upsert_market(market(None));
        set_books(&mut core, 0.50, 0.50);

        let events = core.decide(1900 * NS, false);
        assert_eq!(events.len(), 1);
        match &events[0] {
            LogEvent::WouldEnter {
                side,
                p_exo,
                p_side,
                marketable_limit_price,
                touch_price,
                token_id,
                ..
            } => {
                assert_eq!(*side, "down");
                assert_eq!(token_id, "down-tok");
                assert!(*p_exo < 0.1, "p_exo={p_exo}");
                assert!((*p_side - (1.0 - p_exo)).abs() < 1e-9);
                assert!((*marketable_limit_price - 0.70).abs() < 1e-6);
                assert!(*marketable_limit_price > *touch_price);
            }
            other => panic!("expected WouldEnter, got {other:?}"),
        }
    }

    #[test]
    fn repeating_strategy_ladders_clips_without_an_engine_cap() {
        let mut core =
            core_with_spot_strike_driven_by(99_000.0, StubEntry::buy_yes_every_pass());
        core.upsert_market(market(None));
        set_books(&mut core, 0.50, 0.50);
        for expected_clip in 1..=3u32 {
            let events = core.decide((1900 + expected_clip as i64) * NS, false);
            assert_eq!(events.len(), 1);
            match &events[0] {
                LogEvent::WouldEnter { clip, .. } => assert_eq!(*clip, expected_clip),
                other => panic!("expected WouldEnter, got {other:?}"),
            }
        }
        assert_eq!(core.stats.entries_total, 3);
        assert_eq!(core.markets["btc-updown-5m-1800"].n_clips, 3);
    }

    #[test]
    fn deferred_commit_holds_the_market_until_the_fill_confirms() {
        let mut core =
            core_with_spot_strike_driven_by(99_000.0, StubEntry::buy_yes_every_pass());
        core.upsert_market(market(None));
        set_books(&mut core, 0.50, 0.50);
        assert_eq!(core.decide(1900 * NS, true).len(), 1);
        let m = &core.markets["btc-updown-5m-1800"];
        assert!(m.pending_commit && !m.entered && m.n_clips == 0);
        // A pending entry blocks further passes even for an eager strategy.
        assert!(core.decide(1901 * NS, true).is_empty());
        // A miss keeps the commit pending for the next submit attempt.
        core.commit_entry("btc-updown-5m-1800", false);
        assert!(core.markets["btc-updown-5m-1800"].pending_commit);
        core.commit_entry("btc-updown-5m-1800", true);
        let m = &core.markets["btc-updown-5m-1800"];
        assert!(!m.pending_commit && m.entered && m.n_clips == 1);
    }

    #[test]
    fn ctx_and_event_carry_per_market_state() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut core = core_with_spot_strike_driven_by(
            99_000.0,
            Box::new(CtxRecorder { seen: seen.clone() }),
        );
        core.upsert_market(market(None));
        set_books(&mut core, 0.50, 0.50);
        core.decide(1900 * NS, false);
        // Move the YES book so the observed mid range actually widens.
        core.apply_book_snapshot(
            "up-tok",
            &[(0.60, 10.0)],
            &[(0.64, 10.0)],
            Some(1_900_500),
            1_900_540,
        );
        core.decide(1901 * NS, false);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, seen[1].0, "same market keeps one id");
        assert_eq!(seen[0].1.events_seen, 1);
        assert_eq!(seen[1].1.events_seen, 2);
        assert_eq!(seen[0].1.cash_usdc, 0.0, "the twin holds no capital");
        assert_eq!(seen[0].1.market_close_ns, 2100 * NS);
        // First pass sees a single mid, the second a widened range.
        assert!(seen[0].1.market_yes_range_so_far.abs() < 1e-6);
        assert!(seen[1].1.market_yes_range_so_far > 0.05);
    }

    #[test]
    fn market_ids_are_assigned_by_the_core_and_unique() {
        let mut core = core_with_spot();
        core.upsert_market(market(None));
        let mut second = market(None);
        second.slug = "btc-updown-5m-2100".to_string();
        second.id = MarketId(0);
        core.upsert_market(second);
        let a = core.markets["btc-updown-5m-1800"].id;
        let b = core.markets["btc-updown-5m-2100"].id;
        assert_ne!(a, b);
        // Re-discovery of a known slug keeps the id it was given.
        core.upsert_market(market(Some(100_123.0)));
        assert_eq!(core.markets["btc-updown-5m-1800"].id, a);
    }

    #[test]
    fn no_entry_without_strike_and_proxy_used_when_spot_covers_open() {
        // Spot history starting AFTER the open: no proxy strike available.
        let mut late = ShadowCore::new(cfg(), StubEntry::buy_yes());
        let mut price = 100_000.0;
        for s in 1850..3900i64 {
            late.push_spot(s * 1_000, s * 1_000 + 25, price, 1.0, false);
            price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
        }
        late.upsert_market(market(None));
        set_books(&mut late, 0.10, 0.95);
        assert!(late.decide(3000 * NS, false).is_empty(), "no strike -> stand down");

        // Full history: proxy = last Binance trade at-or-before open.
        let mut core = core_with_spot();
        core.upsert_market(market(None));
        set_books(&mut core, 0.10, 0.95);
        let events = core.decide(1900 * NS, false);
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
        let mut core = core_with_spot_strike(99_000.0);
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        let entry_ns = 1900 * NS;
        assert_eq!(core.decide(entry_ns, false).len(), 1);

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
        // The passive-exit probe keeps the trade pending until it resolves.
        assert_eq!(core.pending.len(), 1);
        // A bid crossing the resting mid level fills the probe fee-free.
        core.apply_book_snapshot(
            "up-tok",
            &[(0.58, 20.0)],
            &[(0.60, 10.0)],
            Some(1_932_000),
            1_932_020,
        );
        let passive = core.poll_due(entry_ns + 32 * NS);
        assert_eq!(passive.len(), 1);
        match &passive[0] {
            LogEvent::PassiveExit { filled, level, pnl_usd, .. } => {
                assert!(*filled);
                assert!((*level - 0.575).abs() < 1e-9);
                // Laddered entry was 0.60; the mid level sits below it.
                assert!(pnl_usd.unwrap() < 0.0);
            }
            other => panic!("expected PassiveExit, got {other:?}"),
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
        let mut core = core_with_spot_strike(99_000.0);
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        let entry_ns = 1900 * NS;
        assert_eq!(core.decide(entry_ns, false).len(), 1);
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
        // Long exit horizon so a deadline-legal entry still lands past close.
        let mut core = core_with_spot_strike(99_000.0);
        core.cfg.exit_after_s = 120;
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        // Entry 91s before close (inside the 90s deadline); exit_after=120s
        // would land 29s past close and must clamp to close.
        let entry_ns = 2009 * NS;
        assert_eq!(core.decide(entry_ns, false).len(), 1);
        assert!(core.poll_due(2099 * NS).iter().all(|e| matches!(e, LogEvent::QuoteProbe { .. })));
        let exit = core.poll_due(2100 * NS);
        assert_eq!(exit.len(), 2);
        assert!(matches!(exit[0], LogEvent::WouldExit { .. }));
        // At close the passive probe resolves in the same poll.
        assert!(matches!(exit[1], LogEvent::PassiveExit { .. }));
    }

    #[test]
    fn resolution_watch_settles_and_clears() {
        let mut core = core_with_spot_strike(99_000.0);
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        assert_eq!(core.decide(1900 * NS, false).len(), 1);
        // Not due before close + 15s grace.
        assert!(core.resolutions_due(2100 * NS).is_empty());
        let due = core.resolutions_due(2116 * NS);
        assert_eq!(due.len(), 1);
        let w = &due[0];
        // A losing outcome settles at -entry; the watch is consumed.
        let ev = core.apply_resolution(w.entry_id, false, 2200 * NS).unwrap();
        match ev {
            LogEvent::Resolution { won, settle_pnl_per_share, .. } => {
                assert!(!won);
                assert!((settle_pnl_per_share - -w.entry_touch_price).abs() < 1e-12);
            }
            other => panic!("expected Resolution, got {other:?}"),
        }
        assert!(core.apply_resolution(w.entry_id, false, 2300 * NS).is_none());
        assert!(core.resolutions_due(2400 * NS).is_empty());
    }

    #[test]
    fn resolution_watch_retries_then_expires() {
        let mut core = core_with_spot_strike(99_000.0);
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        assert_eq!(core.decide(1900 * NS, false).len(), 1);
        let mut t = 2116 * NS;
        for _ in 0..10 {
            assert_eq!(core.resolutions_due(t).len(), 1);
            t += 61 * NS;
        }
        assert!(core.resolutions_due(t).is_empty(), "watch expires after 10 attempts");
    }

    #[test]
    fn ladder_fill_round_trip_accounting() {
        let mut core = core_with_spot_strike(99_000.0);
        core.upsert_market(market(None));
        // 100 sh @ 0.50 asked = $50 notional: 50 sh fill at 0.50 ($25), the
        // remaining $25 at 0.52 = 48.08 sh. Bids hold 60 sh @0.48.
        core.apply_book_snapshot(
            "up-tok",
            &[(0.48, 60.0)],
            &[(0.50, 50.0), (0.52, 200.0)],
            Some(1_899_000),
            1_899_040,
        );
        core.apply_book_snapshot("down-tok", &[(0.40, 60.0)], &[(0.50, 70.0)], Some(1_899_000), 1_899_040);
        let entry_ns = 1900 * NS;
        assert_eq!(core.decide(entry_ns, false).len(), 1);

        let probe = core.poll_due(entry_ns + 150_000_000);
        let (avg, shares) = match &probe[0] {
            LogEvent::QuoteProbe { ladder_avg_price, ladder_shares, .. } => {
                (ladder_avg_price.unwrap(), ladder_shares.unwrap())
            }
            other => panic!("expected QuoteProbe, got {other:?}"),
        };
        let want_shares = 50.0 + 25.0 / 0.52;
        assert!((shares - want_shares).abs() < 1e-9, "{shares} vs {want_shares}");
        assert!((avg - 50.0 / want_shares).abs() < 1e-9);

        // Exit: only 60 sh sellable at 0.48; remainder rides to resolution.
        let exit = core.poll_due(entry_ns + 30 * NS);
        match &exit[0] {
            LogEvent::WouldExit { ladder_exit_avg, ladder_shares_sold, ladder_mark_pnl_usd, .. } => {
                assert!((ladder_exit_avg.unwrap() - 0.48).abs() < 1e-9);
                assert_eq!(ladder_shares_sold.unwrap(), 60.0);
                let want = 60.0 * (ladder_exit_avg.unwrap() - avg);
                assert!((ladder_mark_pnl_usd.unwrap() - want).abs() < 1e-9);
            }
            other => panic!("expected WouldExit, got {other:?}"),
        }

        // Resolution: unsold shares settle at 1-avg on a win.
        let due = core.resolutions_due(2116 * NS);
        let w = &due[0];
        let unsold = shares - 60.0;
        match core.apply_resolution(w.entry_id, true, 2200 * NS).unwrap() {
            LogEvent::Resolution { ladder_settle_pnl_usd, .. } => {
                let want = unsold * (1.0 - avg);
                assert!((ladder_settle_pnl_usd.unwrap() - want).abs() < 1e-9);
            }
            other => panic!("expected Resolution, got {other:?}"),
        }
    }

    #[test]
    fn lead_lag_detects_known_shift() {
        // Same pseudo-random walk on both venues, the "fast" one printed
        // 300ms earlier on the receipt clock; estimator must recover +300ms.
        let mut fast = VenueBuf::default();
        let mut slow = VenueBuf::default();
        let mut price = 100_000.0f64;
        let mut x = 0x2545F4914F6CDD1Du64;
        let now_ms = 10_000_000i64;
        for k in 0..3000i64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let step = ((x % 2001) as f64 - 1000.0) / 50.0;
            price += step;
            let t = now_ms - 600_000 + k * 200;
            fast.push(t, price, None);
            slow.push(t + 300, price, None);
        }
        let (lead, corr, _) = lead_lag_ms(&fast, &slow, now_ms).unwrap();
        assert_eq!(lead, 300, "expected +300ms lead, got {lead} (corr {corr})");
        assert!(corr > 0.8, "shifted identical walks should correlate: {corr}");
    }

    #[test]
    fn lead_lag_none_without_signal() {
        let empty = VenueBuf::default();
        let mut flat = VenueBuf::default();
        for k in 0..100 {
            flat.push(9_400_000 + k * 1000, 100.0, None);
        }
        assert!(lead_lag_ms(&empty, &flat, 10_000_000).is_none());
        assert!(lead_lag_ms(&flat, &empty, 10_000_000).is_none());
    }

    #[test]
    fn trade_dedup_first_arrival_only() {
        let mut d = super::feeds::TradeDedup::default();
        assert!(d.first_arrival(1));
        assert!(!d.first_arrival(1));
        assert!(d.first_arrival(2));
        for id in 100..9000 {
            d.first_arrival(id);
        }
        // Ring evicted id=1; re-arrival counts as new (acceptable: trade ids
        // this stale never race between live connections).
        assert!(d.first_arrival(1));
    }

    /// Mirrors `bootstrap::warm_klines`: 70×1m candles spanning ~69 minutes.
    fn push_klines_bootstrap(core: &mut ShadowCore, count: usize, span_s: i64, end_ms: i64) {
        let start_ms = end_ms - span_s * 1_000;
        let step_ms = if count > 1 {
            (end_ms - start_ms) / (count as i64 - 1)
        } else {
            0
        };
        let mut price = 99_000.0;
        for i in 0..count {
            let ms = start_ms + step_ms * i as i64;
            price *= if i % 2 == 0 { 1.0001 } else { 0.9999 };
            core.push_spot(ms, ms + 25, price, 1.0, false);
        }
    }

    fn spot_buffer_span_s(core: &ShadowCore) -> i64 {
        match (core.spot.front(), core.spot.back()) {
            (Some(first), Some(last)) => (last.ts_ns - first.ts_ns) / NS,
            _ => 0,
        }
    }

    fn vol_warmup_cleared(core: &ShadowCore, vol_lookback_s: u32) -> bool {
        spot_buffer_span_s(core) >= vol_lookback_s as i64
    }

    #[test]
    fn klines_bootstrap_clears_vol3600_warmup_immediately() {
        let args = default_shadow_args(PathBuf::from("shadow-final"));
        assert_eq!(args.vol_lookback_s, 3600);

        // Partial post-restart buffer: warmup gate must block.
        let mut cold = ShadowCore::new(shadow_config_from_args(&args), StubEntry::buy_yes());
        push_klines_bootstrap(&mut cold, 30, 1740, 1_900_000);
        assert!(
            spot_buffer_span_s(&cold) < 3600,
            "short buffer should not span vol lookback"
        );
        cold.upsert_market(market(Some(99_000.0)));
        set_books(&mut cold, 0.10, 0.95);
        assert!(
            cold.decide(1900 * NS, false).is_empty(),
            "partial buffer must stand down"
        );

        // Live bootstrap geometry: 70×1m Binance klines (~4140s span).
        let mut warm = ShadowCore::new(shadow_config_from_args(&args), StubEntry::buy_yes());
        push_klines_bootstrap(&mut warm, 70, 4140, 1_900_000);
        assert!(
            vol_warmup_cleared(&warm, args.vol_lookback_s),
            "bootstrap must clear vol3600 without waiting for 1h of live tape"
        );
    }

    #[test]
    fn warmup_gate_blocks_entries_until_buffer_spans_lookback() {
        // Buffer covering less than vol_lookback_s: stand down even with an
        // eager strategy (the post-restart off-model regime).
        let mut core = ShadowCore::new(cfg(), StubEntry::buy_yes());
        let mut price = 99_000.0;
        for s in 1000..2000i64 {
            core.push_spot(s * 1_000, s * 1_000 + 25, price, 1.0, false);
            price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
        }
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.10, 0.95);
        assert!(core.decide(1900 * NS, false).is_empty(), "warmup gate must block");
    }

    #[test]
    fn no_entry_inside_stop_before_close_window() {
        let mut core = core_with_spot_strike(99_000.0);
        core.upsert_market(market(Some(99_000.0)));
        set_books(&mut core, 0.50, 0.50);
        assert!(core.decide(2095 * NS, false).is_empty(), "deadline is close - 90s");
        assert!(core.decide(1700 * NS, false).is_empty(), "not open yet");
    }

    #[test]
    fn upsert_refreshes_strike_but_keeps_entered_flag() {
        let mut core = core_with_spot();
        core.upsert_market(market(None));
        set_books(&mut core, 0.10, 0.95);
        assert_eq!(core.decide(1900 * NS, false).len(), 1);
        // Re-discovery now carries the true strike; entered must survive.
        core.upsert_market(market(Some(100_123.0)));
        let m = core.markets.get("btc-updown-5m-1800").unwrap();
        assert!(m.entered);
        assert_eq!(m.gamma_strike, Some(100_123.0));
        assert!(core.decide(1901 * NS, false).is_empty());
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
        let core = ShadowCore::new(cfg(), Box::new(NoopStrategy));
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
        let mut core = ShadowCore::new(cfg(), Box::new(NoopStrategy));
        for s in 0..9000i64 {
            core.push_spot(s * 1_000, s * 1_000 + 5, 100_000.0, 1.0, false);
        }
        let history = core.spot_history();
        let first = history.samples().first().unwrap().ts_ns / NS;
        assert!(first >= 8999 - SPOT_KEEP_SECS && first > 0);
    }
}
