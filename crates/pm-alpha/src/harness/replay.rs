//! Per-market replay: belief at decision instants, latency-shifted fills,
//! depth-walked costs, settlement at resolution.
//!
//! The belief pass is computed once per market and shared across the
//! latency x threshold grid (beliefs depend on neither; only entries and
//! fills do), which is what makes parameter sweeps affordable.

use super::types::{
    BookTick, HarnessConfig, MarketRunOutput, MarketSeries, ProbSample, Side, TradeRecord,
};
use crate::calibrator::TrainingSample;
use crate::model::AlphaModel;
use crate::state::ExoState;
use pm_types::SpotHistory;
use pm_types::tape::BookLevel;

/// Checkpoints (seconds since window open) where log-loss samples are taken.
const SAMPLE_OFFSETS_S: [i64; 4] = [60, 120, 180, 240];

/// One decision instant: the belief plus the book touch as of that tick.
struct Decision {
    tick_idx: usize,
    ts_ns: i64,
    p_up: f64,
    mid: f64,
    yes_bid: f64,
    yes_ask: f64,
}

struct BeliefPass {
    decisions: Vec<Decision>,
    samples: Vec<ProbSample>,
    train_samples: Vec<TrainingSample>,
    had_belief: bool,
}

fn belief_pass(
    series: &MarketSeries,
    spot: &SpotHistory,
    model: &AlphaModel,
    cfg: &HarnessConfig,
) -> BeliefPass {
    let open_ns = series.meta.open_ts_ns;
    let close_ns = series.meta.close_ts_ns;
    let entry_deadline_ns = close_ns - cfg.stop_before_close_s as i64 * 1_000_000_000;
    let decision_dt_ns = (cfg.decision_dt_ms.max(1) as i64) * 1_000_000;

    let mut pass = BeliefPass {
        decisions: Vec::new(),
        samples: Vec::new(),
        train_samples: Vec::new(),
        had_belief: false,
    };
    let mut next_decision_ns = open_ns;
    let mut next_sample = 0usize;
    let mut next_train_ns = open_ns;
    let train_dt_ns = (cfg.train_sample_dt_s.max(1) as i64) * 1_000_000_000;

    for (i, tick) in series.ticks.iter().enumerate() {
        if tick.ts_ns < open_ns || tick.ts_ns > close_ns {
            continue;
        }

        // Log-loss checkpoints, scored on whatever tick first crosses each
        // offset (identical instants for p_exo and the book baseline).
        while next_sample < SAMPLE_OFFSETS_S.len()
            && tick.ts_ns >= open_ns + SAMPLE_OFFSETS_S[next_sample] * 1_000_000_000
        {
            let state = ExoState {
                spot,
                market: series.meta,
                now_ns: tick.ts_ns,
            };
            if let (Some(ev), Some(mid)) = (model.evaluate(&state, false), tick.mid()) {
                pass.had_belief = true;
                pass.samples.push(ProbSample {
                    ts_ns: tick.ts_ns,
                    p_exo: ev.p,
                    p_book: mid,
                    resolved_yes: series.resolved_yes,
                });
            }
            next_sample += 1;
        }

        // Training-sample collection at its own cadence (raw base p +
        // exogenous features + outcome; the calibrator maps raw -> truth).
        if cfg.collect_training && tick.ts_ns >= next_train_ns {
            next_train_ns = tick.ts_ns + train_dt_ns;
            let state = ExoState {
                spot,
                market: series.meta,
                now_ns: tick.ts_ns,
            };
            if let Some(ev) = model.evaluate(&state, true)
                && let Some(features) = ev.features
            {
                pass.train_samples.push(TrainingSample {
                    features,
                    base_side_probability: ev.raw.p_up as f32,
                    side_observed: series.resolved_yes,
                });
            }
        }

        if tick.ts_ns < next_decision_ns || tick.ts_ns >= entry_deadline_ns {
            continue;
        }
        next_decision_ns = tick.ts_ns + decision_dt_ns;

        let state = ExoState {
            spot,
            market: series.meta,
            now_ns: tick.ts_ns,
        };
        let Some(ev) = model.evaluate(&state, false) else {
            continue;
        };
        pass.had_belief = true;
        let (Some(mid), true) = (tick.mid(), tick.yes_ask > tick.yes_bid) else {
            continue;
        };
        pass.decisions.push(Decision {
            tick_idx: i,
            ts_ns: tick.ts_ns,
            p_up: ev.p,
            mid,
            yes_bid: tick.yes_bid as f64,
            yes_ask: tick.yes_ask as f64,
        });
    }

    pass
}

/// Execute one (latency, threshold) combination against a shared belief pass.
fn execute(
    series: &MarketSeries,
    pass: &BeliefPass,
    latency_ms: u64,
    edge_threshold: f64,
    cfg: &HarnessConfig,
) -> Option<TradeRecord> {
    let close_ns = series.meta.close_ts_ns;
    let latency_ns = latency_ms as i64 * 1_000_000;

    for d in &pass.decisions {
        // Edge per side against touch prices (entry test; fill walks depth).
        let edge_yes = d.p_up - d.yes_ask;
        let edge_no = d.yes_bid - d.p_up; // buy NO at 1 - bid
        let (side, edge) = if edge_yes >= edge_no {
            (Side::Yes, edge_yes)
        } else {
            (Side::No, edge_no)
        };
        if edge < edge_threshold {
            continue;
        }

        // Latency: fill against the book as it actually is at T + latency.
        let fill_at_ns = d.ts_ns + latency_ns;
        let Some(fill_tick) = series.ticks[d.tick_idx..]
            .iter()
            .find(|t| t.ts_ns >= fill_at_ns && t.ts_ns <= close_ns)
        else {
            continue;
        };

        let Some((avg_price, shares)) = fill(fill_tick, side, cfg.notional_usdc) else {
            continue;
        };
        let fee = avg_price * shares * cfg.taker_fee_bps / 10_000.0;
        let won = match side {
            Side::Yes => series.resolved_yes,
            Side::No => !series.resolved_yes,
        };
        let payout = if won { 1.0 } else { 0.0 };
        let pnl = shares * (payout - avg_price) - fee;
        let mark_60s = series.ticks[d.tick_idx..]
            .iter()
            .find(|t| t.ts_ns >= fill_tick.ts_ns + 60_000_000_000)
            .and_then(|t| t.mid())
            .map(|m| match side {
                Side::Yes => m,
                Side::No => 1.0 - m,
            });

        return Some(TradeRecord {
            side,
            decision_ts_ns: d.ts_ns,
            fill_ts_ns: fill_tick.ts_ns,
            avg_price,
            shares,
            fee,
            p_exo: d.p_up,
            mid_at_decision: d.mid,
            pnl,
            won,
            mark_60s,
        });
    }
    None
}

/// Run one market across a latency x threshold grid, computing the belief
/// pass once. Output is row-major: `[latency_idx * thresholds.len() + thr_idx]`.
pub fn run_market_grid(
    series: &MarketSeries,
    spot: &SpotHistory,
    model: &AlphaModel,
    cfg: &HarnessConfig,
    latencies_ms: &[u64],
    edge_thresholds: &[f64],
) -> Vec<MarketRunOutput> {
    let mut outputs = Vec::with_capacity(latencies_ms.len() * edge_thresholds.len());
    if series.ticks.is_empty() {
        for _ in 0..latencies_ms.len() * edge_thresholds.len() {
            outputs.push(MarketRunOutput::default());
        }
        return outputs;
    }
    let pass = belief_pass(series, spot, model, cfg);
    let mut first = true;
    for &latency_ms in latencies_ms {
        for &threshold in edge_thresholds {
            // Training samples are identical across grid cells; attach them
            // to the first cell only so the caller doesn't dedupe.
            let train_samples = if first {
                pass.train_samples.clone()
            } else {
                Vec::new()
            };
            first = false;
            outputs.push(MarketRunOutput {
                trade: execute(series, &pass, latency_ms, threshold, cfg),
                samples: pass.samples.clone(),
                had_belief: pass.had_belief,
                train_samples,
            });
        }
    }
    outputs
}

pub fn run_market(
    series: &MarketSeries,
    spot: &SpotHistory,
    model: &AlphaModel,
    cfg: &HarnessConfig,
) -> MarketRunOutput {
    run_market_grid(
        series,
        spot,
        model,
        cfg,
        &[cfg.latency_ms],
        &[cfg.edge_threshold],
    )
    .into_iter()
    .next()
    .unwrap_or_default()
}

/// Walk depth on the relevant side for `notional` dollars. Buying YES walks
/// the YES asks; buying NO is synthesized as `1 - yes_bid` walking the YES
/// bids (the NO leg is not in the tape). Returns (avg_price, shares).
fn fill(tick: &BookTick, side: Side, notional: f64) -> Option<(f64, f64)> {
    let levels: Vec<(f64, f64)> = match side {
        Side::Yes => tick
            .asks
            .iter()
            .filter(|l| valid(l))
            .map(|l| (l.price as f64, l.size as f64))
            .collect(),
        Side::No => tick
            .bids
            .iter()
            .filter(|l| valid(l))
            .map(|l| (1.0 - l.price as f64, l.size as f64))
            .collect(),
    };
    if levels.is_empty() {
        return None;
    }

    let mut remaining = notional;
    let mut cost = 0.0;
    let mut shares = 0.0;
    for (price, size) in levels {
        if remaining <= 1e-9 || price <= 0.0 {
            break;
        }
        let level_notional = price * size;
        let take = remaining.min(level_notional);
        let qty = take / price;
        cost += take;
        shares += qty;
        remaining -= take;
    }
    if shares <= 1e-9 {
        return None;
    }
    Some((cost / shares, shares))
}

fn valid(l: &BookLevel) -> bool {
    l.price > 0.0 && l.price < 1.0 && l.size > 0.0
}
