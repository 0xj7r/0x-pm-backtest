//! Shared entry-decision SSOT for the exogenous-fade strategy.
//!
//! `decide_entry` owns every gate, side-pick, and sizing line that determines
//! WHETHER and HOW to enter a fade clip. This logic was previously duplicated
//! across `harness::replay::execute` (backtest), `pm_app::shadow::decide` (the
//! live mirror), and the offline sigma-floor / Saturday scoring filters. It
//! computes nothing about fills: the caller realizes the order (the sim
//! book-walk in the backtest, a marketable IOC live). Keeping the decision in
//! one pure function is what guarantees that the backtest and the live agent
//! make byte-identical decisions, proven by the `decide_construction_parity`
//! test (`crates/pm-alpha/src/decide_construction_parity.rs`), which compares
//! the two paths' `DecisionInputs` construction value-for-value.
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
use crate::regime::Regime;
use chrono::{Datelike, Weekday};

/// Frozen fade `DecideConfig` validated on backtest + the `shadow-final` live twin.
///
/// hold@0.12, sigma floor 3.0, skip-Saturday, rearm 0.08, 90s pre-close stop,
/// hold-to-redemption (`exit_after_s = 0`). `notional_usdc` is a runtime concern
/// (shadow telemetry uses 50; live clips use env-configured sizing).
pub fn frozen_fade_decide_config(notional_usdc: f64) -> DecideConfig {
    DecideConfig {
        edge_threshold: 0.12,
        min_marginal_edge: 0.04,
        min_entry_sigma_bps: 3.0,
        max_entry_sigma_bps: 0.0,
        skip_saturday: true,
        rearm_edge: 0.08,
        clip_cooldown_ms: 5_000,
        exit_after_s: 0,
        enter_within_close_s: 0,
        stop_before_close_s: 90,
        notional_usdc,
        kelly_sizing: false,
        min_p_side: 0.0,
        max_p_side: 1.0,
        min_entry_ask: 0.0,
        max_entry_ask: 1.0,
        min_secs_from_open: 0,
        min_belief_dwell_s: 0.0,
        vol_sizing_ref_bps: 0.0,
        vol_sizing_lo: 0.5,
        vol_sizing_hi: 2.0,
        basis_mom_agree: 1.0,
        basis_mom_disagree: 1.0,
        entry_mode: EntryMode::Fade,
        align_min_mid: 0.55,
        skip_calm: false,
        only_calm: false,
        skip_expanded_mixed: false,
        skip_expanded_high_flip: false,
        skip_open_fav_gap: false,
        open_fav_p_min: 0.90,
        open_fav_ask_max: 0.60,
        open_fav_secs: 5,
        pause_after_consec_losses: 0,
        max_rearm_entry_ask: 0.0,
        skip_spot_misalign_s: 0,
        skip_spot_against_all: false,
    }
}

/// Cross-market session state for loss-streak gates (backtest serial replay;
/// live/shadow should mirror the same updates after each resolution).
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionGateState {
    pub consec_losses: u32,
}

impl SessionGateState {
    pub fn observe_trade(&mut self, won: bool) {
        if won {
            self.consec_losses = 0;
        } else {
            self.consec_losses = self.consec_losses.saturating_add(1);
        }
    }
}

/// Advance session state after a market's trades resolve (chronological clips).
pub fn session_observe_trades(session: &mut SessionGateState, trades: &[crate::harness::TradeRecord]) {
    let mut ordered: Vec<_> = trades.iter().filter(|t| !t.is_completion && !t.is_hedge && !t.is_cut).collect();
    ordered.sort_by_key(|t| t.decision_ts_ns);
    for t in ordered {
        session.observe_trade(t.won);
    }
}

/// True when any gate needs chronological market replay (not parallel-safe).
pub fn session_gates_active(cfg: &DecideConfig) -> bool {
    cfg.pause_after_consec_losses > 0
}

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
    /// Exogenous regime at this decision instant (None when spot is too thin).
    pub regime_at_decision: Option<Regime>,
    /// Zero-based clip index within the market (0 = first entry).
    pub clip_index: u32,
    pub spot_ret_10s_bps: Option<f64>,
    pub spot_ret_30s_bps: Option<f64>,
    pub spot_ret_60s_bps: Option<f64>,
    pub spot_ret_120s_bps: Option<f64>,
    pub spot_ret_300s_bps: Option<f64>,
    pub spot_ret_600s_bps: Option<f64>,
    pub spot_ret_900s_bps: Option<f64>,
    /// Seconds the belief has held its current side: time since sign(p - 0.5)
    /// last flipped, or since the first belief of the window. None when the
    /// caller cannot track it (gates treating None as permissive).
    pub belief_dwell_s: Option<f64>,
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
    /// Skip when chosen-side belief is below this (0 = off).
    pub min_p_side: f64,
    /// Skip when chosen-side belief exceeds this (1.0 = off).
    pub max_p_side: f64,
    /// Skip when entry ask is below this (0 = off).
    pub min_entry_ask: f64,
    /// No entries until this many seconds after market open (0 = off).
    pub min_secs_from_open: u32,
    /// Decision-quality gate: skip entries whose belief has held its side for
    /// fewer than this many seconds (0 = off; None dwell = permissive).
    /// Targets the first-seconds instability bucket (docs/decision-stability).
    pub min_belief_dwell_s: f64,
    /// Skip when entry ask exceeds this (1.0 = off).
    pub max_entry_ask: f64,
    pub vol_sizing_ref_bps: f64,
    pub vol_sizing_lo: f64,
    pub vol_sizing_hi: f64,
    pub basis_mom_agree: f64,
    pub basis_mom_disagree: f64,
    pub entry_mode: EntryMode,
    pub align_min_mid: f64,
    /// Skip when decision-time regime is `calm_low_vol`.
    pub skip_calm: bool,
    /// Take entries only in `calm_low_vol` (mutually exclusive with skip_calm in spirit).
    pub only_calm: bool,
    /// Skip when decision-time regime is `expanded_mixed`.
    pub skip_expanded_mixed: bool,
    /// Skip when decision-time regime is `expanded_high_flip`.
    pub skip_expanded_high_flip: bool,
    /// Skip favourites where model >> book (see open_fav_*). `open_fav_secs`
    /// scopes from market open; use 300 for the full btc-5m window (prod_gap_full).
    pub skip_open_fav_gap: bool,
    pub open_fav_p_min: f64,
    pub open_fav_ask_max: f64,
    pub open_fav_secs: u32,
    /// Pause entries when `consec_losses` >= this (0 = off).
    pub pause_after_consec_losses: u32,
    /// On rearm clips (clip_index > 0), skip when entry ask exceeds this (0 = off).
    pub max_rearm_entry_ask: f64,
    /// Skip when spot return over this lookback (seconds) disagrees with side (0 = off).
    pub skip_spot_misalign_s: u32,
    /// Skip when 60/300/600/900s spot all disagree with entry side.
    pub skip_spot_against_all: bool,
}

impl DecideConfig {
    /// Canonical config fingerprint: a compact JSON object with keys in
    /// struct declaration order. The string IS the fingerprint (no hashing);
    /// two streams with byte-equal canon strings run the same decide config.
    /// Written by hand (not serde) so the field order and number formatting
    /// stay frozen even if serialization defaults change.
    pub fn canon(&self) -> String {
        let entry_mode = match self.entry_mode {
            EntryMode::Fade => "fade",
            EntryMode::Aligned => "aligned",
        };
        format!(
            concat!(
                "{{",
                "\"edge_threshold\":{},",
                "\"min_marginal_edge\":{},",
                "\"min_entry_sigma_bps\":{},",
                "\"max_entry_sigma_bps\":{},",
                "\"skip_saturday\":{},",
                "\"rearm_edge\":{},",
                "\"clip_cooldown_ms\":{},",
                "\"exit_after_s\":{},",
                "\"enter_within_close_s\":{},",
                "\"stop_before_close_s\":{},",
                "\"notional_usdc\":{},",
                "\"kelly_sizing\":{},",
                "\"min_p_side\":{},",
                "\"max_p_side\":{},",
                "\"min_entry_ask\":{},",
                "\"min_secs_from_open\":{},",
                "\"min_belief_dwell_s\":{},",
                "\"max_entry_ask\":{},",
                "\"vol_sizing_ref_bps\":{},",
                "\"vol_sizing_lo\":{},",
                "\"vol_sizing_hi\":{},",
                "\"basis_mom_agree\":{},",
                "\"basis_mom_disagree\":{},",
                "\"entry_mode\":\"{}\",",
                "\"align_min_mid\":{},",
                "\"skip_calm\":{},",
                "\"only_calm\":{},",
                "\"skip_expanded_mixed\":{},",
                "\"skip_expanded_high_flip\":{},",
                "\"skip_open_fav_gap\":{},",
                "\"open_fav_p_min\":{},",
                "\"open_fav_ask_max\":{},",
                "\"open_fav_secs\":{},",
                "\"pause_after_consec_losses\":{},",
                "\"max_rearm_entry_ask\":{},",
                "\"skip_spot_misalign_s\":{},",
                "\"skip_spot_against_all\":{}",
                "}}",
            ),
            self.edge_threshold,
            self.min_marginal_edge,
            self.min_entry_sigma_bps,
            self.max_entry_sigma_bps,
            self.skip_saturday,
            self.rearm_edge,
            self.clip_cooldown_ms,
            self.exit_after_s,
            self.enter_within_close_s,
            self.stop_before_close_s,
            self.notional_usdc,
            self.kelly_sizing,
            self.min_p_side,
            self.max_p_side,
            self.min_entry_ask,
            self.min_secs_from_open,
            self.min_belief_dwell_s,
            self.max_entry_ask,
            self.vol_sizing_ref_bps,
            self.vol_sizing_lo,
            self.vol_sizing_hi,
            self.basis_mom_agree,
            self.basis_mom_disagree,
            entry_mode,
            self.align_min_mid,
            self.skip_calm,
            self.only_calm,
            self.skip_expanded_mixed,
            self.skip_expanded_high_flip,
            self.skip_open_fav_gap,
            self.open_fav_p_min,
            self.open_fav_ask_max,
            self.open_fav_secs,
            self.pause_after_consec_losses,
            self.max_rearm_entry_ask,
            self.skip_spot_misalign_s,
            self.skip_spot_against_all,
        )
    }

    /// Backtest construction. `edge_threshold` is the grid value the harness
    /// sweeps, passed separately (not `HarnessConfig::edge_threshold`).
    pub fn from_harness(cfg: &HarnessConfig, edge_threshold: f64) -> Self {
        Self {
            edge_threshold,
            min_marginal_edge: cfg.min_marginal_edge,
            min_entry_sigma_bps: cfg.min_entry_sigma_bps,
            max_entry_sigma_bps: cfg.max_entry_sigma_bps,
            skip_saturday: cfg.skip_saturday,
            rearm_edge: cfg.rearm_edge,
            clip_cooldown_ms: cfg.clip_cooldown_ms,
            exit_after_s: cfg.exit_after_s,
            enter_within_close_s: cfg.enter_within_close_s,
            stop_before_close_s: cfg.stop_before_close_s,
            notional_usdc: cfg.notional_usdc,
            kelly_sizing: cfg.kelly_sizing,
            min_p_side: cfg.min_p_side,
            max_p_side: cfg.max_p_side,
            min_entry_ask: cfg.min_entry_ask,
            max_entry_ask: cfg.max_entry_ask,
            min_secs_from_open: cfg.min_secs_from_open,
            min_belief_dwell_s: cfg.min_belief_dwell_s,
            vol_sizing_ref_bps: cfg.vol_sizing_ref_bps,
            vol_sizing_lo: cfg.vol_sizing_lo,
            vol_sizing_hi: cfg.vol_sizing_hi,
            basis_mom_agree: cfg.basis_mom_agree,
            basis_mom_disagree: cfg.basis_mom_disagree,
            entry_mode: cfg.entry_mode,
            align_min_mid: cfg.align_min_mid,
            skip_calm: cfg.skip_calm,
            only_calm: cfg.only_calm,
            skip_expanded_mixed: cfg.skip_expanded_mixed,
            skip_expanded_high_flip: cfg.skip_expanded_high_flip,
            skip_open_fav_gap: cfg.skip_open_fav_gap,
            open_fav_p_min: cfg.open_fav_p_min,
            open_fav_ask_max: cfg.open_fav_ask_max,
            open_fav_secs: cfg.open_fav_secs,
            pause_after_consec_losses: cfg.pause_after_consec_losses,
            max_rearm_entry_ask: cfg.max_rearm_entry_ask,
            skip_spot_misalign_s: cfg.skip_spot_misalign_s,
            skip_spot_against_all: cfg.skip_spot_against_all,
        }
    }
}

fn spot_ret_for_lookback(inp: &DecisionInputs, lookback_s: u32) -> Option<f64> {
    match lookback_s {
        10 => inp.spot_ret_10s_bps,
        30 => inp.spot_ret_30s_bps,
        60 => inp.spot_ret_60s_bps,
        120 => inp.spot_ret_120s_bps,
        300 => inp.spot_ret_300s_bps,
        600 => inp.spot_ret_600s_bps,
        900 => inp.spot_ret_900s_bps,
        _ => None,
    }
}

fn spot_agrees_with_side(side: Side, ret_bps: f64) -> bool {
    match side {
        Side::Yes => ret_bps > 0.0,
        Side::No => ret_bps < 0.0,
    }
}

fn spot_momentum_gates_pass(inp: &DecisionInputs, side: Side, cfg: &DecideConfig) -> bool {
    if cfg.skip_spot_misalign_s > 0 {
        if let Some(ret) = spot_ret_for_lookback(inp, cfg.skip_spot_misalign_s)
            && !spot_agrees_with_side(side, ret)
        {
            return false;
        }
    }
    if cfg.skip_spot_against_all {
        let horizons = [60_u32, 300, 600, 900];
        let mut saw = false;
        for h in horizons {
            let Some(ret) = spot_ret_for_lookback(inp, h) else {
                return true;
            };
            saw = true;
            if spot_agrees_with_side(side, ret) {
                return true;
            }
        }
        if saw {
            return false;
        }
    }
    true
}

/// Regime stand-down gates (per decision instant, rolling 30m spot path).
/// Evaluated inside `whipsaw_gates_pass`, AFTER the rearm branch, so a gated
/// regime blocks entries but never blocks re-arming (parity with the original
/// skip_expanded_high_flip placement).
fn regime_gates_pass(inp: &DecisionInputs, cfg: &DecideConfig) -> bool {
    let Some(regime) = inp.regime_at_decision else {
        // Skip-gates are permissive when the window is unclassifiable;
        // only_calm is strict (parity with the removed market-level calm
        // gate, which required Some(CalmLowVol)).
        return !cfg.only_calm;
    };
    if cfg.skip_calm && regime == Regime::CalmLowVol {
        return false;
    }
    if cfg.only_calm && regime != Regime::CalmLowVol {
        return false;
    }
    if cfg.skip_expanded_mixed && regime == Regime::ExpandedMixed {
        return false;
    }
    if cfg.skip_expanded_high_flip && regime == Regime::ExpandedHighFlip {
        return false;
    }
    true
}

fn whipsaw_gates_pass(
    inp: &DecisionInputs,
    now_ns: i64,
    open_ns: i64,
    side: Side,
    p_up: f64,
    entry_cost: f64,
    session: Option<&SessionGateState>,
    cfg: &DecideConfig,
) -> bool {
    if !regime_gates_pass(inp, cfg) {
        return false;
    }
    // Decision-quality: a belief that just flipped sides is the unstable
    // first-seconds signature (cross-process side agreement 43-58% there).
    // None = caller cannot track dwell = permissive (harness pre-dwell runs).
    if cfg.min_belief_dwell_s > 0.0
        && let Some(dwell) = inp.belief_dwell_s
        && dwell < cfg.min_belief_dwell_s
    {
        return false;
    }
    if let Some(s) = session
        && cfg.pause_after_consec_losses > 0
        && s.consec_losses >= cfg.pause_after_consec_losses
    {
        return false;
    }
    let p_side = match side {
        Side::Yes => p_up,
        Side::No => 1.0 - p_up,
    };
    if cfg.skip_open_fav_gap && cfg.open_fav_secs > 0 {
        let elapsed_s = (now_ns.saturating_sub(open_ns)) / 1_000_000_000;
        if elapsed_s <= cfg.open_fav_secs as i64
            && p_side > cfg.open_fav_p_min
            && entry_cost < cfg.open_fav_ask_max
        {
            return false;
        }
    }
    if cfg.max_rearm_entry_ask > 0.0
        && inp.clip_index > 0
        && entry_cost > cfg.max_rearm_entry_ask
    {
        return false;
    }
    spot_momentum_gates_pass(inp, side, cfg)
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
    open_ns: i64,
    close_ns: i64,
    state: &EntryState,
    session: Option<&SessionGateState>,
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
    // Open delay: stand down until the book has had time to reprice.
    if cfg.min_secs_from_open > 0 {
        let elapsed_s = (now_ns.saturating_sub(open_ns)) / 1_000_000_000;
        if elapsed_s < cfg.min_secs_from_open as i64 {
            return skip(Side::Yes, cfg);
        }
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
    // Thesis entry gates: belief floor and ask band on the chosen side.
    let entry_cost = match side {
        Side::Yes => inp.yes_ask,
        Side::No => inp.no_buy,
    };
    let p_side = match side {
        Side::Yes => p_up,
        Side::No => 1.0 - p_up,
    };
    if cfg.min_p_side > 0.0 && p_side < cfg.min_p_side {
        return skip(side, cfg);
    }
    if cfg.max_p_side < 1.0 && p_side > cfg.max_p_side {
        return skip(side, cfg);
    }
    if cfg.min_entry_ask > 0.0 && entry_cost < cfg.min_entry_ask {
        return skip(side, cfg);
    }
    if cfg.max_entry_ask < 1.0 && entry_cost > cfg.max_entry_ask {
        return skip(side, cfg);
    }
    if !whipsaw_gates_pass(inp, now_ns, open_ns, side, p_up, entry_cost, session, cfg) {
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

    fn live_cfg() -> DecideConfig {
        frozen_fade_decide_config(50.0)
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
            regime_at_decision: None,
            clip_index: 0,
            spot_ret_10s_bps: None,
            spot_ret_30s_bps: None,
            spot_ret_60s_bps: None,
            spot_ret_120s_bps: None,
            spot_ret_300s_bps: None,
            spot_ret_600s_bps: None,
            spot_ret_900s_bps: None,
            belief_dwell_s: None,
        }
    }

    fn session_none() -> Option<SessionGateState> {
        None
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

    fn open_of(now: i64) -> i64 {
        now - 60 * 1_000_000_000
    }

    #[test]
    fn enters_on_edge_with_correct_side_and_marketable_limit() {
        let now = FRI();
        let (dec, delta) =
            decide_entry(
                &up_inputs(),
                now,
                open_of(now),
                close_of(now),
                &fresh_state(),
                session_none().as_ref(),
                &live_cfg(),
            );
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
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &live_cfg(),
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_below_sigma_floor() {
        let mut inp = up_inputs();
        inp.sigma_bar_bps = 2.0; // < floor 3.0
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &live_cfg(),
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_saturday_but_trades_friday() {
        let cfg = live_cfg();
        let fri = FRI();
        let sat = SAT();
        assert_eq!(
            decide_entry(&up_inputs(), fri, open_of(fri), close_of(fri), &fresh_state(), session_none().as_ref(), &cfg)
                .0
                .action,
            EntryAction::Enter
        );
        assert_eq!(
            decide_entry(&up_inputs(), sat, open_of(sat), close_of(sat), &fresh_state(), session_none().as_ref(), &cfg)
                .0
                .action,
            EntryAction::Skip
        );
        // With skip_saturday off (backtest path), Saturday trades.
        let mut bt = cfg;
        bt.skip_saturday = false;
        assert_eq!(
            decide_entry(&up_inputs(), sat, open_of(sat), close_of(sat), &fresh_state(), session_none().as_ref(), &bt)
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
        let (dec, delta) = decide_entry(&up_inputs(), now, open_of(now), close_of(now), &disarmed, session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Rearm);
        assert_eq!(delta.set_armed, None);
        // Edges collapsed below rearm_edge -> re-arm (set_armed Some(true)), still Rearm.
        let mut tight = up_inputs();
        tight.yes_ask = 0.88; // edge_yes = 0.02 < 0.08; edge_no negative
        let (dec2, delta2) = decide_entry(&tight, now, open_of(now), close_of(now), &disarmed, session_none().as_ref(), &cfg);
        assert_eq!(dec2.action, EntryAction::Rearm);
        assert_eq!(delta2.set_armed, Some(true));
    }

    #[test]
    fn skips_during_cooldown() {
        let now = FRI();
        let st = EntryState { armed: true, next_entry_ns: now + 1 };
        let (dec, _) = decide_entry(&up_inputs(), now, open_of(now), close_of(now), &st, session_none().as_ref(), &live_cfg());
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_past_inner_deadline() {
        let now = FRI();
        // close only 30s ahead -> inside the 90s stop_before_close deadline.
        let close = now + 30 * 1_000_000_000;
        let (dec, _) = decide_entry(&up_inputs(), now, open_of(now), close, &fresh_state(), session_none().as_ref(), &live_cfg());
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
            regime_at_decision: None,
            clip_index: 0,
            spot_ret_10s_bps: None,
            spot_ret_30s_bps: None,
            spot_ret_60s_bps: None,
            spot_ret_120s_bps: None,
            spot_ret_300s_bps: None,
            spot_ret_600s_bps: None,
            spot_ret_900s_bps: None,
            belief_dwell_s: None,
        };
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &live_cfg(),
        );
        assert_eq!(dec.action, EntryAction::Enter);
        assert_eq!(dec.side, Side::Yes);
    }

    #[test]
    fn skips_when_notional_below_one() {
        let mut cfg = live_cfg();
        cfg.notional_usdc = 0.5; // < 1.0 floor
        let now = FRI();
        let (dec, _) = decide_entry(&up_inputs(), now, open_of(now), close_of(now), &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Skip);
    }

    fn thesis_a_cfg() -> DecideConfig {
        let mut cfg = live_cfg();
        cfg.edge_threshold = 0.12;
        cfg.min_p_side = 0.25;
        cfg.min_entry_ask = 0.15;
        cfg.max_entry_ask = 0.85;
        cfg.kelly_sizing = false;
        cfg
    }

    #[test]
    fn thesis_gates_off_preserve_parity() {
        let now = FRI();
        let (dec, _) = decide_entry(&up_inputs(), now, open_of(now), close_of(now), &fresh_state(), session_none().as_ref(), &live_cfg());
        assert_eq!(dec.action, EntryAction::Enter);
    }

    #[test]
    fn skips_when_p_side_below_min() {
        let mut inp = up_inputs();
        inp.p_exo = 0.40; // edge_yes = 0.40 - 0.72 < 0, picks No: p_side = 0.60
        inp.no_buy = 0.30; // edge_no = 0.60 - 0.30 = 0.30
        let mut cfg = thesis_a_cfg();
        cfg.min_p_side = 0.65;
        let now = FRI();
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now), &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Skip);
        assert_eq!(dec.side, Side::No);
    }

    #[test]
    fn skips_when_entry_ask_below_min() {
        let mut inp = up_inputs();
        inp.yes_ask = 0.10; // edge_yes = 0.80, but ask below 0.15 floor
        let now = FRI();
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now), &fresh_state(), session_none().as_ref(), &thesis_a_cfg());
        assert_eq!(dec.action, EntryAction::Skip);
        assert_eq!(dec.side, Side::Yes);
    }

    #[test]
    fn skips_when_entry_ask_above_max() {
        let inp = DecisionInputs {
            p_exo: 0.98,
            dir_p_up: None,
            dir_model_active: false,
            yes_ask: 0.86, // edge_yes = 0.12, ask above 0.85 ceiling
            no_buy: 0.20,
            mid: 0.85,
            sigma_bar_bps: 10.0,
            basis_mom_60s_bps: 0.0,
            regime_at_decision: None,
            clip_index: 0,
            spot_ret_10s_bps: None,
            spot_ret_30s_bps: None,
            spot_ret_60s_bps: None,
            spot_ret_120s_bps: None,
            spot_ret_300s_bps: None,
            spot_ret_600s_bps: None,
            spot_ret_900s_bps: None,
            belief_dwell_s: None,
        };
        let now = FRI();
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now), &fresh_state(), session_none().as_ref(), &thesis_a_cfg());
        assert_eq!(dec.action, EntryAction::Skip);
        assert_eq!(dec.side, Side::Yes);
    }

    #[test]
    fn skips_before_min_secs_from_open() {
        let mut cfg = live_cfg();
        cfg.min_secs_from_open = 30;
        let open = FRI();
        let early = open + 5 * 1_000_000_000;
        let (dec, _) =
            decide_entry(&up_inputs(), early, open, close_of(early), &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Skip);
        let later = open + 45 * 1_000_000_000;
        let (dec2, _) =
            decide_entry(&up_inputs(), later, open, close_of(later), &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec2.action, EntryAction::Enter);
    }

    #[test]
    fn skips_when_p_side_above_max() {
        let mut cfg = live_cfg();
        cfg.max_p_side = 0.85;
        let inp = DecisionInputs {
            p_exo: 0.96,
            dir_p_up: None,
            dir_model_active: false,
            yes_ask: 0.50,
            no_buy: 0.52,
            mid: 0.50,
            sigma_bar_bps: 10.0,
            basis_mom_60s_bps: 0.0,
            regime_at_decision: None,
            clip_index: 0,
            spot_ret_10s_bps: None,
            spot_ret_30s_bps: None,
            spot_ret_60s_bps: None,
            spot_ret_120s_bps: None,
            spot_ret_300s_bps: None,
            spot_ret_600s_bps: None,
            spot_ret_900s_bps: None,
            belief_dwell_s: None,
        };
        let now = FRI();
        let (dec, _) =
            decide_entry(&inp, now, open_of(now), close_of(now), &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Skip);
        assert_eq!(dec.side, Side::Yes);
    }

    #[test]
    fn thesis_a_champion_enters_in_band() {
        let inp = DecisionInputs {
            p_exo: 0.40,
            dir_p_up: None,
            dir_model_active: false,
            yes_ask: 0.55,
            no_buy: 0.48,
            mid: 0.54,
            sigma_bar_bps: 10.0,
            basis_mom_60s_bps: 0.0,
            regime_at_decision: None,
            clip_index: 0,
            spot_ret_10s_bps: None,
            spot_ret_30s_bps: None,
            spot_ret_60s_bps: None,
            spot_ret_120s_bps: None,
            spot_ret_300s_bps: None,
            spot_ret_600s_bps: None,
            spot_ret_900s_bps: None,
            belief_dwell_s: None,
        };
        let now = FRI();
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now), &fresh_state(), session_none().as_ref(), &thesis_a_cfg());
        assert_eq!(dec.action, EntryAction::Enter);
        assert_eq!(dec.side, Side::No); // p_side=0.60, ask=0.48, edge=0.12
        assert!(dec.target_notional > 0.0);
        assert!((dec.target_notional - 50.0).abs() < 1e-6);
    }

    #[test]
    fn skips_expanded_high_flip_regime() {
        let mut cfg = live_cfg();
        cfg.skip_expanded_high_flip = true;
        let mut inp = up_inputs();
        inp.regime_at_decision = Some(Regime::ExpandedHighFlip);
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_calm_low_vol_regime() {
        let mut cfg = live_cfg();
        cfg.skip_calm = true;
        let mut inp = up_inputs();
        inp.regime_at_decision = Some(Regime::CalmLowVol);
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn regime_gate_never_blocks_rearm() {
        // Original skip_expanded_high_flip semantics: a gated regime blocks
        // entries, not re-arming. A disarmed engine in a skipped regime must
        // still return Rearm (and set_armed when edges are low).
        let mut cfg = live_cfg();
        cfg.skip_calm = true;
        cfg.rearm_edge = 0.08;
        let mut inp = up_inputs();
        inp.regime_at_decision = Some(Regime::CalmLowVol);
        // Low edges on both sides so the rearm branch would set armed.
        inp.p_exo = 0.50;
        inp.yes_ask = 0.50;
        inp.no_buy = 0.50;
        let now = FRI();
        let disarmed = EntryState { armed: false, next_entry_ns: i64::MIN };
        let (dec, delta) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &disarmed,
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Rearm);
        assert_eq!(delta.set_armed, Some(true));
    }

    #[test]
    fn dwell_gate_blocks_young_beliefs_only() {
        let mut cfg = live_cfg();
        cfg.min_belief_dwell_s = 30.0;
        let now = FRI();
        // Young dwell: blocked.
        let mut inp = up_inputs();
        inp.belief_dwell_s = Some(4.0);
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now),
            &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Skip);
        // Seasoned dwell: passes through to Enter.
        inp.belief_dwell_s = Some(80.0);
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now),
            &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Enter);
        // Unknown dwell: permissive.
        inp.belief_dwell_s = None;
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now),
            &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Enter);
        // Gate off: young dwell trades.
        cfg.min_belief_dwell_s = 0.0;
        inp.belief_dwell_s = Some(1.0);
        let (dec, _) = decide_entry(&inp, now, open_of(now), close_of(now),
            &fresh_state(), session_none().as_ref(), &cfg);
        assert_eq!(dec.action, EntryAction::Enter);
    }

    #[test]
    fn only_calm_blocks_unclassified_regime() {
        // Parity with the removed market-level calm gate: only_calm requires
        // Some(CalmLowVol); a None regime must not trade.
        let mut cfg = live_cfg();
        cfg.only_calm = true;
        let mut inp = up_inputs();
        inp.regime_at_decision = None;
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_expanded_mixed_regime() {
        let mut cfg = live_cfg();
        cfg.skip_expanded_mixed = true;
        let mut inp = up_inputs();
        inp.regime_at_decision = Some(Regime::ExpandedMixed);
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_open_fav_gap() {
        let mut cfg = live_cfg();
        cfg.skip_open_fav_gap = true;
        let open = FRI();
        let now = open + 2 * 1_000_000_000;
        let mut inp = up_inputs();
        inp.p_exo = 0.95;
        inp.yes_ask = 0.55; // p_side>0.90, ask<0.60 within 5s of open
        let (dec, _) = decide_entry(
            &inp,
            now,
            open,
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn rejects_prod_gap_full_model_book_gap() {
        let mut cfg = live_cfg();
        cfg.skip_open_fav_gap = true;
        cfg.open_fav_p_min = 0.88;
        cfg.open_fav_ask_max = 0.62;
        cfg.open_fav_secs = 300;
        let open = FRI();
        let now = open + 180 * 1_000_000_000;
        let mut inp = up_inputs();
        inp.p_exo = 0.95;
        inp.yes_ask = 0.52;
        let (dec, _) = decide_entry(
            &inp,
            now,
            open,
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn pauses_after_consec_losses() {
        let mut cfg = live_cfg();
        cfg.pause_after_consec_losses = 2;
        let session = SessionGateState { consec_losses: 2 };
        let now = FRI();
        let (dec, _) = decide_entry(
            &up_inputs(),
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            Some(&session),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_expensive_rearm_clip() {
        let mut cfg = live_cfg();
        cfg.max_rearm_entry_ask = 0.70;
        let mut inp = up_inputs();
        inp.clip_index = 1;
        inp.yes_ask = 0.75;
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
    }

    #[test]
    fn skips_spot_misalign_60s() {
        let mut cfg = live_cfg();
        cfg.skip_spot_misalign_s = 60;
        let mut inp = up_inputs(); // enters Yes
        inp.spot_ret_60s_bps = Some(-5.0);
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
        inp.spot_ret_60s_bps = Some(5.0);
        let (dec2, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec2.action, EntryAction::Enter);
    }

    #[test]
    fn skips_spot_against_all_horizons() {
        let mut cfg = live_cfg();
        cfg.skip_spot_against_all = true;
        let mut inp = up_inputs();
        inp.spot_ret_60s_bps = Some(-1.0);
        inp.spot_ret_300s_bps = Some(-2.0);
        inp.spot_ret_600s_bps = Some(-3.0);
        inp.spot_ret_900s_bps = Some(-4.0);
        let now = FRI();
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec.action, EntryAction::Skip);
        inp.spot_ret_300s_bps = Some(1.0);
        let (dec2, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &cfg,
        );
        assert_eq!(dec2.action, EntryAction::Enter);
    }

    #[test]
    fn canon_is_the_exact_frozen_fingerprint() {
        // The canon string IS the config fingerprint shared across shadow
        // startup events, alpha out-json, and the matched-replay tooling.
        // Any change here is a fingerprint break: bump ALL consumers.
        let expected = concat!(
            "{\"edge_threshold\":0.12,\"min_marginal_edge\":0.04,",
            "\"min_entry_sigma_bps\":3,\"max_entry_sigma_bps\":0,",
            "\"skip_saturday\":true,\"rearm_edge\":0.08,",
            "\"clip_cooldown_ms\":5000,\"exit_after_s\":0,",
            "\"enter_within_close_s\":0,\"stop_before_close_s\":90,",
            "\"notional_usdc\":50,\"kelly_sizing\":false,",
            "\"min_p_side\":0,\"max_p_side\":1,\"min_entry_ask\":0,",
            "\"min_secs_from_open\":0,\"min_belief_dwell_s\":0,",
            "\"max_entry_ask\":1,\"vol_sizing_ref_bps\":0,",
            "\"vol_sizing_lo\":0.5,\"vol_sizing_hi\":2,",
            "\"basis_mom_agree\":1,\"basis_mom_disagree\":1,",
            "\"entry_mode\":\"fade\",\"align_min_mid\":0.55,",
            "\"skip_calm\":false,\"only_calm\":false,",
            "\"skip_expanded_mixed\":false,\"skip_expanded_high_flip\":false,",
            "\"skip_open_fav_gap\":false,\"open_fav_p_min\":0.9,",
            "\"open_fav_ask_max\":0.6,\"open_fav_secs\":5,",
            "\"pause_after_consec_losses\":0,\"max_rearm_entry_ask\":0,",
            "\"skip_spot_misalign_s\":0,\"skip_spot_against_all\":false}",
        );
        assert_eq!(frozen_fade_decide_config(50.0).canon(), expected);
    }

    /// Aligned mode's `align_min_mid` floor. This gate had no behavior test
    /// anywhere after the shadow's lane tests were retired with the fade: the
    /// only other Aligned-mode tests cover canon rendering and the dir head.
    #[test]
    fn aligned_mode_requires_the_entered_side_to_clear_align_min_mid() {
        let mut cfg = live_cfg();
        cfg.entry_mode = EntryMode::Aligned;
        cfg.align_min_mid = 0.85;
        let now = FRI();
        let decide_at_mid = |mid: f64| {
            let mut inp = up_inputs();
            inp.mid = mid;
            decide_entry(
                &inp,
                now,
                open_of(now),
                close_of(now),
                &fresh_state(),
                session_none().as_ref(),
                &cfg,
            )
            .0
        };

        // Below the floor: the favourite is not favourite enough.
        assert_eq!(decide_at_mid(0.71).action, EntryAction::Skip);
        assert_eq!(decide_at_mid(0.8499).action, EntryAction::Skip);
        // At and above it: the gate lets the entry through.
        let dec = decide_at_mid(0.90);
        assert_eq!(dec.action, EntryAction::Enter);
        assert_eq!(dec.side, Side::Yes);
        // Fade mode ignores the floor entirely, so the same low mid enters.
        let mut fade = cfg;
        fade.entry_mode = EntryMode::Fade;
        let mut inp = up_inputs();
        inp.mid = 0.71;
        let (dec, _) = decide_entry(
            &inp,
            now,
            open_of(now),
            close_of(now),
            &fresh_state(),
            session_none().as_ref(),
            &fade,
        );
        assert_eq!(dec.action, EntryAction::Enter);
    }

    #[test]
    fn aligned_min_mid_reads_the_no_side_as_one_minus_mid() {
        let mut cfg = live_cfg();
        cfg.entry_mode = EntryMode::Aligned;
        cfg.align_min_mid = 0.85;
        let now = FRI();
        // Down belief: the No leg is the favourite, so the gate must look at
        // 1 - mid, not mid.
        let mut inp = up_inputs();
        inp.p_exo = 0.10;
        inp.yes_ask = 0.70;
        inp.no_buy = 0.72;
        let decide_at_mid = |mid: f64| {
            let mut inp = inp;
            inp.mid = mid;
            decide_entry(
                &inp,
                now,
                open_of(now),
                close_of(now),
                &fresh_state(),
                session_none().as_ref(),
                &cfg,
            )
            .0
        };
        // mid 0.29 -> No-side mid 0.71, below the floor.
        assert_eq!(decide_at_mid(0.29).action, EntryAction::Skip);
        // mid 0.10 -> No-side mid 0.90, clears it.
        let dec = decide_at_mid(0.10);
        assert_eq!(dec.action, EntryAction::Enter);
        assert_eq!(dec.side, Side::No);
    }

    #[test]
    fn canon_is_valid_json_and_aligned_mode_renders() {
        let mut cfg = frozen_fade_decide_config(50.0);
        cfg.entry_mode = EntryMode::Aligned;
        let canon = cfg.canon();
        let parsed: serde_json::Value = serde_json::from_str(&canon).expect("canon parses");
        assert_eq!(parsed["entry_mode"], "aligned");
        assert_eq!(parsed["edge_threshold"], 0.12);
        assert_eq!(parsed["stop_before_close_s"], 90);
    }
}
