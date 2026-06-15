//! Shared entry-decision SSOT for the exogenous-fade strategy.
//!
//! `decide_entry` owns every gate, side-pick, and sizing line that determines
//! WHETHER and HOW to enter a fade clip. This logic was previously duplicated
//! across `harness::replay::execute` (backtest), `pm_app::shadow::decide` (the
//! live mirror), and the offline sigma-floor / Saturday scoring filters. It
//! computes nothing about fills: the caller realizes the order (the sim
//! book-walk in the backtest, a marketable IOC live). Keeping the decision in
//! one pure function is what guarantees that the backtest and the live agent
//! make byte-identical decisions, proven by the `exo_fade_equivalence` test.
//!
//! The function is ported character-for-character from `execute()` L354-457 so
//! the backtest refactor changes nothing (provable by a before/after diff). The
//! only steps that exist solely in the live path today — the `min_entry_sigma_bps`
//! floor and `skip_saturday` — are folded in as config-gated steps that are
//! inert when the backtest passes `min_entry_sigma_bps = 0.0` and
//! `skip_saturday = false` (those gates are applied OFFLINE in the backtest).
//!
//! The pre-entry STABILITY gate stays in the caller: it needs the trailing tick
//! history (not just the touch), and for the frozen config it is inert
//! (`entry_stability_s = 0`). Running sizing before the caller's stability check
//! is output-neutral (the sized clip is discarded if stability fails), so the
//! emitted trade is unchanged.

use crate::harness::{EntryMode, HarnessConfig, Side};
use chrono::{Datelike, Weekday};

/// Per-tick decision inputs, built identically in the backtest (from a
/// precomputed `Decision`) and live (from the spot/perp tapes + the book touch).
#[derive(Debug, Clone, Copy)]
pub struct DecisionInputs {
    /// Exogenous belief P(up) (replay `d.p_up`).
    pub p_exo: f64,
    /// Continuation-model belief, present only in Aligned mode with a move in
    /// progress (replay `d.dir_p_up`).
    pub dir_p_up: Option<f64>,
    /// True when a dir model gates Aligned entries this run.
    pub dir_model_active: bool,
    pub yes_ask: f64,
    pub no_buy: f64,
    pub mid: f64,
    pub sigma_bar_bps: f64,
    pub basis_mom_60s_bps: f64,
}

/// Loop-carried state the caller owns and threads in (rearm + cooldown).
#[derive(Debug, Clone, Copy)]
pub struct EntryState {
    pub armed: bool,
    pub next_entry_ns: i64,
}

/// Mutations the caller applies AFTER an entry actually completes (keeps
/// `decide_entry` pure). `set_armed` on the Rearm path is applied immediately.
#[derive(Debug, Clone, Copy, Default)]
pub struct EntryStateDelta {
    pub set_armed: Option<bool>,
    pub set_next_entry_ns: Option<i64>,
    pub inc_clips: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAction {
    /// Enter `side` for `target_notional`.
    Enter,
    /// Stand down this tick (any gate failed).
    Skip,
    /// Disarmed-watch path: state may flip `armed`, never enters this tick.
    Rearm,
}

#[derive(Debug, Clone, Copy)]
pub struct EntryDecision {
    pub action: EntryAction,
    pub side: Side,
    pub target_notional: f64,
    /// The marketable limit the live order carries (= `p_side - min_marginal_edge`,
    /// i.e. replay's `entry_cap`). Only meaningful on `Enter`.
    pub marketable_limit_price: f64,
    pub hold_to_redemption: bool,
}

/// Consolidated decision config. `min_entry_sigma_bps` and `skip_saturday` live
/// here (NOT in `HarnessConfig`) because they were shadow-only; the backtest
/// constructs this via [`DecideConfig::from_harness`] which leaves them inert.
#[derive(Debug, Clone, Copy)]
pub struct DecideConfig {
    pub edge_threshold: f64,
    pub min_marginal_edge: f64,
    pub min_entry_sigma_bps: f64,
    pub max_entry_sigma_bps: f64,
    pub skip_saturday: bool,
    pub rearm_edge: f64,
    pub clip_cooldown_ms: u64,
    pub exit_after_s: u32,
    pub enter_within_close_s: u32,
    pub stop_before_close_s: u32,
    pub notional_usdc: f64,
    pub kelly_sizing: bool,
    pub vol_sizing_ref_bps: f64,
    pub vol_sizing_lo: f64,
    pub vol_sizing_hi: f64,
    pub basis_mom_agree: f64,
    pub basis_mom_disagree: f64,
    pub entry_mode: EntryMode,
    pub align_min_mid: f64,
}

impl DecideConfig {
    /// Backtest construction. The sigma floor and Saturday-skip are applied
    /// OFFLINE (post-filter on emitted trades), so they are inert here to keep
    /// `replay::execute` byte-identical. `edge_threshold` is the grid value the
    /// harness sweeps, passed separately (not `HarnessConfig::edge_threshold`).
    pub fn from_harness(cfg: &HarnessConfig, edge_threshold: f64) -> Self {
        Self {
            edge_threshold,
            min_marginal_edge: cfg.min_marginal_edge,
            min_entry_sigma_bps: 0.0,
            max_entry_sigma_bps: cfg.max_entry_sigma_bps,
            skip_saturday: false,
            rearm_edge: cfg.rearm_edge,
            clip_cooldown_ms: cfg.clip_cooldown_ms,
            exit_after_s: cfg.exit_after_s,
            enter_within_close_s: cfg.enter_within_close_s,
            stop_before_close_s: cfg.stop_before_close_s,
            notional_usdc: cfg.notional_usdc,
            kelly_sizing: cfg.kelly_sizing,
            vol_sizing_ref_bps: cfg.vol_sizing_ref_bps,
            vol_sizing_lo: cfg.vol_sizing_lo,
            vol_sizing_hi: cfg.vol_sizing_hi,
            basis_mom_agree: cfg.basis_mom_agree,
            basis_mom_disagree: cfg.basis_mom_disagree,
            entry_mode: cfg.entry_mode,
            align_min_mid: cfg.align_min_mid,
        }
    }
}

fn skip(side: Side, cfg: &DecideConfig) -> (EntryDecision, EntryStateDelta) {
    (
        EntryDecision {
            action: EntryAction::Skip,
            side,
            target_notional: 0.0,
            marketable_limit_price: 0.0,
            hold_to_redemption: cfg.exit_after_s == 0,
        },
        EntryStateDelta::default(),
    )
}

/// The single owner of the fade entry decision. Pure: identical inputs ->
/// identical `(EntryDecision, EntryStateDelta)`. The caller applies the delta
/// (after a completed entry) and runs the history-dependent stability gate.
pub fn decide_entry(
    inp: &DecisionInputs,
    now_ns: i64,
    close_ns: i64,
    state: &EntryState,
    cfg: &DecideConfig,
) -> (EntryDecision, EntryStateDelta) {
    // Entry window outer bound (replay L354-358).
    if cfg.enter_within_close_s > 0
        && now_ns < close_ns - cfg.enter_within_close_s as i64 * 1_000_000_000
    {
        return skip(Side::Yes, cfg);
    }
    // Inner deadline: in the backtest belief_pass never emits Decisions past
    // this, so this is redundant-but-safe there; live it is load-bearing.
    if cfg.stop_before_close_s > 0
        && now_ns >= close_ns - cfg.stop_before_close_s as i64 * 1_000_000_000
    {
        return skip(Side::Yes, cfg);
    }
    // Regime / vol ceiling (replay L361-363).
    if cfg.max_entry_sigma_bps > 0.0 && inp.sigma_bar_bps > cfg.max_entry_sigma_bps {
        return skip(Side::Yes, cfg);
    }
    // Belief selection (replay L366-370).
    let p_up = match (cfg.entry_mode, inp.dir_p_up) {
        (EntryMode::Aligned, Some(p)) => p,
        (EntryMode::Aligned, None) if inp.dir_model_active => return skip(Side::Yes, cfg),
        _ => inp.p_exo,
    };
    // Edge per side (replay L372-373).
    let edge_yes = p_up - inp.yes_ask;
    let edge_no = (1.0 - p_up) - inp.no_buy;
    // Rearm gate (replay L376-381): disarmed-watch, never enters this tick.
    if cfg.rearm_edge > 0.0 && !state.armed {
        let mut delta = EntryStateDelta::default();
        if edge_yes < cfg.rearm_edge && edge_no < cfg.rearm_edge {
            delta.set_armed = Some(true);
        }
        return (
            EntryDecision {
                action: EntryAction::Rearm,
                side: Side::Yes,
                target_notional: 0.0,
                marketable_limit_price: 0.0,
                hold_to_redemption: cfg.exit_after_s == 0,
            },
            delta,
        );
    }
    // Cooldown (replay L382-384).
    if now_ns < state.next_entry_ns {
        return skip(Side::Yes, cfg);
    }
    // Side + edge pick (replay L385-389): tie to Yes/Up.
    let (side, edge) = if edge_yes >= edge_no {
        (Side::Yes, edge_yes)
    } else {
        (Side::No, edge_no)
    };
    // Edge threshold (replay L390-392) — the core gate.
    if edge < cfg.edge_threshold {
        return skip(side, cfg);
    }
    // Sigma floor (shadow/live only; inert in the backtest where it is 0.0).
    if inp.sigma_bar_bps < cfg.min_entry_sigma_bps {
        return skip(side, cfg);
    }
    // Saturday-skip (shadow/live only; inert in the backtest where it is false).
    if cfg.skip_saturday
        && chrono::DateTime::from_timestamp_nanos(now_ns).weekday() == Weekday::Sat
    {
        return skip(side, cfg);
    }
    // Aligned min-mid gate (replay L393-403).
    if cfg.entry_mode == EntryMode::Aligned {
        let side_mid = match side {
            Side::Yes => inp.mid,
            Side::No => 1.0 - inp.mid,
        };
        if side_mid < cfg.align_min_mid {
            return skip(side, cfg);
        }
    }
    // Pre-entry stability gate (replay L405-416) is applied by the CALLER (it
    // needs the trailing tick history). Running sizing first is output-neutral.

    // Sizing (replay L418-457).
    let entry_cost = match side {
        Side::Yes => inp.yes_ask,
        Side::No => inp.no_buy,
    };
    let p_side = match side {
        Side::Yes => p_up,
        Side::No => 1.0 - p_up,
    };
    let mut notional = if cfg.kelly_sizing {
        let p_eff = entry_cost + 0.5 * (p_side - entry_cost);
        let kelly = ((p_eff - entry_cost) / (1.0 - entry_cost).max(1e-6)).max(0.0);
        let edge_factor = (kelly / 0.16).clamp(0.0, 1.0);
        let var_factor =
            (entry_cost / (p_eff * (1.0 - p_eff)).sqrt().max(1e-6)).clamp(0.0, 1.0);
        (cfg.notional_usdc * edge_factor * var_factor).max(0.0)
    } else if cfg.vol_sizing_ref_bps > 0.0 {
        let mult = (inp.sigma_bar_bps / cfg.vol_sizing_ref_bps)
            .clamp(cfg.vol_sizing_lo, cfg.vol_sizing_hi);
        cfg.notional_usdc * mult
    } else {
        cfg.notional_usdc
    };
    if cfg.basis_mom_agree != 1.0 || cfg.basis_mom_disagree != 1.0 {
        let signed = match side {
            Side::Yes => inp.basis_mom_60s_bps,
            Side::No => -inp.basis_mom_60s_bps,
        };
        notional *= if signed > 0.0 {
            cfg.basis_mom_agree
        } else {
            cfg.basis_mom_disagree
        };
    }
    if notional < 1.0 {
        return skip(side, cfg);
    }

    let delta = EntryStateDelta {
        set_armed: if cfg.rearm_edge > 0.0 { Some(false) } else { None },
        set_next_entry_ns: Some(now_ns + cfg.clip_cooldown_ms as i64 * 1_000_000),
        inc_clips: true,
    };
    (
        EntryDecision {
            action: EntryAction::Enter,
            side,
            target_notional: notional,
            marketable_limit_price: p_side - cfg.min_marginal_edge,
            hold_to_redemption: cfg.exit_after_s == 0,
        },
        delta,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    // The frozen live/shadow config: hold, edge 0.12, sigma floor 3.0, skip-Sat.
    fn live_cfg() -> DecideConfig {
        DecideConfig {
            edge_threshold: 0.12,
            min_marginal_edge: 0.04,
            min_entry_sigma_bps: 3.0,
            max_entry_sigma_bps: 0.0,
            skip_saturday: true,
            rearm_edge: 0.08,
            clip_cooldown_ms: 5000,
            exit_after_s: 0,
            enter_within_close_s: 0,
            stop_before_close_s: 90,
            notional_usdc: 50.0,
            kelly_sizing: false,
            vol_sizing_ref_bps: 0.0,
            vol_sizing_lo: 0.5,
            vol_sizing_hi: 2.0,
            basis_mom_agree: 1.0,
            basis_mom_disagree: 1.0,
            entry_mode: EntryMode::Fade,
            align_min_mid: 0.55,
        }
    }

    // Up-belief inputs: edge_yes = 0.90 - 0.72 = 0.18 (> 0.12), sigma 10 (> floor).
    fn up_inputs() -> DecisionInputs {
        DecisionInputs {
            p_exo: 0.90,
            dir_p_up: None,
            dir_model_active: false,
            yes_ask: 0.72,
            no_buy: 0.30,
            mid: 0.71,
            sigma_bar_bps: 10.0,
            basis_mom_60s_bps: 0.0,
        }
    }

    fn ts(y: i32, mo: u32, d: u32, h: u32) -> i64 {
        Utc.with_ymd_and_hms(y, mo, d, h, 0, 0)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap()
    }

    fn fresh_state() -> EntryState {
        EntryState { armed: true, next_entry_ns: i64::MIN }
    }

    // 2026-06-12 is a Friday, 2026-06-13 a Saturday. close 5 min ahead.
    const FRI: fn() -> i64 = || ts(2026, 6, 12, 12);
    const SAT: fn() -> i64 = || ts(2026, 6, 13, 12);
    fn close_of(now: i64) -> i64 {
        now + 300 * 1_000_000_000
    }

    #[test]
    fn enters_on_edge_with_correct_side_and_marketable_limit() {
        let now = FRI();
        let (dec, delta) =
            decide_entry(&up_inputs(), now, close_of(now), &fresh_state(), &live_cfg());
        assert_eq!(dec.action, EntryAction::Enter);
        assert_eq!(dec.side, Side::Yes);
        assert_eq!(dec.target_notional, 50.0);
        // marketable limit = p_side(0.90) - min_marginal_edge(0.04).
        assert!((dec.marketable_limit_price - 0.86).abs() < 1e-9);
        assert!(dec.hold_to_redemption);
        assert_eq!(delta.set_next_entry_ns, Some(now + 5000 * 1_000_000));
        assert!(delta.inc_clips);
        assert_eq!(delta.set_armed, Some(false));
    }

    #[test]
    fn skips_when_edge_below_threshold() {
        let mut inp = up_inputs();
        inp.yes_ask = 0.80; // edge_yes = 0.10 < 0.12
        let now = FRI();
        let (dec, _) = decide_entry(&inp, now, close_of(now), &fresh_state(), &live_cfg());
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_below_sigma_floor() {
        let mut inp = up_inputs();
        inp.sigma_bar_bps = 2.0; // < floor 3.0
        let now = FRI();
        let (dec, _) = decide_entry(&inp, now, close_of(now), &fresh_state(), &live_cfg());
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_saturday_but_trades_friday() {
        let cfg = live_cfg();
        let fri = FRI();
        let sat = SAT();
        assert_eq!(
            decide_entry(&up_inputs(), fri, close_of(fri), &fresh_state(), &cfg)
                .0
                .action,
            EntryAction::Enter
        );
        assert_eq!(
            decide_entry(&up_inputs(), sat, close_of(sat), &fresh_state(), &cfg)
                .0
                .action,
            EntryAction::Skip
        );
        // With skip_saturday off (backtest path), Saturday trades.
        let mut bt = cfg;
        bt.skip_saturday = false;
        assert_eq!(
            decide_entry(&up_inputs(), sat, close_of(sat), &fresh_state(), &bt)
                .0
                .action,
            EntryAction::Enter
        );
    }

    #[test]
    fn rearm_arms_when_edges_collapse_and_never_enters() {
        let cfg = live_cfg();
        let now = FRI();
        let disarmed = EntryState { armed: false, next_entry_ns: i64::MIN };
        // Edges still wide -> stay disarmed (set_armed None), action Rearm.
        let (dec, delta) = decide_entry(&up_inputs(), now, close_of(now), &disarmed, &cfg);
        assert_eq!(dec.action, EntryAction::Rearm);
        assert_eq!(delta.set_armed, None);
        // Edges collapsed below rearm_edge -> re-arm (set_armed Some(true)), still Rearm.
        let mut tight = up_inputs();
        tight.yes_ask = 0.88; // edge_yes = 0.02 < 0.08; edge_no negative
        let (dec2, delta2) = decide_entry(&tight, now, close_of(now), &disarmed, &cfg);
        assert_eq!(dec2.action, EntryAction::Rearm);
        assert_eq!(delta2.set_armed, Some(true));
    }

    #[test]
    fn skips_during_cooldown() {
        let now = FRI();
        let st = EntryState { armed: true, next_entry_ns: now + 1 };
        let (dec, _) = decide_entry(&up_inputs(), now, close_of(now), &st, &live_cfg());
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_past_inner_deadline() {
        let now = FRI();
        // close only 30s ahead -> inside the 90s stop_before_close deadline.
        let close = now + 30 * 1_000_000_000;
        let (dec, _) = decide_entry(&up_inputs(), now, close, &fresh_state(), &live_cfg());
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn side_tie_breaks_to_yes() {
        // edge_yes == edge_no exactly -> Yes.
        let inp = DecisionInputs {
            p_exo: 0.50,
            dir_p_up: None,
            dir_model_active: false,
            yes_ask: 0.30, // edge_yes = 0.20
            no_buy: 0.30,  // edge_no = 0.20
            mid: 0.50,
            sigma_bar_bps: 10.0,
            basis_mom_60s_bps: 0.0,
        };
        let now = FRI();
        let (dec, _) = decide_entry(&inp, now, close_of(now), &fresh_state(), &live_cfg());
        assert_eq!(dec.action, EntryAction::Enter);
        assert_eq!(dec.side, Side::Yes);
    }

    #[test]
    fn skips_when_notional_below_one() {
        let mut cfg = live_cfg();
        cfg.notional_usdc = 0.5; // < 1.0 floor
        let now = FRI();
        let (dec, _) = decide_entry(&up_inputs(), now, close_of(now), &fresh_state(), &cfg);
        assert_eq!(dec.action, EntryAction::Skip);
    }
}
