//! Per-market replay: belief at decision instants, latency-shifted fills,
//! depth-walked costs, settlement at resolution.

use super::types::{BookTick, HarnessConfig, MarketRunOutput, MarketSeries, ProbSample, Side, TradeRecord};
use crate::model::{AlphaModelConfig, belief};
use crate::state::ExoState;
use pm_types::SpotHistory;
use pm_types::tape::BookLevel;

/// Checkpoints (seconds since window open) where log-loss samples are taken.
const SAMPLE_OFFSETS_S: [i64; 4] = [60, 120, 180, 240];

pub fn run_market(
    series: &MarketSeries,
    spot: &SpotHistory,
    model_cfg: &AlphaModelConfig,
    cfg: &HarnessConfig,
) -> MarketRunOutput {
    let mut out = MarketRunOutput::default();
    if series.ticks.is_empty() {
        return out;
    }

    let open_ns = series.meta.open_ts_ns;
    let close_ns = series.meta.close_ts_ns;
    let entry_deadline_ns = close_ns - cfg.stop_before_close_s as i64 * 1_000_000_000;
    let decision_dt_ns = (cfg.decision_dt_ms.max(1) as i64) * 1_000_000;
    let latency_ns = cfg.latency_ms as i64 * 1_000_000;

    let mut next_decision_ns = open_ns;
    let mut next_sample = 0usize;
    let mut entered = false;

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
            if let (Some(b), Some(mid)) = (belief(&state, model_cfg), tick.mid()) {
                out.had_belief = true;
                out.samples.push(ProbSample {
                    ts_ns: tick.ts_ns,
                    p_exo: b.p_up,
                    p_book: mid,
                    resolved_yes: series.resolved_yes,
                });
            }
            next_sample += 1;
        }

        if entered || tick.ts_ns < next_decision_ns || tick.ts_ns >= entry_deadline_ns {
            continue;
        }
        next_decision_ns = tick.ts_ns + decision_dt_ns;

        let state = ExoState {
            spot,
            market: series.meta,
            now_ns: tick.ts_ns,
        };
        let Some(b) = belief(&state, model_cfg) else {
            continue;
        };
        out.had_belief = true;

        let (Some(mid), true) = (tick.mid(), tick.yes_ask > tick.yes_bid) else {
            continue;
        };

        // Edge per side against touch prices (entry test; fill walks depth).
        let edge_yes = b.p_up - tick.yes_ask as f64;
        let edge_no = tick.yes_bid as f64 - b.p_up; // buy NO at 1 - bid
        let (side, edge) = if edge_yes >= edge_no {
            (Side::Yes, edge_yes)
        } else {
            (Side::No, edge_no)
        };
        if edge < cfg.edge_threshold {
            continue;
        }

        // Latency: fill against the book as it actually is at T + latency.
        let fill_at_ns = tick.ts_ns + latency_ns;
        let Some(fill_tick) = series.ticks[i..]
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

        out.trade = Some(TradeRecord {
            side,
            decision_ts_ns: tick.ts_ns,
            fill_ts_ns: fill_tick.ts_ns,
            avg_price,
            shares,
            fee,
            p_exo: b.p_up,
            mid_at_decision: mid,
            pnl,
            won,
        });
        entered = true;
    }

    out
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
