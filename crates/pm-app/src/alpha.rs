//! `alpha` subcommand: feed pm-alpha's validation harness from the telonex
//! cache. Streams markets one at a time (tape dropped after each) so full-May
//! runs fit in memory.

use anyhow::{Context, Result, anyhow};
use pm_alpha::harness::{BookTick, HarnessConfig, HuntReport, MarketRunOutput, MarketSeries, aggregate, run_market_grid};
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
    pub decision_dt_ms: u64,
    pub stop_before_close_s: u32,
    pub vol_lookback_s: u32,
    pub momentum_lookback_s: u32,
    pub momentum_weight: f64,
    pub out_json: Option<PathBuf>,
    /// Train the exogenous calibrator on dates before this (YYYY-MM-DD) and
    /// evaluate on dates at-or-after it.
    pub calibrate_split: Option<String>,
    pub calibrator_out: Option<PathBuf>,
    pub calibrator_in: Option<PathBuf>,
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
}

#[allow(clippy::too_many_arguments)]
async fn process_markets(
    store: &TelonexStore,
    spot_cache: &mut SpotCache,
    markets: &[MarketHandle],
    model: &AlphaModel,
    base_cfg: &HarnessConfig,
    latencies_ms: &[u64],
    edge_thresholds: &[f64],
    replay_event_cache_dir: Option<&Path>,
) -> Result<ProcessOutput> {
    let store_inner = store.store();
    let n_cells = latencies_ms.len() * edge_thresholds.len();
    let mut out = ProcessOutput {
        per_cell: vec![Vec::with_capacity(markets.len()); n_cells],
        counters: ProcessCounters::default(),
        train_samples: Vec::new(),
    };
    let (n_run, n_no_strike, n_no_outcome, n_load_error) = (
        &mut out.counters.n_run,
        &mut out.counters.n_no_strike,
        &mut out.counters.n_no_outcome,
        &mut out.counters.n_load_error,
    );

    for (idx, market) in markets.iter().enumerate() {
        let Some(resolved_yes) = outcome_label_resolved_yes(&market.outcome) else {
            *n_no_outcome += 1;
            continue;
        };
        let Some(token) = Token::from_slug(&market.slug) else {
            *n_no_outcome += 1;
            continue;
        };
        let Ok(Some(symbol)) = spot_symbol_for_market("auto", &market.slug) else {
            *n_no_outcome += 1;
            continue;
        };

        let spot = match spot_cache.get_or_load(store, &symbol, &market.date).await {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(market = %market.slug, error = %err, "spot load failed");
                *n_load_error += 1;
                continue;
            }
        };

        let open_ns = market_open_ns(market);
        let close_ns = market_close_ns(market);
        // Strike proxy: CEX spot at the open instant (no oracle history in
        // our data). Skip when the first sample is >5s late — a stale strike
        // poisons the moneyness more than dropping the market costs.
        let strike = match spot.price_at_or_after(open_ns) {
            Some(p)
                if spot
                    .range(open_ns, open_ns + 5_000_000_000)
                    .first()
                    .is_some() =>
            {
                p
            }
            _ => {
                *n_no_strike += 1;
                continue;
            }
        };

        let events = match load_replay_events_for_market(
            store,
            store_inner.clone(),
            market,
            MarketId(idx as u32),
            replay_event_cache_dir,
        )
        .await
        {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(market = %market.slug, error = %err, "tape load failed");
                *n_load_error += 1;
                continue;
            }
        };
        if events.is_empty() {
            *n_load_error += 1;
            continue;
        }

        let ticks: Vec<BookTick> = events
            .iter()
            .filter(|e| e.yes_bid > 0.0 && e.yes_ask > 0.0 && e.yes_ask < 1.0)
            .map(|e| BookTick {
                ts_ns: e.ts_ns,
                yes_bid: e.yes_bid,
                yes_ask: e.yes_ask,
                bids: e.bids,
                asks: e.asks,
            })
            .collect();
        if ticks.is_empty() {
            *n_load_error += 1;
            continue;
        }

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

        let outputs = run_market_grid(&series, &spot, model, base_cfg, latencies_ms, edge_thresholds);
        for (cell, mut market_out) in outputs.into_iter().enumerate() {
            out.train_samples.append(&mut market_out.train_samples);
            out.per_cell[cell].push((series.meta, market_out));
        }
        *n_run += 1;
        if *n_run % 500 == 0 {
            tracing::info!(n_run = *n_run, total = markets.len(), "alpha progress");
        }
    }
    Ok(out)
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
        decision_dt_ms: args.decision_dt_ms,
        stop_before_close_s: args.stop_before_close_s,
        collect_training: false,
        train_sample_dt_s: 15,
    };
    let mut spot_cache = SpotCache::default();

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
        };
        let train_out = process_markets(
            store,
            &mut spot_cache,
            train,
            &base_model,
            &train_cfg,
            &[0],
            &[f64::INFINITY],
            args.replay_event_cache_dir.as_deref(),
        )
        .await?;
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

    let model = AlphaModel {
        cfg: model_cfg,
        calibrator,
    };
    let eval_out = process_markets(
        store,
        &mut spot_cache,
        eval_markets,
        &model,
        &base_cfg,
        &args.latencies_ms,
        &args.edge_thresholds,
        args.replay_event_cache_dir.as_deref(),
    )
    .await?;
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
