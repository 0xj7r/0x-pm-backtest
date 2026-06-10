//! Per-market replay: belief at decision instants, latency-shifted fills,
//! depth-walked costs, settlement at resolution.
//!
//! The belief pass is computed once per market and shared across the
//! latency x threshold grid (beliefs depend on neither; only entries and
//! fills do), which is what makes parameter sweeps affordable.

use super::types::{
    BookTick, EntryMode, HarnessConfig, MarketRunOutput, MarketSeries, ProbSample, Side,
    TradeRecord,
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
    yes_ask: f64,
    no_buy: f64,
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
        let Some(no_buy) = tick.no_buy_price() else {
            continue;
        };
        pass.decisions.push(Decision {
            tick_idx: i,
            ts_ns: tick.ts_ns,
            p_up: ev.p,
            mid,
            yes_ask: tick.yes_ask as f64,
            no_buy,
        });
    }

    pass
}

/// Execute one (latency, threshold) combination against a shared belief
/// pass, laddering up to `max_clips` entries separated by the cooldown.
fn execute(
    series: &MarketSeries,
    pass: &BeliefPass,
    latency_ms: u64,
    edge_threshold: f64,
    cfg: &HarnessConfig,
) -> Vec<TradeRecord> {
    let close_ns = series.meta.close_ts_ns;
    let latency_ns = latency_ms as i64 * 1_000_000;
    let cooldown_ns = cfg.clip_cooldown_ms as i64 * 1_000_000;
    let max_clips = cfg.max_clips.max(1) as usize;
    let mut trades: Vec<TradeRecord> = Vec::new();
    let mut next_entry_ns = i64::MIN;

    for d in &pass.decisions {
        if trades.len() >= max_clips {
            break;
        }
        if d.ts_ns < next_entry_ns {
            continue;
        }
        // Edge per side against touch prices (entry test; fill walks depth).
        let edge_yes = d.p_up - d.yes_ask;
        let edge_no = (1.0 - d.p_up) - d.no_buy; // real NO ask when loaded
        let (side, edge) = if edge_yes >= edge_no {
            (Side::Yes, edge_yes)
        } else {
            (Side::No, edge_no)
        };
        if edge < edge_threshold {
            continue;
        }
        if cfg.entry_mode == EntryMode::Aligned {
            // Directional entries require the book to already favour the
            // same side (we ride agreement, not disagreement).
            let side_mid = match side {
                Side::Yes => d.mid,
                Side::No => 1.0 - d.mid,
            };
            if side_mid < cfg.align_min_mid {
                continue;
            }
        }

        // Latency: fill against the book as it actually is at T + latency.
        let fill_at_ns = d.ts_ns + latency_ns;
        let Some(fill_tick) = series.ticks[d.tick_idx..]
            .iter()
            .find(|t| t.ts_ns >= fill_at_ns && t.ts_ns <= close_ns)
        else {
            continue;
        };

        let Some((avg_price, shares)) =
            fill(fill_tick, side, cfg.notional_usdc, cfg.depth_capture_frac)
        else {
            continue;
        };
        let entry_fee = avg_price * shares * cfg.taker_fee_bps / 10_000.0;
        let won = match side {
            Side::Yes => series.resolved_yes,
            Side::No => !series.resolved_yes,
        };
        let payout = if won { 1.0 } else { 0.0 };

        // Optional early exit: cross the spread at the first tick past the
        // horizon; any unsold remainder settles at resolution.
        let mut exit_price = None;
        let mut exit_fee = 0.0;
        let mut proceeds_pnl = None;
        if cfg.exit_after_s > 0 {
            let exit_at_ns = fill_tick.ts_ns + cfg.exit_after_s as i64 * 1_000_000_000;
            if let Some(exit_tick) = series.ticks[d.tick_idx..]
                .iter()
                .find(|t| t.ts_ns >= exit_at_ns && t.ts_ns <= close_ns)
                && let Some((px, sold)) =
                    sell_fill(exit_tick, side, shares, cfg.depth_capture_frac)
            {
                exit_fee = px * sold * cfg.taker_fee_bps / 10_000.0;
                let remainder = (shares - sold).max(0.0);
                proceeds_pnl =
                    Some(sold * (px - avg_price) + remainder * (payout - avg_price));
                exit_price = Some(px);
            }
        }
        let fee = entry_fee + exit_fee;
        let pnl = proceeds_pnl.unwrap_or(shares * (payout - avg_price)) - fee;
        next_entry_ns = d.ts_ns + cooldown_ns;
        let mark_60s = series.ticks[d.tick_idx..]
            .iter()
            .find(|t| t.ts_ns >= fill_tick.ts_ns + 60_000_000_000)
            .and_then(|t| t.mid())
            .map(|m| match side {
                Side::Yes => m,
                Side::No => 1.0 - m,
            });

        trades.push(TradeRecord {
            side,
            decision_ts_ns: d.ts_ns,
            fill_ts_ns: fill_tick.ts_ns,
            avg_price,
            shares,
            fee,
            p_exo: d.p_up,
            mid_at_decision: d.mid,
            pnl: { pnl },
            won: if exit_price.is_some() { pnl > 0.0 } else { won },
            exit_price,
            mark_60s,
        });

        // Convexity hedge: buy the opposite cheap tail (held to resolution;
        // it exists to bound the crossed-mid disaster, not to be traded).
        if cfg.tail_max_price > 0.0 && cfg.tail_frac > 0.0 {
            let tail_side = side.opposite();
            let tail_touch = match tail_side {
                Side::Yes => fill_tick.yes_ask as f64,
                Side::No => fill_tick.no_buy_price().unwrap_or(1.0),
            };
            if tail_touch > 0.0 && tail_touch <= cfg.tail_max_price
                && let Some((tail_price, tail_shares)) =
                    fill(fill_tick, tail_side, cfg.notional_usdc * cfg.tail_frac, cfg.depth_capture_frac)
            {
                let tail_fee = tail_price * tail_shares * cfg.taker_fee_bps / 10_000.0;
                let tail_won = match tail_side {
                    Side::Yes => series.resolved_yes,
                    Side::No => !series.resolved_yes,
                };
                let tail_payout = if tail_won { 1.0 } else { 0.0 };
                trades.push(TradeRecord {
                    side: tail_side,
                    decision_ts_ns: d.ts_ns,
                    fill_ts_ns: fill_tick.ts_ns,
                    avg_price: tail_price,
                    shares: tail_shares,
                    fee: tail_fee,
                    p_exo: d.p_up,
                    mid_at_decision: d.mid,
                    pnl: tail_shares * (tail_payout - tail_price) - tail_fee,
                    won: tail_won,
                    exit_price: None,
                    mark_60s: None,
                });
            }
        }
    }
    trades
}

/// Sell `shares` by crossing the spread: selling YES walks the YES bids;
/// selling NO (synthesized) means buying back YES, i.e. proceeds per share
/// are `1 - ask` walking the YES asks. Returns (avg_proceeds_price, sold).
fn sell_fill(tick: &BookTick, side: Side, shares: f64, capture_frac: f64) -> Option<(f64, f64)> {
    let levels: Vec<(f64, f64)> = match side {
        Side::Yes => tick
            .bids
            .iter()
            .filter(|l| valid(l))
            .map(|l| (l.price as f64, l.size as f64))
            .collect(),
        Side::No if tick.has_real_no() => tick
            .no_bids
            .iter()
            .filter(|l| valid(l))
            .map(|l| (l.price as f64, l.size as f64))
            .collect(),
        Side::No => tick
            .asks
            .iter()
            .filter(|l| valid(l))
            .map(|l| (1.0 - l.price as f64, l.size as f64))
            .collect(),
    };
    if levels.is_empty() {
        return None;
    }
    let capture = capture_frac.clamp(0.0, 1.0);
    let mut remaining = shares;
    let mut proceeds = 0.0;
    let mut sold = 0.0;
    for (price, size) in levels {
        if remaining <= 1e-9 {
            break;
        }
        let qty = remaining.min(size * capture);
        proceeds += qty * price;
        sold += qty;
        remaining -= qty;
    }
    if sold <= 1e-9 {
        return None;
    }
    Some((proceeds / sold, sold))
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
    let regime = crate::regime::classify(spot, series.meta.open_ts_ns);
    let mut min_pair_cost: Option<f64> = None;
    let mut real_no_ticks = 0usize;
    for t in &series.ticks {
        if t.has_real_no() {
            real_no_ticks += 1;
        }
        if let Some(pc) = t.pair_cost() {
            min_pair_cost = Some(min_pair_cost.map_or(pc, |m: f64| m.min(pc)));
        }
    }
    let real_no_coverage = real_no_ticks as f64 / series.ticks.len().max(1) as f64;
    let calm_blocked =
        cfg.skip_calm && regime == Some(crate::regime::Regime::CalmLowVol);
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
                trades: if calm_blocked {
                    Vec::new()
                } else {
                    execute(series, &pass, latency_ms, threshold, cfg)
                },
                samples: pass.samples.clone(),
                had_belief: pass.had_belief,
                train_samples,
                regime,
                min_pair_cost,
                real_no_coverage,
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
fn fill(tick: &BookTick, side: Side, notional: f64, capture_frac: f64) -> Option<(f64, f64)> {
    let levels: Vec<(f64, f64)> = match side {
        Side::Yes => tick
            .asks
            .iter()
            .filter(|l| valid(l))
            .map(|l| (l.price as f64, l.size as f64))
            .collect(),
        // Real Down-token asks when loaded; synthetic complement otherwise.
        Side::No if tick.has_real_no() => tick
            .no_asks
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

    let capture = capture_frac.clamp(0.0, 1.0);
    let mut remaining = notional;
    let mut cost = 0.0;
    let mut shares = 0.0;
    for (price, size) in levels {
        if remaining <= 1e-9 || price <= 0.0 {
            break;
        }
        let level_notional = price * size * capture;
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
