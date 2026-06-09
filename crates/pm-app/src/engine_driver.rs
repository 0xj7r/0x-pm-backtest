//! Leg-pairing and parquet→EngineEvent loading for the pm-engine backtest driver.
//!
//! Each Polymarket binary market has two legs (YES / NO), stored as separate
//! `MarketHandle` rows with `outcome` in {"Up","Yes"} or {"Down","No"}.
//! `pair_legs` merges both legs' `ReplayEvent` slices into one ts-ordered
//! `Vec<EngineEvent>`, forward-filling the counter-leg's ladder on every update.
//!
//! Tie-break rule: when a YES and NO event share an identical `ts_ns`, the YES
//! event is processed first (state updated, event emitted) then the NO event
//! (state updated, second event emitted). The output length therefore equals
//! `yes.len() + no.len()` in all cases.

// Public API consumed by the engine driver (Task 8); suppress dead_code until
// the caller is wired in.
#![allow(dead_code)]

use anyhow::{Context, Result, anyhow};
use pm_engine::engine::{Engine, MarketMeta};
use pm_engine::enrich::{CtxEnricher, PriorRanges};
use pm_engine::event::{EngineEvent, Token};
use pm_engine::feed::SliceFeed;
use pm_engine::portfolio::Portfolio;
use pm_engine::risk::{RiskGate, RiskLimits};
use pm_engine::seams::SimClock;
use pm_engine::sim_exchange::{SimExchange, SimExchangeConfig};
use pm_model::{ModelConfig, ModelMarketContext};
use pm_strategy::{BonereaperV2, BonereaperV2Config, ConvexBookStrategy, ConvexBookConfig, PositionConfig, Strategy};
use pm_telonex_loader::{
    Channel, TelonexStore, load_binance_agg_trades_async, load_book_snapshot_async,
    load_pm_trades_async, resolve_binance_day, resolve_pm_trades_day,
};
use pm_types::{MarketId, NoBook, ReplayEvent, ReplayFlags, SpotHistory, SpotTick, TradeHistory, TradeTick};
use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::discovery::{MarketHandle, discover_markets_from_local_book_metadata};

/// Window length of a 5-minute Polymarket up/down market, in seconds.
const FIVE_MIN_SECS: i64 = 300;

// Task 8: wire PM trade tape to SimExchange.on_trade

/// Returns `true` when the outcome label maps to the YES leg.
fn is_yes_leg(outcome: &str) -> Option<bool> {
    if outcome.eq_ignore_ascii_case("up") || outcome.eq_ignore_ascii_case("yes") {
        Some(true)
    } else if outcome.eq_ignore_ascii_case("down") || outcome.eq_ignore_ascii_case("no") {
        Some(false)
    } else {
        None
    }
}

/// Build a `NoBook` from the bids/asks of a NO-leg `ReplayEvent`.
fn no_book_from_replay(ev: &ReplayEvent) -> NoBook {
    NoBook {
        bids: ev.bids,
        asks: ev.asks,
    }
}

/// Merge a YES-leg and a NO-leg `ReplayEvent` slice for one market into a
/// single timestamp-ordered `Vec<EngineEvent>`.
///
/// Each output event carries:
/// - `replay`: the latest YES `ReplayEvent` with `market_id` forced to `market`.
///   Before the first YES snapshot the event is NOT emitted (we have nothing to
///   carry for the strategy).
/// - `no_book`: the latest NO ladder seen at or before this timestamp.
///   Before the first NO snapshot `NoBook::default()` (all-zero) is used.
///
/// **Tie-break:** when `yes.ts_ns == no.ts_ns`, YES is applied first, then NO.
/// Each application emits one event, so a tie produces two consecutive events
/// at the same timestamp.
///
/// Both input slices are assumed to be sorted ascending by `ts_ns` (the loader
/// sorts them after loading).
pub fn pair_legs(
    market: MarketId,
    yes: &[ReplayEvent],
    no: &[ReplayEvent],
) -> Vec<EngineEvent> {
    let capacity = yes.len() + no.len();
    let mut out = Vec::with_capacity(capacity);

    let mut latest_yes: Option<ReplayEvent> = None;
    let mut latest_no: NoBook = NoBook::default();

    let mut yi = 0usize;
    let mut ni = 0usize;

    while yi < yes.len() || ni < no.len() {
        let take_yes = match (yes.get(yi), no.get(ni)) {
            (Some(y), Some(n)) => y.ts_ns <= n.ts_ns, // YES wins on tie
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };

        if take_yes {
            let mut ev = yes[yi];
            ev.market_id = market;
            latest_yes = Some(ev);
            yi += 1;
            out.push(EngineEvent::Market { replay: ev, no_book: latest_no });
        } else {
            let no_ev = &no[ni];
            latest_no = no_book_from_replay(no_ev);
            ni += 1;
            if let Some(yes_ev) = latest_yes {
                // Stamp the event at the NO update's ts so the stream stays
                // monotonically ordered. The YES book fields are carried forward
                // unchanged; only ts_ns and market_id are overwritten.
                let mut carried = yes_ev;
                carried.ts_ns = no_ev.ts_ns;
                carried.market_id = market;
                out.push(EngineEvent::Market { replay: carried, no_book: latest_no });
            }
            // If no YES event has arrived yet, we have nothing to emit for the
            // strategy; the NO ladder will be carried once YES arrives.
        }
    }

    out
}

/// Load both legs for one market and return a merged `Vec<EngineEvent>`.
///
/// `handles` must contain exactly two `MarketHandle`s for the same underlying
/// market (one YES leg and one NO leg). The YES/NO distinction is made via
/// `outcome_label_resolved_yes` (Up/Yes → YES; Down/No → NO).
///
/// Loading is async (parquet over S3/local object-store); call this from within
/// a tokio context. The resulting `Vec` can be wrapped in a `SliceFeed` for
/// synchronous engine consumption.
pub async fn load_market_paired(
    store: &TelonexStore,
    market: MarketId,
    yes_handle: &MarketHandle,
    no_handle: &MarketHandle,
) -> Result<Vec<EngineEvent>> {
    let store_arc: Arc<dyn object_store::ObjectStore> = store.store();

    let yes_path = store
        .resolve_asset_day("polymarket", Channel::BookSnapshot25, &yes_handle.date, &yes_handle.asset_id)
        .await
        .with_context(|| format!("resolve YES leg for {}", yes_handle.slug))?;

    let no_path = store
        .resolve_asset_day("polymarket", Channel::BookSnapshot25, &no_handle.date, &no_handle.asset_id)
        .await
        .with_context(|| format!("resolve NO leg for {}", no_handle.slug))?;

    let (yes_events, _) = load_book_snapshot_async(store_arc.clone(), yes_path, market)
        .await
        .with_context(|| format!("load YES leg for {}", yes_handle.slug))?;

    // Use a temporary market id for the NO leg so it doesn't collide; pair_legs
    // ignores NO market_ids (only uses bids/asks for NoBook).
    let no_tmp_id = MarketId(market.0 ^ 0xFFFF_FFFF);
    let (no_events, _) = load_book_snapshot_async(store_arc, no_path, no_tmp_id)
        .await
        .with_context(|| format!("load NO leg for {}", no_handle.slug))?;

    Ok(pair_legs(market, &yes_events, &no_events))
}

/// Classify a slice of two `MarketHandle`s into (yes_handle, no_handle).
///
/// Returns an error if the outcome labels are not both resolvable or if both
/// handles map to the same side.
pub fn split_yes_no(
    handles: &[MarketHandle],
) -> Result<(&MarketHandle, &MarketHandle)> {
    let mut yes: Option<&MarketHandle> = None;
    let mut no: Option<&MarketHandle> = None;
    for h in handles {
        match is_yes_leg(&h.outcome) {
            Some(true) => yes = Some(h),
            Some(false) => no = Some(h),
            None => return Err(anyhow!("unrecognized outcome label {:?} for {}", h.outcome, h.slug)),
        }
    }
    match (yes, no) {
        (Some(y), Some(n)) => Ok((y, n)),
        (None, _) => Err(anyhow!("no YES leg found in handles")),
        (_, None) => Err(anyhow!("no NO leg found in handles")),
    }
}

#[derive(Debug, Clone, Copy)]
pub enum StrategyKind {
    BonereaperV2,
    Convex,
}

/// Configuration for a standalone pm-engine BTC-5m backtest run.
pub struct EngineBacktestCfg {
    pub cache_dir: PathBuf,
    pub start_date: String,
    pub end_date: String,
    pub slug_prefix: String,
    pub spot_symbol: String,
    pub snapshot_path: PathBuf,
    pub starting_cash: f64,
    pub max_clip_usdc: f64,
    pub taker_latency_ms: u64,
    pub taker_fee_bps: f64,
    pub maker_rebate_bps: f64,
    /// Book-event thinning in ms (champion ran 1000); 0 = keep every event.
    pub replay_sample_ms: i64,
    /// Cap on markets processed (for small fixed slices / quick runs).
    pub max_markets: Option<usize>,
    pub strategy: StrategyKind,
}

/// Headline numbers from an engine backtest run.
pub struct EngineBacktestReport {
    pub markets_total: usize,
    pub markets_traded: usize,
    pub orders_submitted: usize,
    pub fills: usize,
    pub starting_cash_usd: f64,
    pub final_equity_usd: f64,
    pub trace: Vec<(i64, &'static str, MarketId, f64)>,
}

/// All BTC-5m markets share one exposure window; the correlated cap is disabled
/// by default (`max_correlated_net_shares = 0.0`), so the window value is inert.
fn classify_btc(_m: MarketId) -> (Token, i64) {
    (Token::Btc, 0)
}

/// Permissive risk limits: br2's own gates drive selectivity, not the risk gate.
/// (T8 is a plausibility run, not a champion-config reproduction.)
fn engine_risk_limits(max_clip_usdc: f64) -> RiskLimits {
    RiskLimits {
        max_order_notional_usd: (max_clip_usdc * 10.0).max(300.0),
        max_gross_notional_usd: 1e9,
        max_net_notional_per_market_usd: 1e9,
        max_position_quantity_per_instrument: 1e9,
        min_free_cash_usd: 0.0,
        min_portfolio_equity_usd: 0.0,
        max_open_orders_total: 1_000_000,
        max_open_orders_per_market: 10_000,
        max_correlated_net_shares: 0.0,
    }
}

/// Inclusive list of `YYYY-MM-DD` dates from `start` to `end`.
fn date_range(start: &str, end: &str) -> Result<Vec<String>> {
    use chrono::{Duration, NaiveDate};
    let s = NaiveDate::parse_from_str(start, "%Y-%m-%d").with_context(|| format!("parse start {start}"))?;
    let e = NaiveDate::parse_from_str(end, "%Y-%m-%d").with_context(|| format!("parse end {end}"))?;
    if e < s {
        return Err(anyhow!("end {end} is before start {start}"));
    }
    let mut out = Vec::new();
    let mut d = s;
    while d <= e {
        out.push(d.format("%Y-%m-%d").to_string());
        d += Duration::days(1);
    }
    Ok(out)
}

fn market_yes_mid(e: &EngineEvent) -> Option<f32> {
    match e {
        EngineEvent::Market { replay, .. } => Some(replay.yes_mid),
        EngineEvent::Trade { .. } => None,
    }
}

/// Keep one book event per `sample_ns` window, always retaining the final event
/// so the market's last book state survives. Mirrors `--replay-sample-ms`.
fn downsample_book_events(events: Vec<EngineEvent>, sample_ns: i64) -> Vec<EngineEvent> {
    if sample_ns <= 0 || events.is_empty() {
        return events;
    }
    let last = *events.last().unwrap();
    let mut out = Vec::with_capacity(events.len());
    let mut last_kept: Option<i64> = None;
    for e in events {
        if last_kept.is_none() || e.ts() - last_kept.unwrap() >= sample_ns {
            last_kept = Some(e.ts());
            out.push(e);
        }
    }
    if out.last().map(|e| e.ts()) != Some(last.ts()) {
        out.push(last);
    }
    out
}

/// Mean of the last `window` values of `prior` (the markets that closed before
/// this one). Empty prefix → 0.0. Mirrors `prior_market_range_mean`.
fn trailing_mean(prior: &[f64], window: usize) -> f32 {
    if window == 0 || prior.is_empty() {
        return 0.0;
    }
    let start = prior.len().saturating_sub(window);
    let slice = &prior[start..];
    (slice.iter().sum::<f64>() / slice.len() as f64) as f32
}

/// One market's loaded, sampled event stream plus derived metadata.
struct LoadedMarket {
    market: MarketId,
    events: Vec<EngineEvent>,
    vol_range: f64,
    open_ns: i64,
    close_ns: i64,
    trades: TradeHistory,
}

/// Discover BTC-5m markets as YES+NO leg pairs from the LOCAL book cache.
///
/// The book-snapshot `outcome` column is the token side (Up/Down), so we scan
/// each side separately and join by slug. `MarketHandle.outcome` is set to the
/// queried side here so `split_yes_no` / pairing can rely on it.
fn discover_btc5m_pairs(cfg: &EngineBacktestCfg, dates: &[String]) -> Result<Vec<(MarketId, MarketHandle, MarketHandle)>> {
    let mut pairs: Vec<(MarketId, MarketHandle, MarketHandle)> = Vec::new();
    for date in dates {
        let ups = discover_markets_from_local_book_metadata(&cfg.cache_dir, date, &cfg.slug_prefix, "Up")?;
        let downs = discover_markets_from_local_book_metadata(&cfg.cache_dir, date, &cfg.slug_prefix, "Down")?;
        let down_by_slug: HashMap<String, MarketHandle> =
            downs.into_iter().map(|h| (h.slug.clone(), h)).collect();
        for mut up in ups {
            let Some(down) = down_by_slug.get(&up.slug) else {
                continue;
            };
            up.outcome = "Up".to_string();
            let mut down = down.clone();
            down.outcome = "Down".to_string();
            pairs.push((MarketId(up.close_ts as u32), up, down));
        }
    }
    // Close-time order drives the prior-range window; dedup markets that appear
    // under more than one date partition (keyed by the unique close_ts).
    pairs.sort_by_key(|(_, up, _)| up.close_ts);
    pairs.dedup_by_key(|(id, _, _)| *id);
    Ok(pairs)
}

/// Load one market's paired book stream, thin it, attach the YES-leg PM trade
/// tape (both as a `TradeHistory` for the strategy and as `Trade` events that
/// drive maker fills), and synthesize a `MARKET_CLOSE` event so the engine
/// settles (the book loader only emits `BOOK_UPDATE`).
async fn load_one_market(
    store: &TelonexStore,
    store_arc: &Arc<dyn object_store::ObjectStore>,
    cfg: &EngineBacktestCfg,
    market: MarketId,
    up: &MarketHandle,
    down: &MarketHandle,
) -> Result<LoadedMarket> {
    let mut events = load_market_paired(store, market, up, down).await?;
    events = downsample_book_events(events, cfg.replay_sample_ms * 1_000_000);

    let vol_range = {
        let mids: Vec<f32> = events.iter().filter_map(market_yes_mid).collect();
        match (
            mids.iter().cloned().fold(f32::INFINITY, f32::min),
            mids.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        ) {
            (lo, hi) if lo.is_finite() && hi.is_finite() => (hi - lo) as f64,
            _ => 0.0,
        }
    };

    // Last book state (for the synthesized close event), taken before trades.
    let last_book = events.iter().rev().find_map(|e| match e {
        EngineEvent::Market { replay, no_book } => Some((*replay, *no_book)),
        EngineEvent::Trade { .. } => None,
    });

    // YES-leg (up-token) PM trade tape: drives both the strategy's trade-flow
    // feature and the SimExchange trade-tape maker fills.
    let yes_trades: Vec<TradeTick> = match resolve_pm_trades_day(store, &up.date, &up.asset_id).await {
        Ok(path) => match load_pm_trades_async(store_arc.clone(), path).await {
            Ok((ticks, _)) => ticks,
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    for t in &yes_trades {
        events.push(EngineEvent::Trade { market, tick: *t });
    }

    // The slug's trailing number is the market OPEN/start ts (the runner's
    // market_open_ts = parse_close_ts(slug)); the close is open + duration.
    // `up.close_ts` was parsed from the slug, so it IS the open ts here.
    let open_ts = up.close_ts;
    let close_ts = open_ts + FIVE_MIN_SECS;
    let open_ns = open_ts * 1_000_000_000;
    let close_ns = close_ts * 1_000_000_000;

    if let Some((mut replay, no_book)) = last_book {
        let max_ts = events.iter().map(|e| e.ts()).max().unwrap_or(replay.ts_ns);
        replay.ts_ns = max_ts + 1;
        replay.flags = ReplayFlags::MARKET_CLOSE;
        events.push(EngineEvent::Market { replay, no_book });
    }
    events.sort_by_key(|e| e.ts());

    Ok(LoadedMarket {
        market,
        events,
        vol_range,
        open_ns,
        close_ns,
        trades: TradeHistory::new(yes_trades),
    })
}

/// Load full-window Binance spot (`start-1d .. end`) into one `SpotHistory`.
async fn load_window_spot(
    store: &TelonexStore,
    store_arc: &Arc<dyn object_store::ObjectStore>,
    symbol: &str,
    dates: &[String],
) -> SpotHistory {
    use chrono::{Duration, NaiveDate};
    let mut all: Vec<String> = Vec::new();
    if let Some(prev) = dates
        .first()
        .and_then(|first| NaiveDate::parse_from_str(first, "%Y-%m-%d").ok())
    {
        all.push((prev - Duration::days(1)).format("%Y-%m-%d").to_string());
    }
    all.extend(dates.iter().cloned());

    let mut ticks: Vec<SpotTick> = Vec::new();
    for date in &all {
        let Ok(path) = resolve_binance_day(store, "agg_trades", symbol, date).await else {
            continue;
        };
        let Ok((day, _)) = load_binance_agg_trades_async(store_arc.clone(), path).await else {
            continue;
        };
        ticks.extend(day);
    }
    SpotHistory::new(ticks)
}

/// Strategy-agnostic engine core: build engine from a template strategy + pre-computed
/// inputs, run the feed, tally the trace, and return the report.
fn run_engine_with<S: Strategy + Clone>(
    template: S,
    cfg: &EngineBacktestCfg,
    enricher: CtxEnricher,
    all_events: Vec<EngineEvent>,
    prior_map: HashMap<MarketId, PriorRanges>,
    meta_map: HashMap<MarketId, MarketMeta>,
    trades_map: HashMap<MarketId, TradeHistory>,
    spot: pm_types::SpotHistory,
    markets_total: usize,
) -> EngineBacktestReport {
    let mut engine = Engine::new(
        template,
        Portfolio::new(cfg.starting_cash),
        RiskGate { limits: engine_risk_limits(cfg.max_clip_usdc) },
        classify_btc,
    )
    .with_enricher(enricher, spot, TradeHistory::default(), PriorRanges::default())
    .with_market_meta(meta_map)
    .with_market_prior_ranges(prior_map)
    .with_market_trades(trades_map);

    let clock_cell = Rc::new(Cell::new(0i64));
    let mut feed = SliceFeed::with_clock(all_events, clock_cell.clone());
    let clock = SimClock { ts: clock_cell };
    let mut sim = SimExchange::new(SimExchangeConfig {
        taker_latency_ms: cfg.taker_latency_ms,
        taker_fee_bps: cfg.taker_fee_bps,
        maker_rebate_bps: cfg.maker_rebate_bps,
    });
    engine.run(&mut feed, &mut sim, &clock);

    let mut traded: std::collections::HashSet<MarketId> = std::collections::HashSet::new();
    let mut orders_submitted = 0usize;
    let mut fills = 0usize;
    let mut tag_tally: std::collections::BTreeMap<&'static str, usize> = std::collections::BTreeMap::new();
    for (_, kind, market, _) in &engine.trace {
        *tag_tally.entry(*kind).or_insert(0) += 1;
        match *kind {
            "submit" => {
                orders_submitted += 1;
                traded.insert(*market);
            }
            "fill" => {
                fills += 1;
                traded.insert(*market);
            }
            _ => {}
        }
    }
    let proposals: usize = tag_tally.iter().filter(|(k, _)| **k != "fill").map(|(_, n)| *n).sum();
    eprintln!("[engine-diag] trace tags: {tag_tally:?}  (proposals submit+rejects = {proposals})");

    EngineBacktestReport {
        markets_total,
        markets_traded: traded.len(),
        orders_submitted,
        fills,
        starting_cash_usd: cfg.starting_cash,
        final_equity_usd: engine.portfolio.free_cash_usd(),
        trace: engine.trace,
    }
}

/// Run an engine backtest over real BTC-5m data with the both-book fill model.
/// Strategy-agnostic discovery/loading is performed once; then `cfg.strategy`
/// selects which template to run.
pub async fn run_engine_backtest(cfg: EngineBacktestCfg) -> Result<EngineBacktestReport> {
    let store = TelonexStore::try_new_local(cfg.cache_dir.clone())?;
    let store_arc = store.store();
    let dates = date_range(&cfg.start_date, &cfg.end_date)?;

    let mut pairs = discover_btc5m_pairs(&cfg, &dates)?;
    if let Some(n) = cfg.max_markets {
        pairs.truncate(n);
    }
    let markets_total = pairs.len();

    let mut loaded: Vec<LoadedMarket> = Vec::with_capacity(markets_total);
    for (market, up, down) in &pairs {
        match load_one_market(&store, &store_arc, &cfg, *market, up, down).await {
            Ok(m) => loaded.push(m),
            Err(err) => tracing::warn!(slug = %up.slug, error = %err, "skip market: load failed"),
        }
    }
    loaded.sort_by_key(|m| m.close_ns);

    // Per-market maps (prior ranges accumulate in close order, before the market).
    let mut prior_ranges_so_far: Vec<f64> = Vec::with_capacity(loaded.len());
    let mut prior_map: HashMap<MarketId, PriorRanges> = HashMap::new();
    let mut meta_map: HashMap<MarketId, MarketMeta> = HashMap::new();
    let mut trades_map: HashMap<MarketId, TradeHistory> = HashMap::new();
    for m in &loaded {
        prior_map.insert(
            m.market,
            PriorRanges {
                d1: trailing_mean(&prior_ranges_so_far, 288),
                d3: trailing_mean(&prior_ranges_so_far, 3 * 288),
                d7: trailing_mean(&prior_ranges_so_far, 7 * 288),
            },
        );
        prior_ranges_so_far.push(m.vol_range);
        meta_map.insert(
            m.market,
            MarketMeta { open_ns: m.open_ns, close_ns: m.close_ns, resolved_yes: None },
        );
        trades_map.insert(m.market, m.trades.clone());
    }

    // K-way merge every market's stream into one ts-ordered event vector.
    let mut all_events: Vec<EngineEvent> = Vec::new();
    for m in loaded {
        all_events.extend(m.events);
    }
    all_events.sort_by_key(|e| e.ts());

    let spot = load_window_spot(&store, &store_arc, &cfg.spot_symbol, &dates).await;

    let enricher = CtxEnricher::with_model_snapshot(
        &cfg.snapshot_path,
        ModelConfig::default(),
        ModelMarketContext::default(),
    )?;

    let report = match cfg.strategy {
        StrategyKind::BonereaperV2 => {
            let template = BonereaperV2::new(BonereaperV2Config {
                bankroll_usdc: cfg.starting_cash,
                max_clip_usdc: cfg.max_clip_usdc,
                ..Default::default()
            });
            run_engine_with(template, &cfg, enricher, all_events, prior_map, meta_map, trades_map, spot, markets_total)
        }
        StrategyKind::Convex => {
            let template = ConvexBookStrategy::new(ConvexBookConfig {
                position: PositionConfig {
                    bankroll_usdc: cfg.starting_cash,
                    max_clip_usdc: cfg.max_clip_usdc,
                    ..Default::default()
                },
                ..Default::default()
            });
            run_engine_with(template, &cfg, enricher, all_events, prior_map, meta_map, trades_map, spot, markets_total)
        }
    };
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{BookLevel, MarketId, NoBook, ReplayEvent, ReplayFlags, tape::TAPE_DEPTH};

    fn yes_event(ts: i64, ask0_price: f32) -> ReplayEvent {
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        asks[0] = BookLevel { price: ask0_price, size: 500.0 };
        ReplayEvent {
            ts_ns: ts,
            market_id: MarketId(99),
            yes_mid: ask0_price - 0.01,
            yes_bid: ask0_price - 0.02,
            yes_ask: ask0_price,
            volume: 0.0,
            bids: Default::default(),
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    fn no_event(ts: i64, ask0_price: f32) -> ReplayEvent {
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        asks[0] = BookLevel { price: ask0_price, size: 400.0 };
        ReplayEvent {
            ts_ns: ts,
            market_id: MarketId(88),
            yes_mid: ask0_price - 0.01,
            yes_bid: ask0_price - 0.02,
            yes_ask: ask0_price,
            volume: 0.0,
            bids: Default::default(),
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    #[test]
    fn pair_legs_forward_fills_counter_leg_by_ts() {
        // YES leg: ts=1000 ask0=0.60, ts=3000 ask0=0.62
        // NO  leg: ts=2000 ask0=0.42
        let yes = vec![yes_event(1000, 0.60), yes_event(3000, 0.62)];
        let no = vec![no_event(2000, 0.42)];

        let evs = pair_legs(MarketId(0), &yes, &no);

        // All three raw events should produce output (2 YES + 1 NO that has a
        // prior YES to carry forward).
        assert_eq!(evs.len(), 3, "merged length should be yes.len() + no.len()");

        // Events must be in ts order.
        let ts: Vec<i64> = evs.iter().map(|e| e.ts()).collect();
        assert_eq!(ts, vec![1000, 2000, 3000]);

        // ts=1000: first YES event, no NO snapshot yet → empty NoBook.
        let EngineEvent::Market { replay: r0, no_book: nb0 } = evs[0] else { unreachable!() };
        assert_eq!(r0.ts_ns, 1000);
        assert!((r0.yes_ask - 0.60).abs() < 1e-6);
        assert_eq!(nb0, NoBook::default(), "ts=1000 must have empty no_book");

        // ts=2000: NO update carries the latest YES (ask=0.60) forward.
        // The event's ts_ns is stamped at the NO event time (2000) so the stream
        // stays monotonically ordered; the YES book fields are unchanged.
        let EngineEvent::Market { replay: r1, no_book: nb1 } = evs[1] else { unreachable!() };
        assert_eq!(r1.ts_ns, 2000, "NO-triggered event stamped at NO ts");
        assert!((r1.yes_ask - 0.60).abs() < 1e-6, "YES ask carried forward");
        assert!((nb1.asks[0].price - 0.42).abs() < 1e-6, "no_book should carry NO ask0=0.42");

        // ts=3000: second YES update, no_book forward-filled from the ts=2000 NO.
        let EngineEvent::Market { replay: r2, no_book: nb2 } = evs[2] else { unreachable!() };
        assert_eq!(r2.ts_ns, 3000);
        assert!((r2.yes_ask - 0.62).abs() < 1e-6);
        assert!(
            (nb2.asks[0].price - 0.42).abs() < 1e-6,
            "ts=3000 event must forward-fill no_book.asks[0].price=0.42, got {}",
            nb2.asks[0].price
        );
    }

    #[test]
    fn pair_legs_all_market_ids_set_to_target() {
        let yes = vec![yes_event(1000, 0.60)];
        let no = vec![no_event(2000, 0.42)];
        let evs = pair_legs(MarketId(7), &yes, &no);
        for ev in &evs {
            assert_eq!(ev.market_id(), MarketId(7));
        }
    }

    #[test]
    fn pair_legs_empty_no_leg() {
        let yes = vec![yes_event(1000, 0.60), yes_event(2000, 0.62)];
        let evs = pair_legs(MarketId(0), &yes, &[]);
        assert_eq!(evs.len(), 2);
        for ev in &evs {
            let EngineEvent::Market { no_book, .. } = ev else { unreachable!() };
            assert_eq!(*no_book, NoBook::default());
        }
    }

    #[test]
    fn pair_legs_empty_yes_leg() {
        // With no YES events, NO updates have nothing to carry forward; no output.
        let no = vec![no_event(1000, 0.42)];
        let evs = pair_legs(MarketId(0), &[], &no);
        assert_eq!(evs.len(), 0, "no YES data → nothing to emit");
    }

    #[test]
    fn pair_legs_tie_break_yes_first() {
        // Both legs have an event at ts=1000; YES should appear first.
        let yes = vec![yes_event(1000, 0.60)];
        let no = vec![no_event(1000, 0.42)];
        let evs = pair_legs(MarketId(0), &yes, &no);
        assert_eq!(evs.len(), 2);
        // First event: YES update, no_book still empty.
        let EngineEvent::Market { replay: r0, no_book: nb0 } = evs[0] else { unreachable!() };
        assert!((r0.yes_ask - 0.60).abs() < 1e-6);
        assert_eq!(nb0, NoBook::default(), "tie-break: YES first, no_book empty");
        // Second event: NO update, carries YES forward with new no_book.
        let EngineEvent::Market { no_book: nb1, .. } = evs[1] else { unreachable!() };
        assert!((nb1.asks[0].price - 0.42).abs() < 1e-6);
    }

    #[test]
    fn split_yes_no_classifies_correctly() {
        let h_yes = MarketHandle {
            asset_id: "a1".into(),
            slug: "btc-updown-yes".into(),
            close_ts: 0,
            outcome: "Up".into(),
            date: "2026-05-01".into(),
        };
        let h_no = MarketHandle {
            asset_id: "a2".into(),
            slug: "btc-updown-no".into(),
            close_ts: 0,
            outcome: "Down".into(),
            date: "2026-05-01".into(),
        };
        let handles = [h_yes.clone(), h_no.clone()];
        let (y, n) = split_yes_no(&handles).unwrap();
        assert_eq!(y.outcome, "Up");
        assert_eq!(n.outcome, "Down");
    }

    /// Determinism gate (golden trace on real data). Runs the engine backtest
    /// twice over a small fixed BTC-5m slice and asserts the trace is identical.
    ///
    /// pm-app is a bin-only crate (no lib target), so this lives here as a unit
    /// test rather than `tests/engine_btc5m_determinism.rs`. Gated on
    /// `PM_ENGINE_BTC5M_FIXTURE` (the local cache dir, e.g. `data/cache`); it
    /// skips with a reason when unset so it never silently passes without data.
    #[tokio::test]
    async fn engine_btc5m_determinism() {
        let Ok(cache_dir) = std::env::var("PM_ENGINE_BTC5M_FIXTURE") else {
            eprintln!(
                "SKIP engine_btc5m_determinism: set PM_ENGINE_BTC5M_FIXTURE to the local \
                 cache dir (e.g. data/cache) with 2026-05-21 BTC-5m both-leg book + spot"
            );
            return;
        };
        // `cargo test` runs with cwd = crate dir; resolve relative paths against
        // the workspace root so `data/...` matches the real layout.
        let ws_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let resolve = |p: &str| {
            let pb = PathBuf::from(p);
            if pb.is_absolute() { pb } else { ws_root.join(p) }
        };
        let snap = resolve("data/snap062901.json");
        let cache = resolve(&cache_dir);
        if !snap.exists() {
            eprintln!("SKIP engine_btc5m_determinism: {} missing", snap.display());
            return;
        }
        let mk = || EngineBacktestCfg {
            cache_dir: cache.clone(),
            start_date: "2026-05-21".into(),
            end_date: "2026-05-21".into(),
            slug_prefix: "btc-updown-5m".into(),
            spot_symbol: "BTCUSDT".into(),
            snapshot_path: snap.clone(),
            starting_cash: 1000.0,
            max_clip_usdc: 30.0,
            taker_latency_ms: 500,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
            replay_sample_ms: 1000,
            max_markets: Some(40),
            strategy: StrategyKind::BonereaperV2,
        };
        let r1 = run_engine_backtest(mk()).await.expect("engine run 1");
        let r2 = run_engine_backtest(mk()).await.expect("engine run 2");
        assert_eq!(r1.trace, r2.trace, "engine trace must be byte-identical across runs");
        assert_eq!(
            r1.final_equity_usd.to_bits(),
            r2.final_equity_usd.to_bits(),
            "final equity must match bit-for-bit"
        );
        eprintln!(
            "determinism OK: markets_total={} markets_traded={} fills={} equity=${:.4}",
            r1.markets_total, r1.markets_traded, r1.fills, r1.final_equity_usd
        );
    }
}
