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
    /// Continuation-model belief, present only when a dir model is loaded,
    /// the run is Aligned-mode, and a move is in progress at this instant.
    dir_p_up: Option<f64>,
    mid: f64,
    yes_ask: f64,
    no_buy: f64,
}

struct BeliefPass {
    decisions: Vec<Decision>,
    samples: Vec<ProbSample>,
    train_samples: Vec<TrainingSample>,
    dir_samples: Vec<crate::directional::DirSample>,
    had_belief: bool,
    /// True when a continuation model gates Aligned entries this run.
    dir_model_active: bool,
}

fn belief_pass(
    series: &MarketSeries,
    spot: &SpotHistory,
    perp: Option<&crate::state::PerpState>,
    ref_spot: Option<&SpotHistory>,
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
        dir_samples: Vec::new(),
        had_belief: false,
        dir_model_active: model.dir_model.is_some() && cfg.entry_mode == EntryMode::Aligned,
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
                perp,
                ref_spot,
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
                perp,
                ref_spot,
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
                // Directional sample only when a move is actually in
                // progress (>= 0.5 bar-sigma over the trailing 60s).
                let dirf = crate::directional::dir_features(&state, ev.raw.sigma_bar_bps);
                let t60 = dirf.values[8];
                if t60.abs() >= 0.5 {
                    pass.dir_samples.push(crate::directional::DirSample {
                        ts_ns: tick.ts_ns,
                        features: dirf,
                        move_up: t60 > 0.0,
                        resolved_yes: series.resolved_yes,
                    });
                }
            }
        }

        if tick.ts_ns < next_decision_ns || tick.ts_ns >= entry_deadline_ns {
            continue;
        }
        next_decision_ns = tick.ts_ns + decision_dt_ns;

        let state = ExoState {
            spot,
            perp,
            ref_spot,
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
        let dir_p_up = match (&model.dir_model, cfg.entry_mode) {
            (Some(dm), EntryMode::Aligned) => {
                dm.p_up(&crate::directional::dir_features(&state, ev.raw.sigma_bar_bps))
            }
            _ => None,
        };
        pass.decisions.push(Decision {
            tick_idx: i,
            ts_ns: tick.ts_ns,
            p_up: ev.p,
            dir_p_up,
            mid,
            yes_ask: tick.yes_ask as f64,
            no_buy,
        });
    }

    pass
}

/// First entry of a market while it can still be pair-completed.
struct OpenLeg {
    trade_idx: usize,
    side: Side,
    avg_price: f64,
    shares: f64,
    entry_fee: f64,
    /// Decisions at-or-after this instant can no longer complete the pair
    /// (the leg's scheduled exit, or close when held to resolution).
    complete_until_ns: i64,
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
    let pair_completion = cfg.pair_completion_margin > 0.0;
    let mut trades: Vec<TradeRecord> = Vec::new();
    let mut next_entry_ns = i64::MIN;
    let mut n_clips = 0usize;
    let mut leg1: Option<OpenLeg> = None;
    let mut first_entry_done = false;
    // Event-based re-entry: disarmed after each entry; re-armed only once a
    // later decision shows the dislocation closed (both edges below the
    // re-arm level). Inert when rearm_edge is 0.
    let rearm_active = cfg.rearm_edge > 0.0;
    let mut armed = true;

    for d in &pass.decisions {
        // Pair completion: buy the opposite token once its ask locks at
        // least the margin against leg 1's cost; both legs then settle.
        if let Some(l1) = leg1.as_ref().filter(|l| d.ts_ns < l.complete_until_ns) {
            let opp = l1.side.opposite();
            let opp_ask = match opp {
                Side::Yes => d.yes_ask,
                Side::No => d.no_buy,
            };
            if opp_ask > 0.0
                && opp_ask <= 1.0 - l1.avg_price - cfg.pair_completion_margin
                && let Some(fill_tick) = series.ticks[d.tick_idx..]
                    .iter()
                    .find(|t| t.ts_ns >= d.ts_ns + latency_ns && t.ts_ns <= close_ns)
                && let Some((px2, sh2)) = fill_shares(
                    fill_tick,
                    opp,
                    l1.shares,
                    cfg.depth_capture_frac,
                    cfg.skip_touch_level,
                )
            {
                let fee2 = px2 * sh2 * cfg.taker_fee_bps / 10_000.0;
                let won2 = match opp {
                    Side::Yes => series.resolved_yes,
                    Side::No => !series.resolved_yes,
                };
                let payout2 = if won2 { 1.0 } else { 0.0 };
                // Leg 1 reverts to hold-to-resolution: exactly one leg pays
                // $1, netting the locked profit across the pair.
                let payout1 = 1.0 - payout2;
                let t1 = &mut trades[l1.trade_idx];
                t1.pnl = l1.shares * (payout1 - l1.avg_price) - l1.entry_fee;
                t1.fee = l1.entry_fee;
                t1.won = payout1 > 0.5;
                t1.exit_price = None;
                t1.pnl_exit_mid_optimistic = None;
                t1.exit_filled_at_mid = None;
                trades.push(TradeRecord {
                    side: opp,
                    decision_ts_ns: d.ts_ns,
                    fill_ts_ns: fill_tick.ts_ns,
                    avg_price: px2,
                    shares: sh2,
                    fee: fee2,
                    p_exo: d.p_up,
                    mid_at_decision: d.mid,
                    pnl: sh2 * (payout2 - px2) - fee2,
                    won: won2,
                    exit_price: None,
                    mark_60s: None,
                    pnl_exit_mid_optimistic: None,
                    is_completion: true,
                    exit_filled_at_mid: None,
                });
                leg1 = None;
                continue;
            }
        }
        if n_clips >= max_clips {
            if pair_completion && leg1.is_some() {
                continue; // out of clips, but the pair may still complete
            }
            break;
        }
        // Aligned runs with a continuation model trade ITS belief, and only
        // when a move is in progress; the fade keeps the exogenous belief.
        let p_up = match (cfg.entry_mode, d.dir_p_up) {
            (EntryMode::Aligned, Some(p)) => p,
            (EntryMode::Aligned, None) if pass.dir_model_active => continue,
            _ => d.p_up,
        };
        // Edge per side against touch prices (entry test; fill walks depth).
        let edge_yes = p_up - d.yes_ask;
        let edge_no = (1.0 - p_up) - d.no_buy; // real NO ask when loaded
        // Disarmed: watch (even through the cooldown) for the dislocation to
        // close; only a later threshold crossing may then enter again.
        if rearm_active && !armed {
            if edge_yes < cfg.rearm_edge && edge_no < cfg.rearm_edge {
                armed = true;
            }
            continue;
        }
        if d.ts_ns < next_entry_ns {
            continue;
        }
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

        // Sizing: flat clip, or Kelly-style scaling on the
        // reliability-discounted edge with per-trade variance equalized.
        let entry_cost = match side {
            Side::Yes => d.yes_ask,
            Side::No => d.no_buy,
        };
        let p_side = match side {
            Side::Yes => p_up,
            Side::No => 1.0 - p_up,
        };
        let notional = if cfg.kelly_sizing {
            // Half-trust the belief (shrink toward the market's price; the
            // belief's claimed edge historically realizes at roughly half).
            let p_eff = entry_cost + 0.5 * (p_side - entry_cost);
            let kelly = ((p_eff - entry_cost) / (1.0 - entry_cost).max(1e-6)).max(0.0);
            let edge_factor = (kelly / 0.16).clamp(0.0, 1.0);
            let var_factor =
                (entry_cost / (p_eff * (1.0 - p_eff)).sqrt().max(1e-6)).clamp(0.0, 1.0);
            (cfg.notional_usdc * edge_factor * var_factor).max(0.0)
        } else {
            cfg.notional_usdc
        };
        if notional < 1.0 {
            continue; // sized below the venue's practical minimum
        }

        // Latency: fill against the book as it actually is at T + latency.
        let fill_at_ns = d.ts_ns + latency_ns;
        let Some(fill_tick) = series.ticks[d.tick_idx..]
            .iter()
            .find(|t| t.ts_ns >= fill_at_ns && t.ts_ns <= close_ns)
        else {
            continue;
        };

        let entry_cap = (cfg.min_marginal_edge > 0.0).then(|| p_side - cfg.min_marginal_edge);
        let Some((avg_price, shares)) = fill(
            fill_tick,
            side,
            notional,
            cfg.depth_capture_frac,
            cfg.skip_touch_level,
            entry_cap,
        ) else {
            continue;
        };
        let entry_fee = avg_price * shares * cfg.taker_fee_bps / 10_000.0;
        let won = match side {
            Side::Yes => series.resolved_yes,
            Side::No => !series.resolved_yes,
        };
        let payout = if won { 1.0 } else { 0.0 };

        // Optional early exit: cross the spread at the first tick past the
        // horizon; any unsold remainder settles at resolution. With
        // exit_at_mid, rest an ask at the side mid instead: the exact
        // conditional fill (a later bid must cross the level before close,
        // else settle) drives pnl, with the always-fills optimistic bound
        // recorded alongside.
        let mut exit_price = None;
        let mut exit_fee = 0.0;
        let mut proceeds_pnl = None;
        let mut pnl_exit_mid_optimistic = None;
        let mut exit_filled_at_mid = None;
        if cfg.exit_after_s > 0 {
            let exit_at_ns = fill_tick.ts_ns + cfg.exit_after_s as i64 * 1_000_000_000;
            if cfg.passive_exit_timeout_s > 0 {
                // Hybrid: rest at the side mid; if no bid crosses within the
                // timeout, convert to a crossing exit against the book as of
                // the timeout (NOT the original exit tick).
                if let Some(rel) = series.ticks[d.tick_idx..]
                    .iter()
                    .position(|t| t.ts_ns >= exit_at_ns && t.ts_ns <= close_ns)
                {
                    let exit_idx = d.tick_idx + rel;
                    let exit_tick = &series.ticks[exit_idx];
                    let deadline_ns =
                        exit_tick.ts_ns + cfg.passive_exit_timeout_s as i64 * 1_000_000_000;
                    match side_mid(exit_tick, side) {
                        Some(level)
                            if series.ticks[exit_idx..]
                                .iter()
                                .take_while(|t| t.ts_ns <= close_ns && t.ts_ns <= deadline_ns)
                                .any(|t| side_bid(t, side) >= level - 1e-9) =>
                        {
                            exit_fee = level * shares * cfg.taker_fee_bps / 10_000.0;
                            proceeds_pnl = Some(shares * (level - avg_price));
                            exit_price = Some(level);
                            exit_filled_at_mid = Some(true);
                        }
                        Some(_) => {
                            // Timed out: cross at the first tick past the
                            // deadline; settle if none remains.
                            exit_filled_at_mid = Some(false);
                            if let Some(conv_tick) = series.ticks[exit_idx..]
                                .iter()
                                .find(|t| t.ts_ns >= deadline_ns && t.ts_ns <= close_ns)
                                && let Some((px, sold)) = sell_fill(
                                    conv_tick,
                                    side,
                                    shares,
                                    cfg.depth_capture_frac,
                                    cfg.skip_touch_level,
                                )
                            {
                                exit_fee = px * sold * cfg.taker_fee_bps / 10_000.0;
                                let remainder = (shares - sold).max(0.0);
                                proceeds_pnl = Some(
                                    sold * (px - avg_price) + remainder * (payout - avg_price),
                                );
                                exit_price = Some(px);
                            }
                        }
                        None => {
                            // No mid to rest at: champion crossing exit at
                            // the original exit tick.
                            if let Some((px, sold)) = sell_fill(
                                exit_tick,
                                side,
                                shares,
                                cfg.depth_capture_frac,
                                cfg.skip_touch_level,
                            ) {
                                exit_fee = px * sold * cfg.taker_fee_bps / 10_000.0;
                                let remainder = (shares - sold).max(0.0);
                                proceeds_pnl = Some(
                                    sold * (px - avg_price) + remainder * (payout - avg_price),
                                );
                                exit_price = Some(px);
                            }
                        }
                    }
                }
            } else if cfg.exit_at_mid {
                if let Some(rel) = series.ticks[d.tick_idx..]
                    .iter()
                    .position(|t| t.ts_ns >= exit_at_ns && t.ts_ns <= close_ns)
                    && let Some(level) = side_mid(&series.ticks[d.tick_idx + rel], side)
                {
                    let mid_fee = level * shares * cfg.taker_fee_bps / 10_000.0;
                    pnl_exit_mid_optimistic =
                        Some(shares * (level - avg_price) - entry_fee - mid_fee);
                    let crossed = series.ticks[d.tick_idx + rel..]
                        .iter()
                        .take_while(|t| t.ts_ns <= close_ns)
                        .any(|t| side_bid(t, side) >= level - 1e-9);
                    if crossed {
                        exit_fee = mid_fee;
                        proceeds_pnl = Some(shares * (level - avg_price));
                        exit_price = Some(level);
                    }
                }
            } else if let Some(exit_tick) = series.ticks[d.tick_idx..]
                .iter()
                .find(|t| t.ts_ns >= exit_at_ns && t.ts_ns <= close_ns)
                && let Some((px, sold)) = sell_fill(
                    exit_tick,
                    side,
                    shares,
                    cfg.depth_capture_frac,
                    cfg.skip_touch_level,
                )
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
        n_clips += 1;
        if rearm_active {
            armed = false;
        }
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
            pnl_exit_mid_optimistic,
            is_completion: false,
            exit_filled_at_mid,
        });
        if pair_completion && !first_entry_done {
            first_entry_done = true;
            leg1 = Some(OpenLeg {
                trade_idx: trades.len() - 1,
                side,
                avg_price,
                shares,
                entry_fee,
                complete_until_ns: if cfg.exit_after_s > 0 {
                    fill_tick.ts_ns + cfg.exit_after_s as i64 * 1_000_000_000
                } else {
                    close_ns
                },
            });
        }

        // Convexity hedge: buy the opposite cheap tail (held to resolution;
        // it exists to bound the crossed-mid disaster, not to be traded).
        if cfg.tail_max_price > 0.0 && cfg.tail_frac > 0.0 {
            let tail_side = side.opposite();
            let tail_touch = match tail_side {
                Side::Yes => fill_tick.yes_ask as f64,
                Side::No => fill_tick.no_buy_price().unwrap_or(1.0),
            };
            if tail_touch > 0.0 && tail_touch <= cfg.tail_max_price
                && let Some((tail_price, tail_shares)) = fill(
                    fill_tick,
                    tail_side,
                    cfg.notional_usdc * cfg.tail_frac,
                    cfg.depth_capture_frac,
                    cfg.skip_touch_level,
                    None,
                )
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
                    pnl_exit_mid_optimistic: None,
                    is_completion: false,
                    exit_filled_at_mid: None,
                });
            }
        }
    }
    trades
}

/// Sell `shares` by crossing the spread: selling YES walks the YES bids;
/// selling NO (synthesized) means buying back YES, i.e. proceeds per share
/// are `1 - ask` walking the YES asks. Returns (avg_proceeds_price, sold).
fn sell_fill(
    tick: &BookTick,
    side: Side,
    shares: f64,
    capture_frac: f64,
    skip_touch: bool,
) -> Option<(f64, f64)> {
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
    let levels = if skip_touch && levels.len() > 1 {
        levels[1..].to_vec()
    } else {
        levels
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

/// Side-oriented mid: the natural resting-ask level for a passive exit.
fn side_mid(tick: &BookTick, side: Side) -> Option<f64> {
    match side {
        Side::Yes => tick.mid(),
        Side::No if tick.has_real_no() => Some(((tick.no_bid + tick.no_ask) / 2.0) as f64),
        Side::No => tick.mid().map(|m| 1.0 - m),
    }
}

/// Side-oriented best bid: what could lift a resting ask on this side.
/// Synthetic NO bid is `1 - yes_ask` (selling NO = buying back YES).
fn side_bid(tick: &BookTick, side: Side) -> f64 {
    match side {
        Side::Yes => tick.yes_bid as f64,
        Side::No if tick.has_real_no() => tick.no_bid as f64,
        Side::No if tick.yes_ask > 0.0 && tick.yes_ask < 1.0 => 1.0 - tick.yes_ask as f64,
        Side::No => 0.0,
    }
}

/// Buy `target_shares` (not dollars) walking the relevant ask ladder; used
/// by pair completion to match leg 1's share count. Returns (avg_price, shares).
fn fill_shares(
    tick: &BookTick,
    side: Side,
    target_shares: f64,
    capture_frac: f64,
    skip_touch: bool,
) -> Option<(f64, f64)> {
    let levels: Vec<(f64, f64)> = match side {
        Side::Yes => tick
            .asks
            .iter()
            .filter(|l| valid(l))
            .map(|l| (l.price as f64, l.size as f64))
            .collect(),
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
    let levels = if skip_touch && levels.len() > 1 {
        levels[1..].to_vec()
    } else {
        levels
    };
    if levels.is_empty() {
        return None;
    }
    let capture = capture_frac.clamp(0.0, 1.0);
    let mut remaining = target_shares;
    let mut cost = 0.0;
    let mut shares = 0.0;
    for (price, size) in levels {
        if remaining <= 1e-9 || price <= 0.0 {
            break;
        }
        let qty = remaining.min(size * capture);
        cost += qty * price;
        shares += qty;
        remaining -= qty;
    }
    if shares <= 1e-9 {
        return None;
    }
    Some((cost / shares, shares))
}

/// Run one market across a latency x threshold grid, computing the belief
/// pass once. Output is row-major: `[latency_idx * thresholds.len() + thr_idx]`.
pub fn run_market_grid(
    series: &MarketSeries,
    spot: &SpotHistory,
    perp: Option<&crate::state::PerpState>,
    ref_spot: Option<&SpotHistory>,
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
    let pass = belief_pass(series, spot, perp, ref_spot, model, cfg);
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
    let calm = regime == Some(crate::regime::Regime::CalmLowVol);
    let calm_blocked = (cfg.skip_calm && calm) || (cfg.only_calm && !calm);
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
            let dir_samples = if first {
                pass.dir_samples.clone()
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
                dir_samples,
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
        None,
        None,
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
fn fill(
    tick: &BookTick,
    side: Side,
    notional: f64,
    capture_frac: f64,
    skip_touch: bool,
    max_price: Option<f64>,
) -> Option<(f64, f64)> {
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
    let levels = if skip_touch && levels.len() > 1 {
        levels[1..].to_vec()
    } else {
        levels
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
        if let Some(cap) = max_price
            && price > cap
        {
            break; // marginal edge below the floor: leave the rest
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
