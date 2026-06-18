//! Exogenous fade — quant-grade reference strategy.
//!
//! Pipeline: `ExoState` → `AlphaModel::belief` → `decide_entry` (SSOT) →
//! `pm_risk` sizing → `OrderRequest` emission. Exit via timed taker sell at bid.
//!
//! This is the canonical implementation of the validated F1 family. Backtest and
//! live must share `pm_alpha::decide_entry`; this module only wires belief
//! construction and the runner execution layer.

use crate::{Ctx, OrderRequest, Side as StratSide, Strategy, StrategyOutput};
use pm_alpha::{
    AlphaModel, AlphaModelConfig, DecideConfig, DecisionInputs, EntryAction, EntryState,
    EntryStateDelta, ExoState, MarketMeta, Token, VolEstimator, decide_entry,
    harness::EntryMode, harness::Side as AlphaSide, model::belief,
};
use pm_risk::fractional_kelly_stake;
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};
use serde::{Deserialize, Serialize};

const NS_PER_S: i64 = 1_000_000_000;

/// Serializable strategy profile (load from TOML / walk-forward flags).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ExoFadeConfig {
    pub bankroll_usdc: f64,
    pub clip_usdc: f64,
    pub kelly_fraction: f64,
    pub edge_threshold: f64,
    pub min_marginal_edge: f64,
    pub max_clips: u32,
    pub rearm_edge: f64,
    pub clip_cooldown_ms: u64,
    pub exit_after_s: u32,
    pub stop_before_close_s: u32,
    pub enter_within_close_s: u32,
    pub vol_lookback_s: u32,
    pub perp_price_weight: f64,
    pub decision_dt_ms: u64,
    pub latency_ms: u64,
    pub entry_mode: String,
    pub align_min_mid: f64,
    pub token: String,
    pub window_secs: u32,
    // Decision-layer gates (SSOT: pm_alpha::decide).
    pub min_p_side: f64,
    pub max_p_side: f64,
    pub min_entry_ask: f64,
    pub max_entry_ask: f64,
    pub skip_open_fav_gap: bool,
    pub open_fav_p_min: f64,
    pub open_fav_ask_max: f64,
    pub open_fav_secs: u32,
    pub skip_spot_misalign_s: u32,
    pub skip_spot_against_all: bool,
    pub pause_after_consec_losses: u32,
    pub max_rearm_entry_ask: f64,
    pub skip_expanded_high_flip: bool,
}

impl Default for ExoFadeConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 1000.0,
            clip_usdc: 25.0,
            kelly_fraction: 0.0,
            edge_threshold: 0.16,
            min_marginal_edge: 0.08,
            max_clips: 2,
            rearm_edge: 0.08,
            clip_cooldown_ms: 5000,
            exit_after_s: 30,
            stop_before_close_s: 90,
            enter_within_close_s: 0,
            vol_lookback_s: 3600,
            perp_price_weight: 0.75,
            decision_dt_ms: 1000,
            latency_ms: 150,
            entry_mode: "fade".into(),
            align_min_mid: 0.55,
            token: "btc".into(),
            window_secs: 300,
            min_p_side: 0.0,
            max_p_side: 1.0,
            min_entry_ask: 0.0,
            max_entry_ask: 1.0,
            skip_open_fav_gap: false,
            open_fav_p_min: 0.90,
            open_fav_ask_max: 0.60,
            open_fav_secs: 5,
            skip_spot_misalign_s: 0,
            skip_spot_against_all: false,
            pause_after_consec_losses: 0,
            max_rearm_entry_ask: 0.0,
            skip_expanded_high_flip: false,
        }
    }
}

impl ExoFadeConfig {
    pub fn champion_1k() -> Self {
        Self::default()
    }

    /// May/June 2026 regime profile — book-aware fade, tuned only on post-April tape.
    /// Blocks saturated BSM beliefs vs mid-range asks (live drawdown failure mode).
    pub fn mayjune_btc5m() -> Self {
        Self {
            edge_threshold: 0.14,
            min_marginal_edge: 0.06,
            exit_after_s: 0,
            min_entry_ask: 0.45,
            max_entry_ask: 0.85,
            max_p_side: 0.92,
            skip_open_fav_gap: true,
            open_fav_p_min: 0.88,
            open_fav_ask_max: 0.62,
            open_fav_secs: 300,
            skip_spot_misalign_s: 60,
            ..Self::default()
        }
    }

    fn token(&self) -> Token {
        match self.token.to_ascii_lowercase().as_str() {
            "eth" => Token::Eth,
            "sol" => Token::Sol,
            "xrp" => Token::Xrp,
            _ => Token::Btc,
        }
    }

    fn entry_mode(&self) -> EntryMode {
        if self.entry_mode.eq_ignore_ascii_case("aligned") {
            EntryMode::Aligned
        } else {
            EntryMode::Fade
        }
    }

    fn decide_config(&self) -> DecideConfig {
        DecideConfig {
            edge_threshold: self.edge_threshold,
            min_marginal_edge: self.min_marginal_edge,
            min_entry_sigma_bps: 0.0,
            max_entry_sigma_bps: 0.0,
            skip_saturday: false,
            rearm_edge: self.rearm_edge,
            clip_cooldown_ms: self.clip_cooldown_ms,
            exit_after_s: self.exit_after_s,
            enter_within_close_s: self.enter_within_close_s,
            stop_before_close_s: self.stop_before_close_s,
            notional_usdc: self.clip_usdc,
            kelly_sizing: false,
            min_p_side: self.min_p_side,
            max_p_side: self.max_p_side,
            min_entry_ask: self.min_entry_ask,
            max_entry_ask: self.max_entry_ask,
            min_secs_from_open: 0,
            vol_sizing_ref_bps: 0.0,
            vol_sizing_lo: 0.5,
            vol_sizing_hi: 2.0,
            basis_mom_agree: 1.0,
            basis_mom_disagree: 1.0,
            entry_mode: self.entry_mode(),
            align_min_mid: self.align_min_mid,
            skip_expanded_high_flip: self.skip_expanded_high_flip,
            skip_open_fav_gap: self.skip_open_fav_gap,
            open_fav_p_min: self.open_fav_p_min,
            open_fav_ask_max: self.open_fav_ask_max,
            open_fav_secs: self.open_fav_secs,
            pause_after_consec_losses: self.pause_after_consec_losses,
            max_rearm_entry_ask: self.max_rearm_entry_ask,
            skip_spot_misalign_s: self.skip_spot_misalign_s,
            skip_spot_against_all: self.skip_spot_against_all,
        }
    }

    fn alpha_model(&self) -> AlphaModel {
        AlphaModel {
            cfg: AlphaModelConfig {
                vol_lookback_s: self.vol_lookback_s,
                vol_sample_dt_s: 1,
                vol_estimator: VolEstimator::Realized,
                momentum_lookback_s: 0,
                momentum_weight: 1.0,
                xasset_weight: 0.0,
                perp_price_weight: self.perp_price_weight,
            },
            calibrator: None,
            dir_model: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OpenLeg {
    side: StratSide,
    shares: f64,
    entry_ts_ns: i64,
    exit_due_ns: i64,
    exited: bool,
}

/// Per-market state for the fade strategy.
pub struct ExoFadeStrategy {
    cfg: ExoFadeConfig,
    model: AlphaModel,
    strike: Option<f64>,
    entry_state: EntryState,
    n_clips: u32,
    armed: bool,
    last_decision_ns: i64,
    open_legs: Vec<OpenLeg>,
    gate_stats: ExoFadeGateStats,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
pub struct ExoFadeGateStats {
    pub decision_ticks: u64,
    pub entries: u64,
    pub exits: u64,
    pub skips_deadline: u64,
    pub skips_no_edge: u64,
}

impl ExoFadeStrategy {
    pub fn new(cfg: ExoFadeConfig) -> Self {
        let model = cfg.alpha_model();
        Self {
            cfg,
            model,
            strike: None,
            entry_state: EntryState {
                armed: true,
                next_entry_ns: i64::MIN,
            },
            n_clips: 0,
            armed: true,
            last_decision_ns: i64::MIN,
            open_legs: Vec::new(),
            gate_stats: ExoFadeGateStats::default(),
        }
    }

    pub fn gate_stats(&self) -> ExoFadeGateStats {
        self.gate_stats
    }

    fn market_meta(&self, ctx: &Ctx) -> Option<MarketMeta> {
        let strike = self.strike?;
        let close_ns = ctx.market_close_ns;
        let open_ns = close_ns.saturating_sub(self.cfg.window_secs as i64 * NS_PER_S);
        Some(MarketMeta {
            token: self.cfg.token(),
            window_secs: self.cfg.window_secs,
            open_ts_ns: open_ns,
            close_ts_ns: close_ns,
            strike,
        })
    }

    fn resolve_strike(&mut self, ctx: &Ctx, spot: &SpotHistory) {
        if self.strike.is_some() {
            return;
        }
        let close_ns = ctx.market_close_ns;
        let open_ns = close_ns.saturating_sub(self.cfg.window_secs as i64 * NS_PER_S);
        if let Some(px) = spot.price_at_or_before(open_ns) {
            self.strike = Some(px);
        }
    }

    fn no_buy_price(&self, event: &ReplayEvent, ctx: &Ctx) -> f64 {
        if ctx.no_ask > 0.0 {
            ctx.no_ask as f64
        } else {
            (1.0 - event.yes_ask as f64).clamp(0.01, 0.99)
        }
    }

    fn apply_entry_delta(&mut self, delta: EntryStateDelta) {
        if let Some(a) = delta.set_armed {
            self.armed = a;
            self.entry_state.armed = a;
        }
        if let Some(ns) = delta.set_next_entry_ns {
            self.entry_state.next_entry_ns = ns;
        }
        if delta.inc_clips {
            self.n_clips += 1;
        }
    }

    fn size_notional(&self, ctx: &Ctx, p_exo: f64, side_ask: f64) -> f64 {
        let base = self.cfg.clip_usdc;
        let equity = ctx.cash_usdc.max(self.cfg.bankroll_usdc);
        if self.cfg.kelly_fraction > 0.0 {
            let k = fractional_kelly_stake(p_exo, side_ask, equity, self.cfg.kelly_fraction, base);
            if k > 0.0 {
                return k;
            }
        }
        base.min(equity * 0.05)
    }

    fn alpha_side_to_strat(side: AlphaSide) -> StratSide {
        match side {
            AlphaSide::Yes => StratSide::BuyYes,
            AlphaSide::No => StratSide::BuyNo,
        }
    }

    fn exit_side(entry: StratSide) -> StratSide {
        match entry {
            StratSide::BuyYes => StratSide::SellYes,
            StratSide::BuyNo => StratSide::SellNo,
            other => other,
        }
    }

    fn process_exits(&mut self, event: &ReplayEvent, orders: &mut Vec<OrderRequest>) {
        if self.cfg.exit_after_s == 0 {
            return;
        }
        for leg in &mut self.open_legs {
            if leg.exited || event.ts_ns < leg.exit_due_ns {
                continue;
            }
            let shares = leg.shares;
            if shares <= 0.0 {
                leg.exited = true;
                continue;
            }
            orders.push(OrderRequest {
                side: Self::exit_side(leg.side),
                shares,
                max_depth: 5,
                limit_price: None,
                tag: "exo_fade_exit",
            });
            leg.exited = true;
            self.gate_stats.exits += 1;
        }
        self.open_legs.retain(|l| !l.exited || l.entry_ts_ns > event.ts_ns);
    }

    fn maybe_enter(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        orders: &mut Vec<OrderRequest>,
    ) {
        if self.n_clips >= self.cfg.max_clips {
            return;
        }
        if event.ts_ns < self.entry_state.next_entry_ns {
            return;
        }
        let dt_ns = self.cfg.decision_dt_ms as i64 * 1_000_000;
        if self.last_decision_ns != i64::MIN && event.ts_ns - self.last_decision_ns < dt_ns {
            return;
        }
        self.last_decision_ns = event.ts_ns;

        let Some(meta) = self.market_meta(ctx) else {
            return;
        };
        let exo = ExoState {
            market: meta,
            spot,
            now_ns: event.ts_ns,
            perp: None,
            ref_spot: None,
        };
        let Some(b) = belief(&exo, &self.model.cfg) else {
            return;
        };
        let yes_ask = event.yes_ask as f64;
        let no_buy = self.no_buy_price(event, ctx);
        let mid = event.yes_mid as f64;

        let inputs = DecisionInputs {
            p_exo: b.p_up,
            dir_p_up: None,
            dir_model_active: false,
            yes_ask,
            no_buy,
            mid,
            sigma_bar_bps: b.sigma_bar_bps,
            basis_mom_60s_bps: 0.0,
            regime_at_decision: None,
            clip_index: self.n_clips,
            spot_ret_10s_bps: None,
            spot_ret_30s_bps: None,
            spot_ret_60s_bps: None,
            spot_ret_120s_bps: None,
            spot_ret_300s_bps: None,
            spot_ret_600s_bps: None,
            spot_ret_900s_bps: None,
        };

        self.gate_stats.decision_ticks += 1;
        let dcfg = self.cfg.decide_config();
        let open_ns = ctx
            .market_close_ns
            .saturating_sub(self.cfg.window_secs as i64 * NS_PER_S);
        let (decision, delta) = decide_entry(
            &inputs,
            event.ts_ns,
            open_ns,
            ctx.market_close_ns,
            &EntryState {
                armed: self.armed,
                next_entry_ns: self.entry_state.next_entry_ns,
            },
            None,
            &dcfg,
        );

        match decision.action {
            EntryAction::Rearm => {
                self.apply_entry_delta(delta);
                return;
            }
            EntryAction::Skip => {
                self.apply_entry_delta(delta);
                self.gate_stats.skips_no_edge += 1;
                return;
            }
            EntryAction::Enter => {}
        }

        let side_ask = match decision.side {
            AlphaSide::Yes => yes_ask,
            AlphaSide::No => no_buy,
        };
        let p_side = match decision.side {
            AlphaSide::Yes => b.p_up,
            AlphaSide::No => 1.0 - b.p_up,
        };
        let notional = self
            .size_notional(ctx, p_side, side_ask)
            .min(decision.target_notional.max(0.0));
        if notional < 1.0 {
            return;
        }
        let shares = (notional / side_ask).max(0.01);
        let strat_side = Self::alpha_side_to_strat(decision.side);

        orders.push(OrderRequest {
            side: strat_side,
            shares,
            max_depth: 5,
            limit_price: Some(decision.marketable_limit_price as f32),
            tag: "exo_fade_entry",
        });

        let exit_due = if self.cfg.exit_after_s > 0 {
            event.ts_ns + self.cfg.exit_after_s as i64 * NS_PER_S
        } else {
            ctx.market_close_ns
        };
        self.open_legs.push(OpenLeg {
            side: strat_side,
            shares,
            entry_ts_ns: event.ts_ns,
            exit_due_ns: exit_due,
            exited: self.cfg.exit_after_s == 0,
        });

        self.apply_entry_delta(delta);
        self.gate_stats.entries += 1;
    }
}

impl Strategy for ExoFadeStrategy {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        self.resolve_strike(ctx, spot);
        let mut orders = Vec::new();
        self.process_exits(event, &mut orders);
        self.maybe_enter(event, ctx, spot, &mut orders);
        StrategyOutput { orders }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::SpotTick;

    fn spot_flat(price: f64, secs: i64) -> SpotHistory {
        let ticks: Vec<SpotTick> = (0..secs)
            .map(|s| SpotTick {
                ts_ns: s * NS_PER_S,
                price,
                quantity: 1.0,
                is_buyer_maker: false,
            })
            .collect();
        SpotHistory::new(ticks)
    }

    #[test]
    fn champion_config_defaults() {
        let cfg = ExoFadeConfig::champion_1k();
        assert!((cfg.edge_threshold - 0.16).abs() < 1e-9);
        assert_eq!(cfg.exit_after_s, 30);
        assert!((cfg.clip_usdc - 25.0).abs() < 1e-9);
    }
}