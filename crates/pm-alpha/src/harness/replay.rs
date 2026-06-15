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
    sigma_bar_bps: f64,
    /// 60s change in perp-minus-spot basis, bps of spot. 0 when no perp.
    basis_mom_60s_bps: f64,
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
        let basis_mom_60s_bps = perp
            .and_then(|p| {
                let bn = p.basis_frac(spot, tick.ts_ns)?;
                let bp = p.basis_frac(spot, tick.ts_ns - 60_000_000_000)?;
                Some((bn - bp) * 1e4)
            })
            .unwrap_or(0.0);
        pass.decisions.push(Decision {
            tick_idx: i,
            ts_ns: tick.ts_ns,
            p_up: ev.p,
            dir_p_up,
            mid,
            yes_ask: tick.yes_ask as f64,
            no_buy,
            sigma_bar_bps: ev.raw.sigma_bar_bps,
            basis_mom_60s_bps,
        });
    }

    pass
}

/// Polymarket crypto taker fee curve: rate * p * (1-p) per share, charged on
/// every aggressive fill at that leg's own fill price. 0 rate = exactly 0.
pub(crate) fn curve_fee(rate: f64, price: f64, shares: f64) -> f64 {
    rate * price * (1.0 - price) * shares
}

/// Trailing mins of the entry side's ask over the diagnostic windows
/// (10/20/40s) and the configured stability window before the decision tick.
/// Returns (min_10s, min_20s, min_40s, gate_passed).
fn trailing_stability(
    series: &MarketSeries,
    tick_idx: usize,
    ts_ns: i64,
    side: Side,
    side_ask_now: f64,
    cfg: &HarnessConfig,
) -> (f64, f64, f64, bool) {
    const DIAG_NS: [i64; 3] = [10_000_000_000, 20_000_000_000, 40_000_000_000];
    let cfg_ns = cfg.entry_stability_s as i64 * 1_000_000_000;
    let scan_ns = cfg_ns.max(DIAG_NS[2]);
    let mut mins = [side_ask_now; 3];
    let mut cfg_min = side_ask_now;
    for t in series.ticks[..=tick_idx].iter().rev() {
        let age_ns = ts_ns - t.ts_ns;
        if age_ns > scan_ns {
            break;
        }
        let ask = match side {
            Side::Yes if t.yes_ask > 0.0 && t.yes_ask < 1.0 => Some(t.yes_ask as f64),
            Side::Yes => None,
            Side::No => t.no_buy_price(),
        };
        let Some(ask) = ask else { continue };
        for (k, w_ns) in DIAG_NS.iter().enumerate() {
            if age_ns <= *w_ns && ask < mins[k] {
                mins[k] = ask;
            }
        }
        if age_ns <= cfg_ns && ask < cfg_min {
            cfg_min = ask;
        }
    }
    let passed = cfg.entry_stability_s == 0 || cfg_min >= side_ask_now - cfg.stability_eps;
    (mins[0], mins[1], mins[2], passed)
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
) -> (Vec<TradeRecord>, u32) {
    let close_ns = series.meta.close_ts_ns;
    let latency_ns = latency_ms as i64 * 1_000_000;
    let cooldown_ns = cfg.clip_cooldown_ms as i64 * 1_000_000;
    let max_clips = cfg.max_clips.max(1) as usize;
    let pair_completion = cfg.pair_completion_margin > 0.0;
    let mut trades: Vec<TradeRecord> = Vec::new();
    let mut maker_placed = 0u32;
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
                let fee2 = px2 * sh2 * cfg.taker_fee_bps / 10_000.0
                    + curve_fee(cfg.fee_curve_rate, px2, sh2);
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
                t1.fee_hold = false;
                t1.hold_alt_sell_pnl = None;
                t1.hold_alt_exit_fee = None;
                trades.push(TradeRecord {
                    sigma_bar_bps: d.sigma_bar_bps,
                    side_ask_at_decision: 0.0,
                    trail_min_ask_10s: None,
                    trail_min_ask_20s: None,
                    trail_min_ask_40s: None,
                    stable_entry: true,
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
                    fee_hold: false,
                    hold_alt_sell_pnl: None,
                    hold_alt_exit_fee: None,
                    stopped: false,
                    stop_hold_pnl: None,
                    maker_entry: false,
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
        // Shared entry-decision SSOT: pm_alpha::decide_entry owns every gate,
        // side-pick, and sizing line, so the backtest, the live shadow, and the
        // live agent make byte-identical decisions. belief_pass already bounded
        // ticks to [open, deadline]; the history-dependent pre-entry stability
        // gate stays here (it needs the trailing tick window, not just touch).
        let inputs = crate::decide::DecisionInputs {
            p_exo: d.p_up,
            dir_p_up: d.dir_p_up,
            dir_model_active: pass.dir_model_active,
            yes_ask: d.yes_ask,
            no_buy: d.no_buy,
            mid: d.mid,
            sigma_bar_bps: d.sigma_bar_bps,
            basis_mom_60s_bps: d.basis_mom_60s_bps,
        };
        let dcfg = crate::decide::DecideConfig::from_harness(cfg, edge_threshold);
        let (decision, delta) = crate::decide::decide_entry(
            &inputs,
            d.ts_ns,
            close_ns,
            &crate::decide::EntryState { armed, next_entry_ns },
            &dcfg,
        );
        match decision.action {
            crate::decide::EntryAction::Rearm => {
                if let Some(a) = delta.set_armed {
                    armed = a;
                }
                continue;
            }
            crate::decide::EntryAction::Skip => continue,
            crate::decide::EntryAction::Enter => {}
        }
        let side = decision.side;
        let notional = decision.target_notional;

        // Pre-entry stability: the trailing window of the entry side's ask
        // must hold at-or-above (current ask - eps); diagnostics are always
        // computed so the control run carries the offline proxy study.
        let side_ask_now = match side {
            Side::Yes => d.yes_ask,
            Side::No => d.no_buy,
        };
        let (trail_min_10, trail_min_20, trail_min_40, stable_entry) =
            trailing_stability(series, d.tick_idx, d.ts_ns, side, side_ask_now, cfg);
        if !stable_entry {
            continue;
        }
        let entry_cost = match side {
            Side::Yes => d.yes_ask,
            Side::No => d.no_buy,
        };

        // Maker entry study: rest a bid below the side's current ask
        // instead of taking it. Live after the entry latency; fills only if
        // the side ask later trades at-or-below the level (crossed-through,
        // same primitive as the passive-exit fill check) before the
        // stop_before_close deadline; zero fee, hold to resolution. One
        // resting order per market, filled or cancelled.
        if cfg.maker_entry_offset >= 0.0 {
            let level = entry_cost - cfg.maker_entry_offset;
            maker_placed += 1;
            if level > 0.0 && level < 1.0 {
                let live_ns = d.ts_ns + latency_ns;
                let deadline_ns = close_ns - cfg.stop_before_close_s as i64 * 1_000_000_000;
                let cross = series.ticks[d.tick_idx..]
                    .iter()
                    .filter(|t| t.ts_ns >= live_ns && t.ts_ns <= deadline_ns)
                    .find(|t| side_ask(t, side).is_some_and(|a| a <= level + 1e-6));
                if let Some(fill_tick) = cross {
                    let shares = notional / level;
                    let won = match side {
                        Side::Yes => series.resolved_yes,
                        Side::No => !series.resolved_yes,
                    };
                    let payout = if won { 1.0 } else { 0.0 };
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
                        avg_price: level,
                        shares,
                        fee: 0.0,
                        p_exo: d.p_up,
                        mid_at_decision: d.mid,
                        pnl: shares * (payout - level),
                        won,
                        exit_price: None,
                        mark_60s,
                        pnl_exit_mid_optimistic: None,
                        is_completion: false,
                        exit_filled_at_mid: None,
                        fee_hold: false,
                        hold_alt_sell_pnl: None,
                        hold_alt_exit_fee: None,
                        sigma_bar_bps: d.sigma_bar_bps,
                        maker_entry: true,
                    
                        side_ask_at_decision: 0.0,
                        trail_min_ask_10s: None,
                        trail_min_ask_20s: None,
                        trail_min_ask_40s: None,
                        stable_entry: true,
                        stopped: false,
                        stop_hold_pnl: None,
                    });
                }
            }
            break;
        }

        // Latency: fill against the book as it actually is at T + latency.
        let fill_at_ns = d.ts_ns + latency_ns;
        let Some(fill_rel) = series.ticks[d.tick_idx..]
            .iter()
            .position(|t| t.ts_ns >= fill_at_ns && t.ts_ns <= close_ns)
        else {
            continue;
        };
        let fill_idx = d.tick_idx + fill_rel;
        let fill_tick = &series.ticks[fill_idx];

        let entry_cap = (cfg.min_marginal_edge > 0.0).then(|| decision.marketable_limit_price);
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
        let entry_fee = avg_price * shares * cfg.taker_fee_bps / 10_000.0
            + curve_fee(cfg.fee_curve_rate, avg_price, shares);
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
        let mut fee_hold = false;
        let mut hold_alt_sell_pnl = None;
        let mut hold_alt_exit_fee = None;
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
                                exit_fee = px * sold * cfg.taker_fee_bps / 10_000.0
                                    + curve_fee(cfg.fee_curve_rate, px, sold);
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
                                exit_fee = px * sold * cfg.taker_fee_bps / 10_000.0
                                    + curve_fee(cfg.fee_curve_rate, px, sold);
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
                let leg_fee = px * sold * cfg.taker_fee_bps / 10_000.0
                    + curve_fee(cfg.fee_curve_rate, px, sold);
                let remainder = (shares - sold).max(0.0);
                let sell_now = if cfg.fee_aware_exit {
                    // Belief at the most recent decision at-or-before the
                    // exit tick (the entry decision is the floor).
                    let p_up_exit = pass
                        .decisions
                        .iter()
                        .rev()
                        .find(|dd| dd.ts_ns <= exit_tick.ts_ns)
                        .map_or(d.p_up, |dd| dd.p_up);
                    let p_side_exit = match side {
                        Side::Yes => p_up_exit,
                        Side::No => 1.0 - p_up_exit,
                    };
                    // The unsold remainder settles either way, so it cancels
                    // from both sides: compare net proceeds vs hold EV on
                    // the sellable portion (+ the variance premium).
                    sold * px - leg_fee
                        >= p_side_exit * sold + cfg.fee_exit_margin * sold
                } else {
                    true
                };
                if sell_now {
                    exit_fee = leg_fee;
                    proceeds_pnl =
                        Some(sold * (px - avg_price) + remainder * (payout - avg_price));
                    exit_price = Some(px);
                } else {
                    fee_hold = true;
                    hold_alt_exit_fee = Some(leg_fee);
                    hold_alt_sell_pnl = Some(
                        sold * (px - avg_price) + remainder * (payout - avg_price)
                            - entry_fee
                            - leg_fee,
                    );
                }
            }
        }
        // Post-entry selldown stop (hold mode only): trades whose side ask
        // later prints through entry lose money even held to expiry; sell
        // at the first such tick (taker, exit-leg fee), remainder settles.
        let mut stopped = false;
        let mut stop_hold_pnl = None;
        if cfg.selldown_stop_eps >= 0.0 && cfg.exit_after_s == 0 {
            let stop_level = avg_price - cfg.selldown_stop_eps;
            if let Some(stop_tick) = series.ticks[fill_idx + 1..]
                .iter()
                .take_while(|t| t.ts_ns <= close_ns)
                .find(|t| side_ask(t, side).is_some_and(|a| a <= stop_level + 1e-9))
                && let Some((px, sold)) = sell_fill(
                    stop_tick,
                    side,
                    shares,
                    cfg.depth_capture_frac,
                    cfg.skip_touch_level,
                )
            {
                exit_fee = px * sold * cfg.taker_fee_bps / 10_000.0
                    + curve_fee(cfg.fee_curve_rate, px, sold);
                let remainder = (shares - sold).max(0.0);
                proceeds_pnl =
                    Some(sold * (px - avg_price) + remainder * (payout - avg_price));
                exit_price = Some(px);
                stopped = true;
                stop_hold_pnl = Some(shares * (payout - avg_price) - entry_fee);
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
            sigma_bar_bps: d.sigma_bar_bps,
            side_ask_at_decision: side_ask_now,
            trail_min_ask_10s: Some(trail_min_10),
            trail_min_ask_20s: Some(trail_min_20),
            trail_min_ask_40s: Some(trail_min_40),
            stable_entry,
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
            fee_hold,
            hold_alt_sell_pnl,
            hold_alt_exit_fee,
            stopped,
            stop_hold_pnl,
            maker_entry: false,
        
                    });
        if pair_completion && !first_entry_done && !stopped {
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
                let tail_fee = tail_price * tail_shares * cfg.taker_fee_bps / 10_000.0
                    + curve_fee(cfg.fee_curve_rate, tail_price, tail_shares);
                let tail_won = match tail_side {
                    Side::Yes => series.resolved_yes,
                    Side::No => !series.resolved_yes,
                };
                let tail_payout = if tail_won { 1.0 } else { 0.0 };
                trades.push(TradeRecord {
                    sigma_bar_bps: d.sigma_bar_bps,
                    side_ask_at_decision: 0.0,
                    trail_min_ask_10s: None,
                    trail_min_ask_20s: None,
                    trail_min_ask_40s: None,
                    stable_entry: true,
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
                    fee_hold: false,
                    hold_alt_sell_pnl: None,
                    hold_alt_exit_fee: None,
                    stopped: false,
                    stop_hold_pnl: None,
                    maker_entry: false,
                });
            }
        }
    }
    (trades, maker_placed)
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

/// Side-oriented ask: the cost of buying one more share of `side` at the
/// touch (real NO ask when loaded, synthetic complement otherwise).
fn side_ask(tick: &BookTick, side: Side) -> Option<f64> {
    match side {
        Side::Yes => {
            (tick.yes_ask > 0.0 && tick.yes_ask < 1.0).then_some(tick.yes_ask as f64)
        }
        Side::No => tick.no_buy_price(),
    }
}

/// Side-oriented mid: the natural resting-ask level for a passive exit.
fn side_mid(tick: &BookTick, side: Side) -> Option<f64> {
    match side {
        Side::Yes => tick.mid(),
        Side::No if tick.has_real_no() => Some(((tick.no_bid + tick.no_ask) / 2.0) as f64),
        Side::No => tick.mid().map(|m| 1.0 - m),
    }
}

/// Side-oriented best ask: the buy touch a resting maker bid must see trade
/// at-or-below to be considered filled (crossed-through).
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
            let (trades, maker_placed) = if calm_blocked {
                (Vec::new(), 0)
            } else {
                execute(series, &pass, latency_ms, threshold, cfg)
            };
            outputs.push(MarketRunOutput {
                trades,
                samples: pass.samples.clone(),
                had_belief: pass.had_belief,
                train_samples,
                dir_samples,
                regime,
                min_pair_cost,
                real_no_coverage,
                maker_placed,
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
