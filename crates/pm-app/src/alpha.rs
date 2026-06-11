//! `alpha` subcommand: feed pm-alpha's validation harness from the telonex
//! cache. Streams markets one at a time (tape dropped after each) so full-May
//! runs fit in memory.

use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
use pm_alpha::harness::{BookTick, EntryMode, HarnessConfig, HuntReport, MarketRunOutput, MarketSeries, aggregate, run_market_grid};
use pm_alpha::{AlphaModel, AlphaModelConfig, ExoCalibrator, MarketMeta, Token, TrainingConfig, TrainingSample};
use pm_telonex_loader::TelonexStore;
use pm_types::MarketId;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::discovery::{MarketHandle, spot_symbol_for_market};
use crate::walkforward::{
    SpotCache, load_replay_events_for_market, market_close_ns, market_duration_secs_from_slug,
    market_open_ns, outcome_label_resolved_yes,
};

#[derive(Debug, Clone)]
pub struct AlphaArgs {
    pub markets_path: PathBuf,
    pub slug_prefix: String,
    pub date_start: Option<String>,
    pub date_end: Option<String>,
    pub max_markets: usize,
    pub replay_event_cache_dir: Option<PathBuf>,
    pub latencies_ms: Vec<u64>,
    pub edge_thresholds: Vec<f64>,
    pub fee_bps: f64,
    pub notional_usdc: f64,
    pub kelly_sizing: bool,
    pub depth_capture_frac: f64,
    pub skip_touch_level: bool,
    pub decision_dt_ms: u64,
    pub stop_before_close_s: u32,
    pub max_clips: u32,
    pub clip_cooldown_ms: u64,
    pub exit_after_s: u32,
    pub skip_calm: bool,
    pub only_calm: bool,
    pub aligned_mode: bool,
    pub align_min_mid: f64,
    pub tail_max_price: f64,
    pub tail_frac: f64,
    /// Use the final tape mid as the outcome when the manifest label is
    /// missing/Unknown (skips markets whose final mid is ambiguous).
    pub infer_outcome: bool,
    pub vol_lookback_s: u32,
    pub momentum_lookback_s: u32,
    pub momentum_weight: f64,
    pub out_json: Option<PathBuf>,
    /// Train the exogenous calibrator on dates before this (YYYY-MM-DD) and
    /// evaluate on dates at-or-after it.
    pub calibrate_split: Option<String>,
    pub calibrator_out: Option<PathBuf>,
    pub calibrator_in: Option<PathBuf>,
    /// Trained continuation model JSON ({w,b,mu,sd} from
    /// scripts/dir_train.py); gates and prices Aligned entries.
    pub dir_model: Option<PathBuf>,
    /// Dump directional continuation samples (requires --calibrate-split or
    /// any training pass) to this JSONL path.
    pub dir_samples_out: Option<PathBuf>,
    /// Dump per-trade records (first grid cell only) to this JSONL path.
    pub trades_out: Option<PathBuf>,
    /// JSONL of Down-token MarketHandle rows (metadata discovery with
    /// --token-outcome Down); enables the real NO ladder, keyed by slug.
    pub down_assets: Option<PathBuf>,
    /// Compact merged-tick cache dir (bincode+zstd of the two-sided
    /// BookTick series; ~10x smaller and faster than re-decoding parquet).
    pub tick_cache_dir: Option<PathBuf>,
    /// Official open prints (slug -> open_price JSONL from
    /// scripts/polymarket_strikes_fetch.py); overrides the Binance-open
    /// strike proxy where present.
    pub strikes: Option<PathBuf>,
    /// Load the perp complex (futures prints, OI, funding) for this symbol
    /// and expose it to the belief via ExoState.perp.
    pub perp_symbol: Option<String>,
    /// Cache root for the perp parquets (defaults to the local cache dir).
    pub perp_cache_dir: Option<PathBuf>,
}

#[derive(serde::Serialize)]
struct AlphaRunReport {
    model_cfg: AlphaModelConfig,
    harness_cfg: HarnessConfig,
    slug_prefix: String,
    date_start: Option<String>,
    date_end: Option<String>,
    n_markets_considered: usize,
    n_markets_run: usize,
    n_skipped_no_strike: usize,
    n_skipped_no_outcome: usize,
    n_skipped_load_error: usize,
    sweep: Vec<GridEntry>,
}

#[derive(serde::Serialize)]
struct GridEntry {
    latency_ms: u64,
    edge_threshold: f64,
    report: HuntReport,
}

fn read_markets(path: &Path) -> Result<Vec<MarketHandle>> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("open markets file {}", path.display()))?;
    let mut out = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line)?);
    }
    Ok(out)
}

fn in_date_range(date: &str, start: Option<&str>, end: Option<&str>) -> bool {
    if let Some(s) = start
        && date < s
    {
        return false;
    }
    if let Some(e) = end
        && date > e
    {
        return false;
    }
    true
}

enum Item {
    SkipNoOutcome,
    SkipNoStrike,
    SkipLoadError,
    Done(Box<(MarketMeta, Vec<MarketRunOutput>)>),
}

// Compact merged-tick cache: post-merge two-sided BookTick series per
// market, bincode + zstd, versioned. ~10x smaller than re-decoding the
// up+down parquets and skips the merge entirely on repeat runs.
const TICK_CACHE_MAGIC: &[u8; 4] = b"PTC2";

fn tick_cache_path(dir: &Path, market: &MarketHandle, has_down: bool) -> PathBuf {
    let suffix = if has_down { "2s" } else { "1s" };
    dir.join(&market.date)
        .join(format!("{}.{}.btc", market.asset_id, suffix))
}

fn read_tick_cache(path: &Path) -> Option<Vec<BookTick>> {
    let raw = std::fs::read(path).ok()?;
    if raw.len() < 4 || &raw[..4] != TICK_CACHE_MAGIC {
        return None;
    }
    let decompressed = zstd::stream::decode_all(&raw[4..]).ok()?;
    bincode::deserialize(&decompressed).ok()
}

fn write_tick_cache(path: &Path, ticks: &[BookTick]) {
    let Ok(body) = bincode::serialize(ticks) else {
        return;
    };
    let Ok(compressed) = zstd::stream::encode_all(body.as_slice(), 3) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut buf = Vec::with_capacity(4 + compressed.len());
    buf.extend_from_slice(TICK_CACHE_MAGIC);
    buf.extend_from_slice(&compressed);
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &buf).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Merge the Down-token tape into the YES tape: latest NO state attached to
/// each valid YES tick (pointer merge on timestamps).
fn build_ticks(
    events: &[pm_types::ReplayEvent],
    down_events: &[pm_types::ReplayEvent],
) -> Vec<BookTick> {
    // Audit finding: a stale Down-token ladder prices NO fills optimistically.
    // Only attach the NO state when it is recent; beyond the cutoff the
    // harness falls back to the synthetic complement (conservative).
    const NO_STALENESS_CUTOFF_NS: i64 = 30_000_000_000;
    let mut down_idx = 0usize;
    let mut last_no: Option<&pm_types::ReplayEvent> = None;
    events
        .iter()
        .filter(|e| e.yes_bid > 0.0 && e.yes_ask > 0.0 && e.yes_ask < 1.0)
        .map(|e| {
            while down_idx < down_events.len() && down_events[down_idx].ts_ns <= e.ts_ns {
                last_no = Some(&down_events[down_idx]);
                down_idx += 1;
            }
            let (no_bid, no_ask, no_bids, no_asks) = match last_no {
                Some(n)
                    if n.yes_bid > 0.0
                        && n.yes_ask > 0.0
                        && n.yes_ask < 1.0
                        && e.ts_ns - n.ts_ns <= NO_STALENESS_CUTOFF_NS =>
                {
                    (n.yes_bid, n.yes_ask, n.bids, n.asks)
                }
                _ => (0.0, 0.0, Default::default(), Default::default()),
            };
            BookTick {
                ts_ns: e.ts_ns,
                yes_bid: e.yes_bid,
                yes_ask: e.yes_ask,
                bids: e.bids,
                asks: e.asks,
                no_bid,
                no_ask,
                no_bids,
                no_asks,
            }
        })
        .collect()
}

#[derive(Default)]
struct ProcessCounters {
    n_run: usize,
    n_no_strike: usize,
    n_no_outcome: usize,
    n_load_error: usize,
}

struct ProcessOutput {
    per_cell: Vec<Vec<(MarketMeta, MarketRunOutput)>>,
    counters: ProcessCounters,
    train_samples: Vec<TrainingSample>,
    dir_samples: Vec<pm_alpha::directional::DirSample>,
}

#[allow(clippy::too_many_arguments)]
async fn process_markets(
    store: &TelonexStore,
    spot_cache: &mut SpotCache,
    strikes_by_slug: &std::collections::HashMap<String, f64>,
    markets: &[MarketHandle],
    model: &AlphaModel,
    base_cfg: &HarnessConfig,
    latencies_ms: &[u64],
    edge_thresholds: &[f64],
    replay_event_cache_dir: Option<&Path>,
    infer_outcome: bool,
    down_by_slug: &std::collections::HashMap<String, MarketHandle>,
    tick_cache_dir: Option<&Path>,
    perp: Option<std::sync::Arc<pm_alpha::PerpState>>,
) -> Result<ProcessOutput> {
    let store_inner = store.store();
    let n_cells = latencies_ms.len() * edge_thresholds.len();
    let mut out = ProcessOutput {
        per_cell: vec![Vec::with_capacity(markets.len()); n_cells],
        counters: ProcessCounters::default(),
        train_samples: Vec::new(),
        dir_samples: Vec::new(),
    };
    let (n_run, n_no_strike, n_no_outcome, n_load_error) = (
        &mut out.counters.n_run,
        &mut out.counters.n_no_strike,
        &mut out.counters.n_no_outcome,
        &mut out.counters.n_load_error,
    );

    // Preload spot days serially (small set), then pipeline tape loads with
    // a bounded prefetcher; `buffered` preserves order so results match the
    // serial implementation exactly.
    let mut spot_by_market: Vec<Option<std::sync::Arc<pm_types::SpotHistory>>> =
        Vec::with_capacity(markets.len());
    for market in markets {
        let spot = match spot_symbol_for_market("auto", &market.slug) {
            Ok(Some(symbol)) => spot_cache.get_or_load(store, &symbol, &market.date).await.ok(),
            _ => None,
        };
        spot_by_market.push(spot);
    }

    const PREFETCH: usize = 24;
    let compute_lanes = std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(2).max(2))
        .unwrap_or(4);
    let model = std::sync::Arc::new(model.clone());
    let base_cfg = std::sync::Arc::new(*base_cfg);
    let latencies: std::sync::Arc<Vec<u64>> = std::sync::Arc::new(latencies_ms.to_vec());
    let thresholds: std::sync::Arc<Vec<f64>> = std::sync::Arc::new(edge_thresholds.to_vec());

    let mut result_stream = futures::stream::iter(markets.iter().enumerate().map(|(idx, market)| {
        let store = store.clone();
        let store_inner = store_inner.clone();
        let down = down_by_slug.get(&market.slug).cloned();
        let cache_dir = replay_event_cache_dir.map(|p| p.to_path_buf());
        let spot = spot_by_market[idx].clone();
        let official_strike = strikes_by_slug.get(&market.slug).copied();
        let market = market.clone();
        let model = model.clone();
        let base_cfg = base_cfg.clone();
        let latencies = latencies.clone();
        let thresholds = thresholds.clone();
        let tick_cache = tick_cache_dir.map(|p| p.to_path_buf());
        let perp = perp.clone();
        async move {
            let has_down = down.is_some();
            let cache_path = tick_cache
                .as_deref()
                .map(|d| tick_cache_path(d, &market, has_down));
            let cached: Option<Vec<BookTick>> = match &cache_path {
                Some(p) => {
                    let p = p.clone();
                    tokio::task::spawn_blocking(move || read_tick_cache(&p))
                        .await
                        .ok()
                        .flatten()
                }
                None => None,
            };
            let (events, down_events) = if cached.is_some() {
                (Ok(Vec::new()), Vec::new())
            } else {
                let events = load_replay_events_for_market(
                    &store,
                    store_inner.clone(),
                    &market,
                    MarketId(idx as u32),
                    cache_dir.as_deref(),
                )
                .await;
                let down_events = match &down {
                    Some(d) => load_replay_events_for_market(
                        &store,
                        store_inner,
                        d,
                        MarketId(idx as u32 | 0x8000_0000),
                        cache_dir.as_deref(),
                    )
                    .await
                    .unwrap_or_default(),
                    None => Vec::new(),
                };
                (events, down_events)
            };
            tokio::task::spawn_blocking(move || {
                compute_market(
                    &market,
                    events,
                    down_events,
                    cached,
                    cache_path.as_deref(),
                    spot,
                    official_strike,
                    perp,
                    infer_outcome,
                    &model,
                    &base_cfg,
                    &latencies,
                    &thresholds,
                )
            })
            .await
            .unwrap_or(Item::SkipLoadError)
        }
    }))
    .buffered(PREFETCH.max(compute_lanes));

    while let Some(item) = result_stream.next().await {
        match item {
            Item::SkipNoOutcome => *n_no_outcome += 1,
            Item::SkipNoStrike => *n_no_strike += 1,
            Item::SkipLoadError => *n_load_error += 1,
            Item::Done(boxed) => {
                let (meta, outputs) = *boxed;
                for (cell, mut market_out) in outputs.into_iter().enumerate() {
                    out.train_samples.append(&mut market_out.train_samples);
                    out.dir_samples.append(&mut market_out.dir_samples);
                    out.per_cell[cell].push((meta, market_out));
                }
                *n_run += 1;
                if *n_run % 500 == 0 {
                    tracing::info!(n_run = *n_run, total = markets.len(), "alpha progress");
                }
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn compute_market(
    market: &MarketHandle,
    events_res: Result<Vec<pm_types::ReplayEvent>>,
    down_events: Vec<pm_types::ReplayEvent>,
    cached_ticks: Option<Vec<BookTick>>,
    cache_path: Option<&Path>,
    spot: Option<std::sync::Arc<pm_types::SpotHistory>>,
    official_strike: Option<f64>,
    perp: Option<std::sync::Arc<pm_alpha::PerpState>>,
    infer_outcome: bool,
    model: &AlphaModel,
    base_cfg: &HarnessConfig,
    latencies_ms: &[u64],
    edge_thresholds: &[f64],
) -> Item {
    {
        let outcome_label = outcome_label_resolved_yes(&market.outcome);
        if outcome_label.is_none() && !infer_outcome {
            return Item::SkipNoOutcome;
        }
        let Some(token) = Token::from_slug(&market.slug) else {
            return Item::SkipNoOutcome;
        };
        let Some(spot) = spot else {
            return Item::SkipLoadError;
        };

        let open_ns = market_open_ns(market);
        let close_ns = market_close_ns(market);
        // Strike: the official Polymarket open print when supplied, else the
        // last CEX trade at-or-before the open instant (at-or-after would be
        // a look-ahead). Official strikes carry a USD-index basis vs Binance
        // USDT state, so they are for verification studies, not beliefs.
        let strike = match official_strike {
            Some(k) if k.is_finite() && k > 0.0 => k,
            _ => match spot.price_at_or_before(open_ns) {
                Some(p)
                    if spot
                        .range(open_ns - 5_000_000_000, open_ns)
                        .last()
                        .is_some() =>
                {
                    p
                }
                _ => {
                    return Item::SkipNoStrike;
                }
            },
        };

        let from_cache = cached_ticks.is_some();
        let ticks: Vec<BookTick> = if let Some(t) = cached_ticks {
            t
        } else {
            let events = match events_res {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!(market = %market.slug, error = %err, "tape load failed");
                    return Item::SkipLoadError;
                }
            };
            if events.is_empty() {
                return Item::SkipLoadError;
            }
            build_ticks(&events, &down_events)
        };
        if !from_cache && let Some(p) = cache_path {
            write_tick_cache(p, &ticks);
        }
        if ticks.is_empty() {
            return Item::SkipLoadError;
        }

        // Inferred label: final in-window mid, ambiguous finals skipped.
        let resolved_yes = match outcome_label {
            Some(v) => v,
            None => {
                let last_mid = ticks
                    .iter()
                    .rev()
                    .find(|t| t.ts_ns <= close_ns)
                    .and_then(|t| t.mid());
                match last_mid {
                    Some(m) if m >= 0.55 => true,
                    Some(m) if m <= 0.45 => false,
                    _ => return Item::SkipNoOutcome,
                }
            }
        };

        let series = MarketSeries {
            meta: MarketMeta {
                token,
                window_secs: market_duration_secs_from_slug(&market.slug) as u32,
                open_ts_ns: open_ns,
                close_ts_ns: close_ns,
                strike,
            },
            resolved_yes,
            ticks,
            date: market.date.clone(),
        };

        let outputs = run_market_grid(&series, &spot, perp.as_deref(), model, base_cfg, latencies_ms, edge_thresholds);
        Item::Done(Box::new((series.meta, outputs)))
    }
}

pub async fn run_alpha(store: &TelonexStore, args: AlphaArgs) -> Result<()> {
    let mut markets = read_markets(&args.markets_path)?;
    markets.retain(|m| {
        m.slug.starts_with(&args.slug_prefix)
            && in_date_range(&m.date, args.date_start.as_deref(), args.date_end.as_deref())
    });
    markets.sort_by_key(|m| (m.date.clone(), m.close_ts));
    if args.max_markets > 0 {
        markets.truncate(args.max_markets);
    }
    let n_considered = markets.len();
    if n_considered == 0 {
        return Err(anyhow!("no markets match prefix/date filters"));
    }
    tracing::info!(n = n_considered, "alpha run starting");

    let model_cfg = AlphaModelConfig {
        vol_lookback_s: args.vol_lookback_s,
        vol_sample_dt_s: 1,
        momentum_lookback_s: args.momentum_lookback_s,
        momentum_weight: args.momentum_weight,
    };
    let base_cfg = HarnessConfig {
        latency_ms: *args.latencies_ms.first().unwrap_or(&150),
        taker_fee_bps: args.fee_bps,
        edge_threshold: *args.edge_thresholds.first().unwrap_or(&0.05),
        notional_usdc: args.notional_usdc,
        kelly_sizing: args.kelly_sizing,
        depth_capture_frac: args.depth_capture_frac,
        skip_touch_level: args.skip_touch_level,
        decision_dt_ms: args.decision_dt_ms,
        stop_before_close_s: args.stop_before_close_s,
        max_clips: args.max_clips,
        clip_cooldown_ms: args.clip_cooldown_ms,
        exit_after_s: args.exit_after_s,
        skip_calm: args.skip_calm,
        only_calm: args.only_calm,
        entry_mode: if args.aligned_mode { EntryMode::Aligned } else { EntryMode::Fade },
        align_min_mid: args.align_min_mid,
        tail_max_price: args.tail_max_price,
        tail_frac: args.tail_frac,
        collect_training: false,
        train_sample_dt_s: 15,
    };
    let mut spot_cache = SpotCache::default();
    let strikes_by_slug: std::collections::HashMap<String, f64> = match &args.strikes {
        Some(path) => {
            let mut m = std::collections::HashMap::new();
            for line in BufReader::new(std::fs::File::open(path)?).lines() {
                let line = line?;
                if line.trim().is_empty() { continue; }
                let r: serde_json::Value = serde_json::from_str(&line)?;
                if let (Some(slug), Some(px)) = (r["slug"].as_str(), r["open_price"].as_f64()) {
                    m.insert(slug.to_string(), px);
                }
            }
            tracing::info!(n = m.len(), "official strikes loaded");
            m
        }
        None => Default::default(),
    };
    let perp: Option<std::sync::Arc<pm_alpha::PerpState>> = match &args.perp_symbol {
        Some(symbol) => {
            let mut dates: Vec<String> = markets.iter().map(|m| m.date.clone()).collect();
            dates.sort();
            dates.dedup();
            let cache_root = args
                .perp_cache_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("data/cache"));
            Some(std::sync::Arc::new(
                crate::perp::load_perp_state(store, &cache_root, symbol, &dates).await?,
            ))
        }
        None => None,
    };
    let down_by_slug: std::collections::HashMap<String, MarketHandle> = match &args.down_assets {
        Some(path) => read_markets(path)?
            .into_iter()
            .map(|m| (m.slug.clone(), m))
            .collect(),
        None => Default::default(),
    };
    if !down_by_slug.is_empty() {
        tracing::info!(n = down_by_slug.len(), "real NO ladders enabled (down-asset map loaded)");
    }

    // Resolve the model: optionally train a calibrator on the pre-split days.
    let mut calibrator: Option<ExoCalibrator> = None;
    let mut eval_markets: &[MarketHandle] = &markets;
    let train_eval_split;
    if let Some(split) = &args.calibrate_split {
        let split_idx = markets.partition_point(|m| m.date.as_str() < split.as_str());
        let (train, eval) = markets.split_at(split_idx);
        if train.is_empty() || eval.is_empty() {
            return Err(anyhow!(
                "calibrate split {split} leaves train={} eval={} markets",
                train.len(),
                eval.len()
            ));
        }
        tracing::info!(train = train.len(), eval = eval.len(), "calibration split");
        let train_cfg = HarnessConfig {
            collect_training: true,
            ..base_cfg
        };
        let base_model = AlphaModel {
            cfg: model_cfg,
            calibrator: None,
            dir_model: None,
        };
        let train_out = process_markets(
            store,
            &mut spot_cache,
            &strikes_by_slug,
            train,
            &base_model,
            &train_cfg,
            &[0],
            &[f64::INFINITY],
            args.replay_event_cache_dir.as_deref(),
            args.infer_outcome,
            &down_by_slug,
            args.tick_cache_dir.as_deref(),
            perp.clone(),
        )
        .await?;
        if let Some(path) = &args.dir_samples_out {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(path)?;
            use std::io::Write as _;
            for d in &train_out.dir_samples {
                writeln!(f, "{}", serde_json::to_string(d)?)?;
            }
            println!("dir samples written: {} -> {}", train_out.dir_samples.len(), path.display());
        }
        let mut cal = ExoCalibrator::default();
        let stats = cal.fit_batch(&train_out.train_samples, TrainingConfig::default());
        println!(
            "calibrator trained: {} samples, train log-loss {:.4} (markets={} skipped: strike={} outcome={} load={})",
            stats.samples,
            stats.log_loss,
            train_out.counters.n_run,
            train_out.counters.n_no_strike,
            train_out.counters.n_no_outcome,
            train_out.counters.n_load_error,
        );
        if let Some(path) = &args.calibrator_out {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, serde_json::to_string_pretty(&cal.snapshot())?)?;
            println!("calibrator snapshot written: {}", path.display());
        }
        calibrator = Some(cal);
        train_eval_split = split_idx;
        eval_markets = &markets[train_eval_split..];
    } else if let Some(path) = &args.calibrator_in {
        let snap = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        calibrator = Some(ExoCalibrator::from_snapshot(snap));
        println!("calibrator loaded: {}", path.display());
    }

    let dir_model = match &args.dir_model {
        Some(path) => {
            let dm = pm_alpha::DirModel::load_json(path)?;
            println!("dir model loaded: {}", path.display());
            Some(dm)
        }
        None => None,
    };
    let model = AlphaModel {
        cfg: model_cfg,
        calibrator,
        dir_model,
    };
    let eval_out = process_markets(
        store,
        &mut spot_cache,
        &strikes_by_slug,
        eval_markets,
        &model,
        &base_cfg,
        &args.latencies_ms,
        &args.edge_thresholds,
        args.replay_event_cache_dir.as_deref(),
        args.infer_outcome,
        &down_by_slug,
        args.tick_cache_dir.as_deref(),
        perp,
    )
    .await?;
    if let Some(path) = &args.trades_out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::File::create(path)?;
        use std::io::Write as _;
        for (meta, out) in &eval_out.per_cell[0] {
            for t in &out.trades {
                let row = serde_json::json!({
                    "token": meta.token, "window_secs": meta.window_secs,
                    "open_ts_ns": meta.open_ts_ns, "strike": meta.strike,
                    "regime": out.regime.map(|r| r.as_str()),
                    "side": t.side, "decision_ts_ns": t.decision_ts_ns,
                    "fill_ts_ns": t.fill_ts_ns, "avg_price": t.avg_price,
                    "shares": t.shares, "p_exo": t.p_exo,
                    "mid_at_decision": t.mid_at_decision, "pnl": t.pnl, "won": t.won,
                    "mark_60s": t.mark_60s,
                });
                writeln!(f, "{row}")?;
            }
        }
        println!("trades written: {}", path.display());
    }
    let per_cell = eval_out.per_cell;
    let n_run = eval_out.counters.n_run;
    let n_no_strike = eval_out.counters.n_no_strike;
    let n_no_outcome = eval_out.counters.n_no_outcome;
    let n_load_error = eval_out.counters.n_load_error;
    let n_cells = args.latencies_ms.len() * args.edge_thresholds.len();

    let mut sweep: Vec<GridEntry> = Vec::with_capacity(n_cells);
    for (li, &latency_ms) in args.latencies_ms.iter().enumerate() {
        for (ti, &edge_threshold) in args.edge_thresholds.iter().enumerate() {
            let results = &per_cell[li * args.edge_thresholds.len() + ti];
            sweep.push(GridEntry {
                latency_ms,
                edge_threshold,
                report: aggregate(results.iter().map(|(m, o)| (m, o))),
            });
        }
    }

    println!(
        "\nalpha run: {} markets run / {} considered (skipped: {} no-strike, {} no-outcome, {} load-error)",
        n_run, n_considered, n_no_strike, n_no_outcome, n_load_error
    );
    if !per_cell.is_empty() {
        let outs = &per_cell[0];
        let cov: f64 = outs.iter().map(|(_, o)| o.real_no_coverage).sum::<f64>()
            / outs.len().max(1) as f64;
        let pcs: Vec<f64> = outs.iter().filter_map(|(_, o)| o.min_pair_cost).collect();
        if cov > 0.0 && !pcs.is_empty() {
            let sub1 = pcs.iter().filter(|p| **p < 1.0).count();
            let mean_min = pcs.iter().sum::<f64>() / pcs.len() as f64;
            println!(
                "real-NO coverage {:.1}% | min pair cost: mean {:.4}, sub-$1 in {}/{} markets",
                cov * 100.0,
                mean_min,
                sub1,
                pcs.len()
            );
        }
    }
    println!(
        "model: vol_lookback={}s momentum_lookback={}s weight={} | fee={}bps notional=${}",
        args.vol_lookback_s,
        args.momentum_lookback_s,
        args.momentum_weight,
        args.fee_bps,
        args.notional_usdc
    );
    println!(
        "{:>8} {:>6} {:>8} {:>7} {:>10} {:>9} {:>6} {:>9} {:>9}",
        "latency", "thresh", "markets", "trades", "totPnL$", "perTrade", "hit%", "LL_exo", "LL_book"
    );
    for entry in &sweep {
        let a = &entry.report.aggregate;
        println!(
            "{:>7}ms {:>6.3} {:>8} {:>7} {:>10.2} {:>9.4} {:>6.1} {:>9.4} {:>9.4}",
            entry.latency_ms,
            entry.edge_threshold,
            a.n_markets,
            a.n_trades,
            a.total_pnl,
            a.mean_pnl_per_trade,
            a.hit_rate * 100.0,
            a.log_loss_exo,
            a.log_loss_book
        );
        for (cell, r) in &entry.report.cells {
            if entry.report.cells.len() > 1 {
                println!(
                    "         {cell}: n={} trades={} pnl={:.2} hit={:.1}% ll_exo={:.4} ll_book={:.4}",
                    r.n_markets,
                    r.n_trades,
                    r.total_pnl,
                    r.hit_rate * 100.0,
                    r.log_loss_exo,
                    r.log_loss_book
                );
            }
        }
    }
    if let Some(last) = sweep.last() {
        println!("regime cells (latency {}ms, thr {}):", last.latency_ms, last.edge_threshold);
        for (cell, r) in &last.report.regime_cells {
            println!(
                "  {cell}: n={} trades={} pnl={:.2} per={:.3} hit={:.1}%",
                r.n_markets,
                r.n_trades,
                r.total_pnl,
                r.mean_pnl_per_trade,
                r.hit_rate * 100.0
            );
        }
    }

    if let Some(path) = &args.out_json {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let report = AlphaRunReport {
            model_cfg,
            harness_cfg: base_cfg,
            slug_prefix: args.slug_prefix.clone(),
            date_start: args.date_start.clone(),
            date_end: args.date_end.clone(),
            n_markets_considered: n_considered,
            n_markets_run: n_run,
            n_skipped_no_strike: n_no_strike,
            n_skipped_no_outcome: n_no_outcome,
            n_skipped_load_error: n_load_error,
            sweep,
        };
        std::fs::write(path, serde_json::to_string_pretty(&report)?)?;
        println!("report written: {}", path.display());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_range_filter_is_inclusive() {
        assert!(in_date_range("2026-05-10", Some("2026-05-10"), Some("2026-05-10")));
        assert!(!in_date_range("2026-05-09", Some("2026-05-10"), None));
        assert!(!in_date_range("2026-05-11", None, Some("2026-05-10")));
        assert!(in_date_range("2026-05-11", None, None));
    }
}
