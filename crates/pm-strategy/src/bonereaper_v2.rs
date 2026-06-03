//! BonereaperDirectional v2.1 — directional pyramid + late directional +
//! high-skew load + convex tail. Taker-only until cancel infra lands.
//!
//! Failure-mode notes shaping this design:
//!
//!   * **Maker stranding** (legacy polymarket-exec bug) — posting maker bids
//!     near mid invites adverse selection. We stay taker-only until the
//!     runner gains cancel-on-adverse-move infrastructure.
//!
//!   * **Whipsaw at high yes_mid** — naively treating yes_mid ≥ 0.80 as
//!     committed gets crushed in high-vol regimes. The high-skew lane has
//!     three guards: spot agrees, skew sustained ≥5s, regime != Whipsaw.
//!
//! Lanes:
//!
//!   1. **Early directional probe** (0–30s) — small clip on composite signal,
//!      establishes a position before the pile-on.
//!   2. **Mid-ladder** (30–240s) — pyramids up as the book moves further
//!      in our direction. Capped at N rungs × min-step price movement.
//!   3. **Late directional** (240–300s) — permissive directional taker.
//!      Workhorse lane — high fill rate, modest edge per fill.
//!   4. **High-skew load** (any phase) — once yes_mid ≥ 0.80 (or ≤ 0.20)
//!      AND whipsaw guards pass, layers multiple clips at high prices.
//!      Matches real Bonereaper's late favourite_load behaviour.
//!   5. **Convex tail** — small one-shot cheap bet on the losing side.

use crate::regime::{BtcRegime, BtcRegimeSnapshot, WhipsawRiskSnapshot};
use crate::signals::{Ring, direction_score};
use crate::spot_momentum::spot_momentum_stack;
use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory, compute_trade_flow};
use serde::{Deserialize, Serialize};

const BETTING_WINDOW_SECS: i64 = 300;
const MOM_WINDOW: usize = 32;
const MICRO_DEV_SCALE: f32 = 0.6;
const TRADE_FLOW_LOOKBACK_NS: i64 = 60 * 1_000_000_000;

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct BonereaperV2GateStats {
    pub late_confirm_checks: u64,
    pub late_confirm_price_fail: u64,
    pub late_confirm_book_favourite_fail: u64,
    pub late_confirm_book_skew_fail: u64,
    pub late_confirm_model_missing: u64,
    pub late_confirm_model_side_fail: u64,
    pub late_confirm_model_confidence_fail: u64,
    pub late_confirm_model_risk_fail: u64,
    pub late_confirm_model_side_p_fail: u64,
    pub late_confirm_model_edge_fail: u64,
    pub late_confirm_whipsaw_fail: u64,
    pub late_confirm_low_vol_fail: u64,
    pub late_confirm_market_range_fail: u64,
    pub late_confirm_recent_regime_fail: u64,
    pub late_confirm_side_lock_fail: u64,
    pub late_confirm_shares_fail: u64,
    pub late_confirm_emits: u64,

    pub high_skew_checks: u64,
    pub high_skew_regime_fail: u64,
    pub high_skew_threshold_fail: u64,
    pub high_skew_sustain_fail: u64,
    pub high_skew_spot_alignment_fail: u64,
    pub high_skew_price_fail: u64,
    pub high_skew_model_missing: u64,
    pub high_skew_model_side_fail: u64,
    pub high_skew_model_confidence_fail: u64,
    pub high_skew_model_risk_fail: u64,
    pub high_skew_model_side_p_fail: u64,
    pub high_skew_model_edge_fail: u64,
    pub high_skew_whipsaw_fail: u64,
    pub high_skew_low_vol_fail: u64,
    pub high_skew_recent_regime_fail: u64,
    pub high_skew_side_lock_fail: u64,
    pub high_skew_shares_fail: u64,
    pub high_skew_emits: u64,

    pub late_favourite_window_checks: u64,
    pub late_favourite_capacity_fail: u64,
    pub late_favourite_refresh_fail: u64,
    pub late_favourite_checks: u64,
    pub late_favourite_skew_fail: u64,
    pub late_favourite_sustain_fail: u64,
    pub late_favourite_alignment_fail: u64,
    pub late_favourite_price_fail: u64,
    pub late_favourite_model_missing: u64,
    pub late_favourite_model_side_fail: u64,
    pub late_favourite_model_confidence_fail: u64,
    pub late_favourite_model_risk_fail: u64,
    pub late_favourite_model_side_p_fail: u64,
    pub late_favourite_model_edge_fail: u64,
    pub late_favourite_model_direction_fail: u64,
    pub late_favourite_whipsaw_fail: u64,
    pub late_favourite_reversal_pressure_fail: u64,
    pub late_favourite_path_efficiency_fail: u64,
    pub late_favourite_low_vol_fail: u64,
    pub late_favourite_market_range_fail: u64,
    pub late_favourite_adverse_momentum_fail: u64,
    pub late_favourite_entry_pullback_fail: u64,
    pub late_favourite_avg_entry_drawdown_fail: u64,
    pub late_favourite_recent_regime_fail: u64,
    pub late_favourite_side_lock_fail: u64,
    pub late_favourite_shares_fail: u64,
    pub late_favourite_emits: u64,
}

impl BonereaperV2GateStats {
    pub fn add_assign(&mut self, other: Self) {
        self.late_confirm_checks += other.late_confirm_checks;
        self.late_confirm_price_fail += other.late_confirm_price_fail;
        self.late_confirm_book_favourite_fail += other.late_confirm_book_favourite_fail;
        self.late_confirm_book_skew_fail += other.late_confirm_book_skew_fail;
        self.late_confirm_model_missing += other.late_confirm_model_missing;
        self.late_confirm_model_side_fail += other.late_confirm_model_side_fail;
        self.late_confirm_model_confidence_fail += other.late_confirm_model_confidence_fail;
        self.late_confirm_model_risk_fail += other.late_confirm_model_risk_fail;
        self.late_confirm_model_side_p_fail += other.late_confirm_model_side_p_fail;
        self.late_confirm_model_edge_fail += other.late_confirm_model_edge_fail;
        self.late_confirm_whipsaw_fail += other.late_confirm_whipsaw_fail;
        self.late_confirm_low_vol_fail += other.late_confirm_low_vol_fail;
        self.late_confirm_market_range_fail += other.late_confirm_market_range_fail;
        self.late_confirm_recent_regime_fail += other.late_confirm_recent_regime_fail;
        self.late_confirm_side_lock_fail += other.late_confirm_side_lock_fail;
        self.late_confirm_shares_fail += other.late_confirm_shares_fail;
        self.late_confirm_emits += other.late_confirm_emits;

        self.high_skew_checks += other.high_skew_checks;
        self.high_skew_regime_fail += other.high_skew_regime_fail;
        self.high_skew_threshold_fail += other.high_skew_threshold_fail;
        self.high_skew_sustain_fail += other.high_skew_sustain_fail;
        self.high_skew_spot_alignment_fail += other.high_skew_spot_alignment_fail;
        self.high_skew_price_fail += other.high_skew_price_fail;
        self.high_skew_model_missing += other.high_skew_model_missing;
        self.high_skew_model_side_fail += other.high_skew_model_side_fail;
        self.high_skew_model_confidence_fail += other.high_skew_model_confidence_fail;
        self.high_skew_model_risk_fail += other.high_skew_model_risk_fail;
        self.high_skew_model_side_p_fail += other.high_skew_model_side_p_fail;
        self.high_skew_model_edge_fail += other.high_skew_model_edge_fail;
        self.high_skew_whipsaw_fail += other.high_skew_whipsaw_fail;
        self.high_skew_low_vol_fail += other.high_skew_low_vol_fail;
        self.high_skew_recent_regime_fail += other.high_skew_recent_regime_fail;
        self.high_skew_side_lock_fail += other.high_skew_side_lock_fail;
        self.high_skew_shares_fail += other.high_skew_shares_fail;
        self.high_skew_emits += other.high_skew_emits;

        self.late_favourite_window_checks += other.late_favourite_window_checks;
        self.late_favourite_capacity_fail += other.late_favourite_capacity_fail;
        self.late_favourite_refresh_fail += other.late_favourite_refresh_fail;
        self.late_favourite_checks += other.late_favourite_checks;
        self.late_favourite_skew_fail += other.late_favourite_skew_fail;
        self.late_favourite_sustain_fail += other.late_favourite_sustain_fail;
        self.late_favourite_alignment_fail += other.late_favourite_alignment_fail;
        self.late_favourite_price_fail += other.late_favourite_price_fail;
        self.late_favourite_model_missing += other.late_favourite_model_missing;
        self.late_favourite_model_side_fail += other.late_favourite_model_side_fail;
        self.late_favourite_model_confidence_fail += other.late_favourite_model_confidence_fail;
        self.late_favourite_model_risk_fail += other.late_favourite_model_risk_fail;
        self.late_favourite_model_side_p_fail += other.late_favourite_model_side_p_fail;
        self.late_favourite_model_edge_fail += other.late_favourite_model_edge_fail;
        self.late_favourite_model_direction_fail += other.late_favourite_model_direction_fail;
        self.late_favourite_whipsaw_fail += other.late_favourite_whipsaw_fail;
        self.late_favourite_reversal_pressure_fail += other.late_favourite_reversal_pressure_fail;
        self.late_favourite_path_efficiency_fail += other.late_favourite_path_efficiency_fail;
        self.late_favourite_low_vol_fail += other.late_favourite_low_vol_fail;
        self.late_favourite_market_range_fail += other.late_favourite_market_range_fail;
        self.late_favourite_adverse_momentum_fail += other.late_favourite_adverse_momentum_fail;
        self.late_favourite_entry_pullback_fail += other.late_favourite_entry_pullback_fail;
        self.late_favourite_avg_entry_drawdown_fail += other.late_favourite_avg_entry_drawdown_fail;
        self.late_favourite_recent_regime_fail += other.late_favourite_recent_regime_fail;
        self.late_favourite_side_lock_fail += other.late_favourite_side_lock_fail;
        self.late_favourite_shares_fail += other.late_favourite_shares_fail;
        self.late_favourite_emits += other.late_favourite_emits;
    }
}

/// Features available to the fill-time reversal-risk score. These are the
/// Phase-1 Binance flow features (recomputed replay-safe at the load decision
/// from the same spot tape + the same `signed_flow_and_adverse` /
/// `spot_returns_and_accel` calls the runner uses) plus a handful of base
/// model/book features. HIGH score = fragile/likely-reversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReversalScoreFeature {
    BinanceFlowImbal5s,
    BinanceFlowImbal15s,
    BinanceFlowImbal30s,
    BinanceAdverseVol5s,
    BinanceAdverseVol15s,
    BinanceAdverseVol30s,
    BinanceLargeAdverseCount10s,
    BinanceTradeIntensity15s,
    SpotRet5s,
    SpotRet15s,
    SpotRet30s,
    SpotAccel15sVs30s,
    SpotAccel5sVs15s,
    SideModelP,
    RiskScore,
    Price,
    SideEdgeVsFill,
}

/// One standardized logistic term: `coef * (feature - mean) / std`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ReversalScoreTerm {
    pub feature: ReversalScoreFeature,
    pub mean: f64,
    pub std: f64,
    pub coef: f64,
}

/// Phase-2-fit logistic coefficients for the reversal-risk score. Loaded from a
/// JSON file. `score = sigmoid(intercept + sum_i coef_i*(x_i - mean_i)/std_i)`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReversalScoreCoeffs {
    pub intercept: f64,
    pub terms: Vec<ReversalScoreTerm>,
}

impl ReversalScoreCoeffs {
    /// Replay-safe values for the score features at a single load decision.
    /// `score` consumes this via `ReversalScoreCoeffs::score`.
    pub fn score(&self, f: &ReversalScoreFeatureValues) -> f32 {
        let mut logit = self.intercept;
        for term in &self.terms {
            let x = f.get(term.feature);
            let std = if term.std.abs() < 1e-12 {
                1.0
            } else {
                term.std
            };
            logit += term.coef * (x - term.mean) / std;
        }
        sigmoid(logit)
    }
}

/// Concrete feature values at one load decision. Built from the same inputs the
/// Phase-1 logger uses (`BinanceFlowFeatures::compute` in the runner) so the
/// score sees the same numbers that get logged.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReversalScoreFeatureValues {
    pub flow_imbal_5s: f64,
    pub flow_imbal_15s: f64,
    pub flow_imbal_30s: f64,
    pub adverse_vol_5s: f64,
    pub adverse_vol_15s: f64,
    pub adverse_vol_30s: f64,
    pub large_adverse_count_10s: f64,
    pub trade_intensity_15s: f64,
    pub spot_ret_5s: f64,
    pub spot_ret_15s: f64,
    pub spot_ret_30s: f64,
    pub spot_accel_15s_vs_30s: f64,
    pub spot_accel_5s_vs_15s: f64,
    pub side_model_p: f64,
    pub risk_score: f64,
    pub price: f64,
    pub side_edge_vs_fill: f64,
}

impl ReversalScoreFeatureValues {
    fn get(&self, feature: ReversalScoreFeature) -> f64 {
        use ReversalScoreFeature::*;
        match feature {
            BinanceFlowImbal5s => self.flow_imbal_5s,
            BinanceFlowImbal15s => self.flow_imbal_15s,
            BinanceFlowImbal30s => self.flow_imbal_30s,
            BinanceAdverseVol5s => self.adverse_vol_5s,
            BinanceAdverseVol15s => self.adverse_vol_15s,
            BinanceAdverseVol30s => self.adverse_vol_30s,
            BinanceLargeAdverseCount10s => self.large_adverse_count_10s,
            BinanceTradeIntensity15s => self.trade_intensity_15s,
            SpotRet5s => self.spot_ret_5s,
            SpotRet15s => self.spot_ret_15s,
            SpotRet30s => self.spot_ret_30s,
            SpotAccel15sVs30s => self.spot_accel_15s_vs_30s,
            SpotAccel5sVs15s => self.spot_accel_5s_vs_15s,
            SideModelP => self.side_model_p,
            RiskScore => self.risk_score,
            Price => self.price,
            SideEdgeVsFill => self.side_edge_vs_fill,
        }
    }
}

/// Linear interpolation, clamped to `[a, b]` order-independent.
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t.clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BonereaperV2Config {
    pub bankroll_usdc: f64,
    pub max_clip_usdc: f64,
    pub tick: f64,
    pub disable_internal_model_gates: bool,

    // Market-neutral participation sleeve. This is the Bonereaper-like
    // all-market layer: small paired maker bids on both outcomes when pair cost
    // is attractive, with inventory repair before directional exposure grows.
    pub participation_clip_frac: f32,
    pub participation_start_secs: f32,
    pub participation_stop_secs_before_close: f32,
    pub participation_max_pair_cost: f32,
    pub participation_min_price: f32,
    pub participation_max_price: f32,
    pub participation_max_inventory_delta_shares: f64,
    pub participation_repair_inventory_delta_shares: f64,
    pub participation_refresh_secs: f32,
    pub participation_max_orders_per_leg: usize,

    // Hedge-first arb-anchored base. A real BTC-5m operator limits losses
    // structurally: buy the YES+NO pair early for a combined taker cost below
    // ~0.98 (locking arb since the engine pays $1 per winning share), stay
    // balanced, and make the late directional bet a small overlay on top of
    // that hedged base. Everything here is inert by default
    // (`hedged_base_max_notional_usdc` = 0 means the lane never opens, and the
    // overlay cap is effectively infinite).
    pub hedged_base_enabled: bool,
    pub hedged_base_max_secs_in: f32,
    pub hedged_base_max_pair_cost: f32,
    pub hedged_base_min_minority_leg_frac: f32,
    pub hedged_base_clip_usdc: f32,
    pub hedged_base_max_notional_usdc: f32,
    /// Cap `directional_notional_emitted` to this fraction of
    /// `hedged_base_notional_emitted`. The huge default makes the directional
    /// lanes unconstrained; with hedge-first mode on, set ~0.064 to keep the
    /// late directional bet a ~6% overlay on the hedged base.
    pub late_directional_overlay_frac: f32,

    pub early_phase_end_secs: f32,
    pub mid_phase_end_secs: f32,

    pub min_composite_direction: f32,
    pub mid_ladder_min_step: f32,
    pub mid_ladder_max_rungs: usize,
    pub early_clip_frac: f32,
    pub mid_clip_frac: f32,
    pub late_clip_frac: f32,
    pub late_max_fires: usize,
    pub late_refresh_secs: f32,
    pub late_sweep_depth: usize,
    pub late_confirm_min_model_confidence: f32,
    pub late_confirm_max_model_risk: f32,
    pub late_confirm_min_model_side_p: f32,
    pub late_confirm_min_model_edge: f32,
    pub late_confirm_min_book_skew: f32,
    pub late_confirm_max_whipsaw_score: f32,
    pub late_confirm_min_realized_vol_180s_bps: f32,
    pub late_confirm_max_observed_range: f32,

    // Replay-safe logistic entry gate trained from historical fills. This uses
    // only current tick/model/regime state plus prior completed-market ranges.
    pub recent_regime_gate_enabled: bool,
    pub recent_regime_gate_min_edge: f32,
    pub recent_regime_gate_late_confirm: bool,
    pub recent_regime_gate_high_skew: bool,
    pub recent_regime_gate_late_favourite: bool,

    // Phase-3 flow-based reversal-risk score modulators. OFF BY DEFAULT.
    // `reversal_score_enabled` is the master gate; with it false (or no
    // `reversal_score_coeffs` supplied) the score path is skipped entirely and
    // orders are byte-identical to baseline. The score in [0,1] is HIGH for
    // fragile/likely-reversal loads. Two soft (continuous) modulators:
    //   a. Convex coverage: effective tail coverage target
    //      = lerp(cov_min, cov_max, score). Fires convex protection only on
    //      fragile loads. Inert defaults (NaN) fall back to the base coverage.
    //   b. Directional sizing: late-lane clip scaled by
    //      size_mult = lerp(size_floor, size_ceiling, 1 - score). Inert
    //      defaults (1.0/1.0) leave sizing unchanged.
    pub reversal_score_enabled: bool,
    #[serde(skip)]
    pub reversal_score_coeffs: Option<ReversalScoreCoeffs>,
    /// Convex-tail coverage at score=0 (stable). NaN => use the base coverage
    /// (`tail_target_favourite_loss_coverage_frac`) unchanged.
    pub reversal_score_cov_min: f32,
    /// Convex-tail coverage at score=1 (fragile). NaN => use the base coverage.
    pub reversal_score_cov_max: f32,
    /// Late-lane size multiplier at score=1 (fragile). 1.0 => no change.
    pub reversal_score_size_floor: f32,
    /// Late-lane size multiplier at score=0 (stable). 1.0 => no change.
    pub reversal_score_size_ceiling: f32,

    /// Per-lane directional size multipliers (1.0 => no change, multiplied
    /// into each lane's clip). Used for lane-targeted amputation in whippy
    /// regimes without disturbing the shared late_clip_frac path.
    pub lane_size_late_favourite: f32,
    pub lane_size_late_confirm: f32,
    pub lane_size_high_skew: f32,

    /// Regime-conditional lane gate. EX-ANTE and INERT by default. When enabled,
    /// a trailing whippy-regime estimate (mean YES-mid range over the last N
    /// completed BTC 5m markets via `ctx.prior_market_range_{1,3,7}d`, optionally
    /// blended with the live ex-ante whipsaw score) scales the directional lanes
    /// (late_favourite + late_confirm) toward `regime_gate_lane_floor` once the
    /// regime score exceeds `regime_gate_threshold`. `high_skew` is left at 1.0.
    /// Below the threshold all lanes run at full size (full br2). All inputs are
    /// available at the load decision instant: no post-fill / full-market lookahead.
    pub regime_gate_enabled: bool,
    /// Which trailing window drives the regime score: 1 => prior_market_range_1d,
    /// 3 => 3d, 7 => 7d. Any other value falls back to 3d.
    pub regime_gate_window: u8,
    /// Trailing-range threshold. When the (optionally whipsaw-blended) trailing
    /// range estimate exceeds this, the directional lanes are amputated.
    pub regime_gate_threshold: f32,
    /// Soft ramp width below the threshold. The lane multiplier interpolates
    /// linearly from 1.0 at `threshold - soft_band` down to the floor at
    /// `threshold`. 0.0 => hard step at the threshold.
    pub regime_gate_soft_band: f32,
    /// Lane multiplier applied to the directional lanes when fully whippy.
    /// 0.0 => full amputation (config H0).
    pub regime_gate_lane_floor: f32,
    /// Optional blend weight in [0,1] for the live ex-ante whipsaw score. The
    /// regime score becomes
    /// `(1-w) * trailing_range + w * whipsaw_score_as_range`, where the whipsaw
    /// score (0..1) is rescaled onto the trailing-range axis by `regime_gate_threshold`.
    /// 0.0 => trailing-range only (default).
    pub regime_gate_whipsaw_weight: f32,

    // High-skew load lane with whipsaw guards
    pub high_skew_threshold: f32,
    pub high_skew_max_ask: f32,
    pub high_skew_clip_frac: f32,
    pub high_skew_max_clips: usize,
    pub high_skew_refresh_secs: f32,
    pub high_skew_sweep_depth: usize,
    pub high_skew_min_sustain_secs: f32,
    pub high_skew_min_spot_alignment: f32,
    pub high_skew_skip_whipsaw: bool,
    pub high_skew_max_whipsaw_score: f32,
    pub high_skew_min_realized_vol_180s_bps: f32,

    // Late favourite loading: heavier than generic high-skew, but only after
    // the market has a clear favourite and book/spot direction agree.
    pub late_favourite_start_secs: f32,
    pub late_favourite_threshold: f32,
    pub late_favourite_min_ask: f32,
    pub late_favourite_high_cert_ask: f32,
    pub late_favourite_max_ask: f32,
    pub late_favourite_clip_frac: f32,
    pub late_favourite_high_cert_clip_frac: f32,
    pub late_favourite_high_cert_full_clip_edge: f32,
    /// Additional size multiplier for high-cert favourite loads with tiny
    /// model edge and weak spot path efficiency. Disabled when trigger ask is
    /// >= 1.0 or size fraction is >= 1.0.
    pub late_favourite_fragile_high_cert_ask: f32,
    pub late_favourite_fragile_high_cert_max_edge: f32,
    pub late_favourite_fragile_high_cert_max_path_efficiency: f32,
    pub late_favourite_fragile_high_cert_size_frac: f32,
    pub late_favourite_max_clips: usize,
    pub late_favourite_refresh_secs: f32,
    pub late_favourite_min_sustain_secs: f32,
    pub late_favourite_sweep_depth: usize,
    pub late_favourite_min_composite_alignment: f32,
    pub late_favourite_min_model_confidence: f32,
    pub late_favourite_min_model_direction_abs: f32,
    pub late_favourite_max_model_risk: f32,
    pub late_favourite_min_model_side_p: f32,
    pub late_favourite_min_model_edge: f32,
    pub late_favourite_high_cert_min_model_edge: f32,
    /// High-cert favourites are a par-discount trade, not a raw calibrated-p
    /// edge trade. When enabled, still require model side/confidence/risk, but
    /// do not require calibrated_p >= entry_px for asks above the high-cert
    /// threshold.
    pub late_favourite_high_cert_bypass_model_edge: bool,
    pub late_favourite_max_whipsaw_score: f32,
    pub late_favourite_max_reversal_pressure: f32,
    pub late_favourite_min_path_efficiency: f32,
    pub late_favourite_min_realized_vol_180s_bps: f32,
    pub late_favourite_max_observed_range: f32,
    /// Start scaling down late-favourite size once the live market has already
    /// traversed this much YES-mid range. Disabled when >= hard range.
    pub late_favourite_range_soft_throttle: f32,
    /// Scale late-favourite size to zero at this live-observed YES-mid range.
    pub late_favourite_range_hard_throttle: f32,
    /// Extra edge required at/above hard range; interpolated from zero at the
    /// soft range. This is independent from the hard max-observed-range gate.
    pub late_favourite_range_extra_edge: f32,
    /// Extra confidence required at/above hard range; interpolated from zero at
    /// the soft range.
    pub late_favourite_range_extra_confidence: f32,
    pub late_favourite_max_adverse_fast_momentum: f32,
    pub late_favourite_max_adverse_broad_momentum: f32,
    pub late_favourite_max_entry_pullback: f32,
    pub late_favourite_max_avg_entry_drawdown: f32,

    // Convex tail ladder. Real Bonereaper buys the losing side in multiple
    // rungs as the book moves further away from the tail side; each rung is
    // roughly USD-constant so cheaper rungs get more shares. NOTE: our
    // matcher assumes 100% taker fill at yes_ask, which is optimistic in the
    // 0.02–0.05 zone where venue depth is thin. `tail_min_ask` enforces a
    // floor below which we don't pretend we can fill.
    pub paired_tail_clip_frac: f32,
    pub tail_clip_frac: f32,
    pub tail_extreme_threshold: f32,
    pub tail_min_skew_step: f32,
    pub tail_max_clips: usize,
    pub tail_sweep_depth: usize,
    pub tail_refresh_secs: f32,
    pub tail_min_ask: f32,
    pub tail_max_ask: f32,
    /// Minimum seconds remaining in the 5m window before opening a new
    /// convex-tail rung. Avoids paying for tails with almost no time left to
    /// reverse.
    pub tail_min_seconds_to_close: f32,
    /// Minimum mark-to-market edge on the favourite sleeve before buying
    /// opposite-tail insurance. This keeps tails funded by a moved-in-favor
    /// favourite position instead of paying premium while the favourite load
    /// is already underwater.
    pub tail_min_favourite_unrealized_edge: f32,
    /// Minimum live-observed YES-mid range required before buying convex tail.
    /// Useful for avoiding steady favourite markets where tail bleed is most
    /// likely and reserving spend for reversal-prone expanded-range regimes.
    pub tail_min_observed_range: f32,
    pub tail_target_favourite_loss_coverage_frac: f32,
    /// Optional higher opposite-tail coverage target for late high-cert
    /// favourite loads in the quick-reversal danger zone.
    pub tail_reversal_coverage_frac: f32,
    pub tail_reversal_min_seconds_to_close: f32,
    pub tail_reversal_max_seconds_to_close: f32,
    pub tail_reversal_min_favourite_ask: f32,
    pub tail_budget_favourite_spend_frac: f32,
    pub tail_budget_favourite_upside_frac: f32,
    /// Optional regime-driven higher tail coverage target. This only applies
    /// after favourite exposure exists; it never opens standalone tails.
    pub tail_regime_boost_coverage_frac: f32,
    pub tail_regime_boost_budget_spend_frac: f32,
    pub tail_regime_boost_budget_upside_frac: f32,
    pub tail_regime_boost_min_whipsaw_score: f32,
    pub tail_regime_boost_min_reversal_pressure: f32,
    pub tail_regime_boost_min_realized_vol_180s_bps: f32,
    pub tail_regime_boost_max_path_efficiency: f32,
}

impl Default for BonereaperV2Config {
    fn default() -> Self {
        Self {
            bankroll_usdc: 1000.0,
            max_clip_usdc: 5.0,
            tick: 0.01,
            disable_internal_model_gates: false,
            participation_clip_frac: 0.0,
            participation_start_secs: 0.0,
            participation_stop_secs_before_close: 20.0,
            participation_max_pair_cost: 0.99,
            participation_min_price: 0.03,
            participation_max_price: 0.97,
            participation_max_inventory_delta_shares: 25.0,
            participation_repair_inventory_delta_shares: 5.0,
            participation_refresh_secs: 0.50,
            participation_max_orders_per_leg: 500,
            hedged_base_enabled: false,
            hedged_base_max_secs_in: 240.0,
            hedged_base_max_pair_cost: 0.98,
            hedged_base_min_minority_leg_frac: 0.20,
            hedged_base_clip_usdc: 5.0,
            hedged_base_max_notional_usdc: 0.0,
            late_directional_overlay_frac: 1e9,
            early_phase_end_secs: 30.0,
            mid_phase_end_secs: 240.0,
            min_composite_direction: 0.10,
            mid_ladder_min_step: 0.02,
            mid_ladder_max_rungs: 4,
            // early_dir and mid_ladder are both net-negative on PM 5m (pay
            // spread + slippage at coin-flip prices). Disabled via zero clip;
            // re-enable if a future signal stack adds enough edge to overcome
            // the spread.
            early_clip_frac: 0.00,
            mid_clip_frac: 0.00,
            late_clip_frac: 1.00,
            late_max_fires: 3,
            late_refresh_secs: 10.0,
            late_sweep_depth: 3,
            late_confirm_min_model_confidence: 0.58,
            late_confirm_max_model_risk: 0.80,
            late_confirm_min_model_side_p: 0.58,
            late_confirm_min_model_edge: 0.02,
            late_confirm_min_book_skew: 0.06,
            late_confirm_max_whipsaw_score: 0.85,
            late_confirm_min_realized_vol_180s_bps: 0.0,
            late_confirm_max_observed_range: 1.0,
            recent_regime_gate_enabled: false,
            recent_regime_gate_min_edge: 0.08,
            recent_regime_gate_late_confirm: true,
            recent_regime_gate_high_skew: true,
            recent_regime_gate_late_favourite: true,
            // Phase-3 reversal-risk score: inert by default.
            reversal_score_enabled: false,
            reversal_score_coeffs: None,
            reversal_score_cov_min: f32::NAN,
            reversal_score_cov_max: f32::NAN,
            reversal_score_size_floor: 1.0,
            reversal_score_size_ceiling: 1.0,
            lane_size_late_favourite: 1.0,
            lane_size_late_confirm: 1.0,
            lane_size_high_skew: 1.0,
            // Regime-conditional gate: inert by default (disabled => baseline).
            regime_gate_enabled: false,
            regime_gate_window: 3,
            regime_gate_threshold: 0.50,
            regime_gate_soft_band: 0.0,
            regime_gate_lane_floor: 0.0,
            regime_gate_whipsaw_weight: 0.0,
            // Favourite-loading lane. Keep this stricter than the old probe
            // defaults: the looser settings overtraded early skew and paid
            // taker spread before the market had a durable favourite.
            high_skew_threshold: 0.16, // yes_mid >= 0.66 or <= 0.34
            high_skew_max_ask: 0.95,
            high_skew_clip_frac: 0.60,
            high_skew_max_clips: 5,
            high_skew_refresh_secs: 4.0,
            high_skew_sweep_depth: 5,
            high_skew_min_sustain_secs: 12.0,
            high_skew_min_spot_alignment: 0.02,
            high_skew_skip_whipsaw: true,
            high_skew_max_whipsaw_score: 0.75,
            high_skew_min_realized_vol_180s_bps: 0.0,
            late_favourite_start_secs: 180.0,
            late_favourite_threshold: 0.22, // yes_mid >= 0.72 or <= 0.28
            late_favourite_min_ask: 0.70,
            late_favourite_high_cert_ask: 0.90,
            late_favourite_max_ask: 0.97,
            late_favourite_clip_frac: 1.00,
            late_favourite_high_cert_clip_frac: 1.00,
            late_favourite_high_cert_full_clip_edge: 0.04,
            late_favourite_fragile_high_cert_ask: 0.923,
            late_favourite_fragile_high_cert_max_edge: 0.005,
            late_favourite_fragile_high_cert_max_path_efficiency: 0.50,
            late_favourite_fragile_high_cert_size_frac: 0.50,
            late_favourite_max_clips: 12,
            late_favourite_refresh_secs: 4.0,
            late_favourite_min_sustain_secs: 0.0,
            late_favourite_sweep_depth: 7,
            late_favourite_min_composite_alignment: 0.05,
            late_favourite_min_model_confidence: 0.68,
            late_favourite_min_model_direction_abs: 0.0,
            late_favourite_max_model_risk: 0.72,
            late_favourite_min_model_side_p: 0.62,
            // Strict 5c edge makes 90c+ favourite loading impossible while
            // the model probability is capped at 94c. Still require positive
            // probability edge so expensive favourite ladders do not pay
            // near-fair prices after fees/slippage.
            late_favourite_min_model_edge: 0.03,
            late_favourite_high_cert_min_model_edge: 0.02,
            late_favourite_high_cert_bypass_model_edge: false,
            late_favourite_max_whipsaw_score: 0.75,
            late_favourite_max_reversal_pressure: 1.0,
            late_favourite_min_path_efficiency: 0.0,
            late_favourite_min_realized_vol_180s_bps: 0.0,
            // Full-history slices showed the largest drawdowns came from
            // oversized favourite loads after the market had already traversed
            // a wide YES-mid range. Scale size and require more model support
            // in that regime instead of applying a blunt whipsaw clamp.
            late_favourite_max_observed_range: 1.0,
            late_favourite_range_soft_throttle: 0.78,
            late_favourite_range_hard_throttle: 0.98,
            late_favourite_range_extra_edge: 0.03,
            late_favourite_range_extra_confidence: 0.08,
            // Disabled by default for backward-compatible sweeps. Set to a
            // small positive value (e.g. 0.04) to reject favourite loads when
            // the fast BTC impulse is actively moving against the favourite.
            late_favourite_max_adverse_fast_momentum: 1.0,
            // Disabled by default. Set to a small positive value (e.g. 0.02)
            // to reject favourite loads when the broader BTC trend stack is
            // moving against the favourite.
            late_favourite_max_adverse_broad_momentum: 1.0,
            // Disabled by default. Set to a small price distance (e.g. 0.015)
            // to stop adding to the same favourite after it rolls over from
            // the best prior entry price.
            late_favourite_max_entry_pullback: 1.0,
            // Disabled by default. Set to a small price distance (e.g. 0.01)
            // to stop adding while the current entry is below the same-side
            // average emitted late-favourite entry.
            late_favourite_max_avg_entry_drawdown: 1.0,
            // Tail ladder: cheap convex bets. Threshold raised to match the
            // skew level where a "cheap" side actually exists.
            // Paired late tails were negative in walk-forward attribution:
            // keep the standalone convex-tail ladder, disable the automatic
            // hedge emitted alongside every late confirmation.
            paired_tail_clip_frac: 0.00,
            tail_clip_frac: 0.10,
            tail_extreme_threshold: 0.30,
            tail_min_skew_step: 0.02,
            tail_max_clips: 3,
            tail_sweep_depth: 3,
            tail_refresh_secs: 5.0,
            tail_min_ask: 0.01,
            tail_max_ask: 0.10,
            tail_min_seconds_to_close: 10.0,
            tail_min_favourite_unrealized_edge: 0.0,
            tail_min_observed_range: 0.0,
            tail_target_favourite_loss_coverage_frac: 0.50,
            tail_reversal_coverage_frac: 0.0,
            tail_reversal_min_seconds_to_close: 10.0,
            tail_reversal_max_seconds_to_close: 35.0,
            tail_reversal_min_favourite_ask: 0.895,
            tail_budget_favourite_spend_frac: 0.20,
            tail_budget_favourite_upside_frac: 0.25,
            tail_regime_boost_coverage_frac: 0.0,
            tail_regime_boost_budget_spend_frac: 0.0,
            tail_regime_boost_budget_upside_frac: 0.0,
            tail_regime_boost_min_whipsaw_score: 1.0,
            tail_regime_boost_min_reversal_pressure: 1.0,
            tail_regime_boost_min_realized_vol_180s_bps: 1.0e9,
            tail_regime_boost_max_path_efficiency: -1.0,
        }
    }
}

pub struct BonereaperV2 {
    cfg: BonereaperV2Config,
    recent_mids: Ring,
    early_emitted: bool,
    ladder_side: Option<Side>,
    last_ladder_mid: f32,
    mid_rungs: usize,
    late_fires: usize,
    last_late_ns: i64,
    directional_side: Option<Side>,
    directional_shares_emitted: f64,
    directional_notional_emitted: f64,
    high_skew_clips: usize,
    last_high_skew_ns: i64,
    late_favourite_clips: usize,
    last_late_favourite_ns: i64,
    late_favourite_side: Option<Side>,
    late_favourite_shares_emitted: f64,
    late_favourite_notional_emitted: f64,
    late_favourite_peak_entry_px: f64,
    late_favourite_side_shares_emitted: f64,
    late_favourite_side_notional_emitted: f64,
    skew_high_first_ns: Option<i64>,
    skew_low_first_ns: Option<i64>,
    tail_clips: usize,
    last_tail_skew_mag: f32,
    last_tail_ns: i64,
    tail_notional_emitted: f64,
    gate_stats: BonereaperV2GateStats,
    participation_yes_emitted: usize,
    participation_no_emitted: usize,
    last_participation_yes_ns: i64,
    last_participation_no_ns: i64,
    hedged_base_notional_emitted: f32,
}

impl BonereaperV2 {
    pub fn new(cfg: BonereaperV2Config) -> Self {
        Self {
            cfg,
            recent_mids: Ring::new(MOM_WINDOW),
            early_emitted: false,
            ladder_side: None,
            last_ladder_mid: 0.0,
            mid_rungs: 0,
            late_fires: 0,
            last_late_ns: i64::MIN / 2,
            directional_side: None,
            directional_shares_emitted: 0.0,
            directional_notional_emitted: 0.0,
            high_skew_clips: 0,
            last_high_skew_ns: i64::MIN / 2,
            late_favourite_clips: 0,
            last_late_favourite_ns: i64::MIN / 2,
            late_favourite_side: None,
            late_favourite_shares_emitted: 0.0,
            late_favourite_notional_emitted: 0.0,
            late_favourite_peak_entry_px: 0.0,
            late_favourite_side_shares_emitted: 0.0,
            late_favourite_side_notional_emitted: 0.0,
            skew_high_first_ns: None,
            skew_low_first_ns: None,
            tail_clips: 0,
            last_tail_skew_mag: 0.0,
            last_tail_ns: i64::MIN / 2,
            tail_notional_emitted: 0.0,
            gate_stats: BonereaperV2GateStats::default(),
            participation_yes_emitted: 0,
            participation_no_emitted: 0,
            last_participation_yes_ns: i64::MIN / 2,
            last_participation_no_ns: i64::MIN / 2,
            hedged_base_notional_emitted: 0.0,
        }
    }

    pub fn gate_stats(&self) -> BonereaperV2GateStats {
        self.gate_stats
    }

    fn directional_side_allowed(&self, side: Side) -> bool {
        self.directional_side.is_none() || self.directional_side == Some(side)
    }

    fn mark_directional_side(&mut self, side: Side) {
        self.directional_side = Some(side);
    }

    fn record_directional_exposure(&mut self, side: Side, shares: f64, px: f64) {
        self.mark_directional_side(side);
        self.directional_shares_emitted += shares;
        self.directional_notional_emitted += shares * px;
    }

    /// True when the Phase-3 reversal-risk score machinery is active. With this
    /// false the score path is skipped and the lanes are byte-identical to
    /// baseline.
    fn reversal_score_active(&self) -> bool {
        self.cfg.reversal_score_enabled && self.cfg.reversal_score_coeffs.is_some()
    }

    /// Replay-safe reversal-risk score in [0,1] for a candidate load on `side`
    /// at `px`. Recomputes the Phase-1 Binance flow features from the same spot
    /// tape and the same `signed_flow_and_adverse` / `spot_returns_and_accel`
    /// calls the runner logs, so the score sees the same values. Returns `None`
    /// when the score is inert (disabled, no coeffs, or model output missing).
    fn reversal_score(
        &self,
        ctx: &Ctx,
        spot: &SpotHistory,
        ts_ns: i64,
        side: Side,
        px: f32,
    ) -> Option<f32> {
        if !self.reversal_score_active() {
            return None;
        }
        let coeffs = self.cfg.reversal_score_coeffs.as_ref()?;
        let model = ctx.model_output?;
        let side_p = model_side_probability(ctx, side)?;
        let is_buy_yes = matches!(side, Side::BuyYes);
        // Mirror BinanceFlowFeatures::compute in the runner exactly.
        let f5 = spot.signed_flow_and_adverse(ts_ns, 5_000_000_000, is_buy_yes);
        let f15 = spot.signed_flow_and_adverse(ts_ns, 15_000_000_000, is_buy_yes);
        let f30 = spot.signed_flow_and_adverse(ts_ns, 30_000_000_000, is_buy_yes);
        let f10 = spot.signed_flow_and_adverse(ts_ns, 10_000_000_000, is_buy_yes);
        let accel = spot.spot_returns_and_accel(ts_ns);
        let values = ReversalScoreFeatureValues {
            flow_imbal_5s: f5.imbalance,
            flow_imbal_15s: f15.imbalance,
            flow_imbal_30s: f30.imbalance,
            adverse_vol_5s: f5.adverse_volume,
            adverse_vol_15s: f15.adverse_volume,
            adverse_vol_30s: f30.adverse_volume,
            large_adverse_count_10s: f10.large_adverse_count as f64,
            trade_intensity_15s: f15.intensity,
            spot_ret_5s: accel.ret_5s,
            spot_ret_15s: accel.ret_15s,
            spot_ret_30s: accel.ret_30s,
            spot_accel_15s_vs_30s: accel.accel_15s_vs_30s,
            spot_accel_5s_vs_15s: accel.accel_5s_vs_15s,
            side_model_p: side_p as f64,
            risk_score: model.risk_score as f64,
            price: px as f64,
            side_edge_vs_fill: (side_p - px) as f64,
        };
        Some(coeffs.score(&values))
    }

    /// Convex-coverage modulator: effective base tail coverage given the
    /// favourite-side reversal score. With the master gate off or inert NaN
    /// `cov_min`/`cov_max`, returns `base` unchanged.
    fn reversal_modulated_coverage(
        &self,
        ctx: &Ctx,
        spot: &SpotHistory,
        ts_ns: i64,
        favourite_side: Side,
        favourite_px: f32,
        base: f32,
    ) -> f32 {
        if self.cfg.reversal_score_cov_min.is_nan() || self.cfg.reversal_score_cov_max.is_nan() {
            return base;
        }
        match self.reversal_score(ctx, spot, ts_ns, favourite_side, favourite_px) {
            Some(score) => lerp(
                self.cfg.reversal_score_cov_min,
                self.cfg.reversal_score_cov_max,
                score,
            ),
            None => base,
        }
    }

    /// Directional-sizing modulator: `lerp(size_floor, size_ceiling, 1-score)`.
    /// Sizes UP stable loads (low score), DOWN fragile loads (high score).
    /// Returns 1.0 (no change) when inert.
    fn reversal_size_mult(
        &self,
        ctx: &Ctx,
        spot: &SpotHistory,
        ts_ns: i64,
        side: Side,
        px: f32,
    ) -> f64 {
        match self.reversal_score(ctx, spot, ts_ns, side, px) {
            Some(score) => lerp(
                self.cfg.reversal_score_size_floor,
                self.cfg.reversal_score_size_ceiling,
                1.0 - score,
            ) as f64,
            None => 1.0,
        }
    }

    /// Regime-conditional directional-lane multiplier. Returns 1.0 when the gate
    /// is disabled (byte-identical to baseline) or when the ex-ante regime
    /// estimate is below threshold. In a whippy regime it ramps down toward
    /// `regime_gate_lane_floor`. All inputs are ex-ante: the trailing
    /// `prior_market_range_*` aggregates only already-closed markets, and
    /// `whipsaw_score` is computed from the price path up to the decision instant.
    fn regime_gate_lane_mult(&self, ctx: &Ctx, whipsaw_score: f32) -> f64 {
        if !self.cfg.regime_gate_enabled {
            return 1.0;
        }
        let trailing = match self.cfg.regime_gate_window {
            1 => ctx.prior_market_range_1d,
            7 => ctx.prior_market_range_7d,
            _ => ctx.prior_market_range_3d,
        };
        // Warmup: no prior-market history yet => stay at full size.
        if !(trailing > 0.0) {
            return 1.0;
        }
        let threshold = self.cfg.regime_gate_threshold.max(1e-6);
        let w = self.cfg.regime_gate_whipsaw_weight.clamp(0.0, 1.0);
        // Rescale the live whipsaw score (0..1) onto the trailing-range axis so
        // the two ex-ante signals are comparable, then blend.
        let whipsaw_as_range = whipsaw_score.clamp(0.0, 1.0) * threshold;
        let score = (1.0 - w) * trailing + w * whipsaw_as_range;
        let floor = self.cfg.regime_gate_lane_floor.clamp(0.0, 1.0);
        let band = self.cfg.regime_gate_soft_band.max(0.0);
        if score >= threshold {
            return floor as f64;
        }
        if band <= 0.0 || score <= threshold - band {
            return 1.0;
        }
        // Linear ramp from 1.0 at (threshold - band) down to floor at threshold.
        let frac = (score - (threshold - band)) / band;
        (1.0 - frac as f64 * (1.0 - floor as f64)).clamp(floor as f64, 1.0)
    }

    /// True when the late directional lanes are allowed to add more exposure.
    /// With the default `late_directional_overlay_frac` (1e9) this is always
    /// true and is a no-op. In hedge-first mode it caps cumulative directional
    /// notional to a small fraction of the hedged base notional, turning the
    /// late directional bet into an overlay rather than a naked load.
    fn directional_overlay_allowed(&self) -> bool {
        if !self.cfg.hedged_base_enabled {
            return true;
        }
        let cap = self.cfg.late_directional_overlay_frac as f64
            * self.hedged_base_notional_emitted as f64;
        self.directional_notional_emitted < cap
    }

    fn model_support_for_side(
        &self,
        ctx: &Ctx,
        side: Side,
        min_confidence: f32,
        max_risk: f32,
        min_side_p: f32,
        entry_px: f32,
        min_edge: f32,
    ) -> ModelSupport {
        if self.cfg.disable_internal_model_gates {
            ModelSupport::Supported
        } else {
            model_support_for_side(
                ctx,
                side,
                min_confidence,
                max_risk,
                min_side_p,
                entry_px,
                min_edge,
            )
        }
    }
}

fn shares_capped(usdc: f64, fill_px: f64) -> f64 {
    let raw = (usdc * 0.98) / fill_px;
    ((raw * 1000.0).floor() / 1000.0).max(0.0)
}

fn buy_px(event: &ReplayEvent, side: Side) -> f64 {
    match side {
        Side::BuyYes => event.yes_ask as f64,
        Side::BuyNo => (1.0 - event.yes_bid as f64).max(0.01),
        _ => 0.0,
    }
}

fn sell_px(event: &ReplayEvent, side: Side) -> f64 {
    match side {
        Side::BuyYes => event.yes_bid as f64,
        Side::BuyNo => (1.0 - event.yes_ask as f64).max(0.0),
        _ => 0.0,
    }
}

fn side_shares(ctx: &Ctx, side: Side) -> f64 {
    match side {
        Side::BuyYes => ctx.yes_shares,
        Side::BuyNo => ctx.no_shares,
        _ => 0.0,
    }
}

fn opposite_buy_side(side: Side) -> Option<Side> {
    match side {
        Side::BuyYes => Some(Side::BuyNo),
        Side::BuyNo => Some(Side::BuyYes),
        _ => None,
    }
}

fn maker_participation_native_prices(event: &ReplayEvent, tick: f64) -> Option<(f64, f64)> {
    if event.yes_bid <= 0.0
        || event.yes_ask <= 0.0
        || event.yes_bid >= event.yes_ask
        || event.yes_ask >= 1.0
    {
        return None;
    }
    let yes_px = event.yes_ask as f64 - tick;
    let no_px = (1.0 - event.yes_bid as f64) - tick;
    if yes_px.is_finite() && no_px.is_finite() && yes_px > 0.0 && no_px > 0.0 {
        Some((yes_px, no_px))
    } else {
        None
    }
}

fn late_favourite_ladder_levels(favourite_ask: f64, secs_in: f32) -> usize {
    let base = if favourite_ask < 0.75 {
        1
    } else if favourite_ask < 0.80 {
        2
    } else if favourite_ask < 0.90 {
        3
    } else {
        4
    };
    if favourite_ask >= 0.90 && secs_in >= BETTING_WINDOW_SECS as f32 - 120.0 {
        5
    } else {
        base
    }
}

fn late_favourite_high_cert_price_taper(favourite_ask: f64) -> f64 {
    if favourite_ask < 0.95 {
        return 1.0;
    }
    if favourite_ask >= 0.99 {
        return 0.18;
    }
    let progress = ((favourite_ask - 0.95) / 0.04).clamp(0.0, 1.0);
    1.0 - progress * 0.82
}

fn late_favourite_high_cert_edge_taper(edge: f32, min_edge: f32, full_clip_edge: f32) -> f64 {
    if full_clip_edge <= min_edge {
        return 1.0;
    }
    ((edge - min_edge) / (full_clip_edge - min_edge))
        .clamp(0.0, 1.0)
        .into()
}

fn late_favourite_fragile_high_cert_effective_price(favourite_ask: f64) -> f32 {
    let sweep_premium = if favourite_ask >= 0.895 { 0.025 } else { 0.0 };
    (favourite_ask as f32 + sweep_premium).clamp(0.0, 0.999)
}

fn late_favourite_fragile_high_cert_edge(side_probability: f32, favourite_ask: f64) -> f32 {
    side_probability - late_favourite_fragile_high_cert_effective_price(favourite_ask)
}

fn late_favourite_fragile_high_cert_taper(
    favourite_ask: f64,
    edge: f32,
    path_efficiency: f32,
    trigger_ask: f32,
    max_edge: f32,
    max_path_efficiency: f32,
    size_frac: f32,
) -> f64 {
    if trigger_ask >= 1.0 || size_frac >= 1.0 {
        return 1.0;
    }
    if favourite_ask as f32 >= trigger_ask
        && edge <= max_edge
        && path_efficiency <= max_path_efficiency
    {
        return size_frac.clamp(0.0, 1.0) as f64;
    }
    1.0
}

fn late_favourite_high_cert_max_levels(favourite_ask: f64, base_levels: usize) -> usize {
    if favourite_ask >= 0.99 {
        base_levels.min(1)
    } else if favourite_ask >= 0.97 {
        base_levels.min(2)
    } else if favourite_ask >= 0.95 {
        base_levels.min(3)
    } else {
        base_levels
    }
}

fn tail_observed_range_allowed(range: f32, min_range: f32) -> bool {
    range >= min_range.clamp(0.0, 1.0)
}

fn tail_seconds_to_close_allowed(seconds_to_close: f32, min_seconds_to_close: f32) -> bool {
    seconds_to_close >= min_seconds_to_close.max(0.0)
}

fn tail_favourite_unrealized_edge_allowed(
    current_bid: f64,
    avg_entry_px: f64,
    min_unrealized_edge: f32,
) -> bool {
    if !current_bid.is_finite() || !avg_entry_px.is_finite() {
        return false;
    }
    if min_unrealized_edge <= 0.0 {
        return true;
    }
    current_bid - avg_entry_px >= min_unrealized_edge as f64
}

fn range_throttle(range: f32, soft: f32, hard: f32) -> f32 {
    if hard <= soft || soft >= 1.0 {
        return 0.0;
    }
    ((range - soft) / (hard - soft)).clamp(0.0, 1.0)
}

fn late_favourite_effective_skew_threshold(
    ctx: &Ctx,
    side: Side,
    base_threshold: f32,
    floor_threshold: f32,
    min_confidence: f32,
    max_risk: f32,
    min_side_p: f32,
    disable_model_gates: bool,
) -> f32 {
    let base = base_threshold.clamp(0.0, 1.0);
    let floor = floor_threshold.clamp(0.0, base);
    if disable_model_gates || floor >= base {
        return base;
    }
    let Some(model) = ctx.model_output else {
        return base;
    };
    let model_side_is_yes = model.direction_score >= 0.0;
    let target_side_is_yes = matches!(side, Side::BuyYes);
    if model_side_is_yes != target_side_is_yes || model.risk_score > max_risk {
        return base;
    }

    let side_p = if target_side_is_yes == model_side_is_yes {
        model.calibrated_p
    } else {
        1.0 - model.calibrated_p
    };
    if side_p < min_side_p || model.confidence_score < min_confidence {
        return base;
    }

    let side_p_strength = ((side_p - min_side_p) / 0.12).clamp(0.0, 1.0);
    let confidence_strength = ((model.confidence_score - min_confidence) / 0.12).clamp(0.0, 1.0);
    let risk_strength = ((max_risk - model.risk_score) / 0.20).clamp(0.0, 1.0);
    let strength = side_p_strength.min(confidence_strength).min(risk_strength);
    (base - (base - floor) * strength).clamp(floor, base)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModelSupport {
    Supported,
    Missing,
    SideMismatch,
    LowConfidence,
    HighRisk,
    LowSideProbability,
    LowEdge,
}

impl ModelSupport {
    fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

fn model_support_for_side(
    ctx: &Ctx,
    side: Side,
    min_confidence: f32,
    max_risk: f32,
    min_side_p: f32,
    entry_px: f32,
    min_edge: f32,
) -> ModelSupport {
    let Some(model) = ctx.model_output else {
        return ModelSupport::Missing;
    };
    let model_side_is_yes = model.direction_score >= 0.0;
    let target_side_is_yes = matches!(side, Side::BuyYes);
    let side_p = if target_side_is_yes == model_side_is_yes {
        model.calibrated_p
    } else {
        1.0 - model.calibrated_p
    };
    if model_side_is_yes != target_side_is_yes {
        ModelSupport::SideMismatch
    } else if model.confidence_score < min_confidence {
        ModelSupport::LowConfidence
    } else if model.risk_score > max_risk {
        ModelSupport::HighRisk
    } else if side_p < min_side_p {
        ModelSupport::LowSideProbability
    } else if side_p - entry_px < min_edge {
        ModelSupport::LowEdge
    } else {
        ModelSupport::Supported
    }
}

fn model_side_probability(ctx: &Ctx, side: Side) -> Option<f32> {
    let model = ctx.model_output?;
    let model_side_is_yes = model.direction_score >= 0.0;
    let target_side_is_yes = matches!(side, Side::BuyYes);
    Some(if target_side_is_yes == model_side_is_yes {
        model.calibrated_p
    } else {
        1.0 - model.calibrated_p
    })
}

fn model_limited_buy_price(ctx: &Ctx, side: Side, max_ask: f32, min_edge: f32) -> f32 {
    model_side_probability(ctx, side)
        .map(|side_p| (side_p - min_edge).min(max_ask).clamp(0.0, 1.0))
        .unwrap_or(max_ask)
}

#[derive(Debug, Clone, Copy)]
enum RecentRegimeTag {
    HighSkewLoad,
    LateConfirm,
    LateFavouriteLoad,
}

const RECENT_REGIME_INTERCEPT: f64 = -0.32595430311806406;
const RECENT_REGIME_BUY_YES: f64 = 0.1510334171392331;
const RECENT_REGIME_CONFIDENCE_SCORE: f64 = 0.2067142264963719;
const RECENT_REGIME_EDGE_X_CONFIDENCE: f64 = 1.216681349179975;
const RECENT_REGIME_MARKET_YES_RANGE_SO_FAR: f64 = -2.1093908248155797;
const RECENT_REGIME_PRICE: f64 = 1.2873834443843846;
const RECENT_REGIME_PRICE_X_MODEL_P: f64 = 1.3856702016116504;
const RECENT_REGIME_PRIOR_MARKET_RANGE_1D: f64 = -1.1913807326776196;
const RECENT_REGIME_PRIOR_MARKET_RANGE_3D: f64 = 1.0991670801933349;
const RECENT_REGIME_PRIOR_MARKET_RANGE_7D: f64 = 0.8270134034720209;
const RECENT_REGIME_RANGE_X_REVERSAL: f64 = -0.009787711121205541;
const RECENT_REGIME_PATH_EFFICIENCY: f64 = 0.5047698671380214;
const RECENT_REGIME_REALIZED_VOL_180S_BPS: f64 = 0.09071979636452739;
const RECENT_REGIME_REVERSAL_PRESSURE: f64 = 0.04359966681145439;
const RECENT_REGIME_SIGN_FLIP_RATE: f64 = -1.219537378180665;
const RECENT_REGIME_WHIPSAW_SCORE: f64 = 0.2626545761670573;
const RECENT_REGIME_RISK_SCORE: f64 = -1.439136629085809;
const RECENT_REGIME_SECONDS_TO_CLOSE: f64 = -0.0018270696800363462;
const RECENT_REGIME_SIDE_EDGE_VS_FILL: f64 = -1.550855857237457;
const RECENT_REGIME_SIDE_MODEL_P: f64 = 0.9496618232112639;
const RECENT_REGIME_TAG_HIGH_SKEW_LOAD: f64 = 0.10378455544308492;
const RECENT_REGIME_TAG_LATE_CONFIRM: f64 = -0.0419135598272544;
const RECENT_REGIME_TAG_LATE_FAVOURITE_LOAD: f64 = 0.07777405236388565;
const RECENT_REGIME_WHIPSAW_X_LOW_EFFICIENCY: f64 = 0.5995117174505017;
const RECENT_REGIME_WHIPSAW_X_REVERSAL: f64 = -0.20407075062535404;

fn recent_regime_tag_coef(tag: RecentRegimeTag) -> f64 {
    match tag {
        RecentRegimeTag::HighSkewLoad => RECENT_REGIME_TAG_HIGH_SKEW_LOAD,
        RecentRegimeTag::LateConfirm => RECENT_REGIME_TAG_LATE_CONFIRM,
        RecentRegimeTag::LateFavouriteLoad => RECENT_REGIME_TAG_LATE_FAVOURITE_LOAD,
    }
}

fn sigmoid(x: f64) -> f32 {
    if x >= 0.0 {
        let z = (-x).exp();
        (1.0 / (1.0 + z)) as f32
    } else {
        let z = x.exp();
        (z / (1.0 + z)) as f32
    }
}

fn recent_regime_win_probability(
    ctx: &Ctx,
    side: Side,
    price: f32,
    seconds_to_close: f32,
    whipsaw: WhipsawRiskSnapshot,
    tag: RecentRegimeTag,
) -> Option<f32> {
    let model = ctx.model_output?;
    let side_p = model_side_probability(ctx, side)?;
    let side_edge_vs_fill = side_p - price;
    let buy_yes = matches!(side, Side::BuyYes) as u8 as f64;
    let confidence = model.confidence_score as f64;
    let risk = model.risk_score as f64;
    let market_range = ctx.market_yes_range_so_far.clamp(0.0, 1.0) as f64;
    let reversal = whipsaw.reversal_pressure as f64;
    let whipsaw_score = whipsaw.score as f64;
    let path_efficiency = whipsaw.path_efficiency as f64;

    let logit = RECENT_REGIME_INTERCEPT
        + RECENT_REGIME_PRICE * price as f64
        + RECENT_REGIME_SIDE_MODEL_P * side_p as f64
        + RECENT_REGIME_SIDE_EDGE_VS_FILL * side_edge_vs_fill as f64
        + RECENT_REGIME_CONFIDENCE_SCORE * confidence
        + RECENT_REGIME_RISK_SCORE * risk
        + RECENT_REGIME_MARKET_YES_RANGE_SO_FAR * market_range
        + RECENT_REGIME_SECONDS_TO_CLOSE * seconds_to_close as f64
        + RECENT_REGIME_WHIPSAW_SCORE * whipsaw_score
        + RECENT_REGIME_PATH_EFFICIENCY * path_efficiency
        + RECENT_REGIME_REVERSAL_PRESSURE * reversal
        + RECENT_REGIME_SIGN_FLIP_RATE * whipsaw.sign_flip_rate as f64
        + RECENT_REGIME_REALIZED_VOL_180S_BPS * whipsaw.realized_vol_180s_bps as f64
        + RECENT_REGIME_PRIOR_MARKET_RANGE_1D * ctx.prior_market_range_1d as f64
        + RECENT_REGIME_PRIOR_MARKET_RANGE_3D * ctx.prior_market_range_3d as f64
        + RECENT_REGIME_PRIOR_MARKET_RANGE_7D * ctx.prior_market_range_7d as f64
        + RECENT_REGIME_BUY_YES * buy_yes
        + recent_regime_tag_coef(tag)
        + RECENT_REGIME_WHIPSAW_X_REVERSAL * whipsaw_score * reversal
        + RECENT_REGIME_WHIPSAW_X_LOW_EFFICIENCY * whipsaw_score * (1.0 - path_efficiency)
        + RECENT_REGIME_RANGE_X_REVERSAL * market_range * reversal
        + RECENT_REGIME_PRICE_X_MODEL_P * price as f64 * side_p as f64
        + RECENT_REGIME_EDGE_X_CONFIDENCE * side_edge_vs_fill as f64 * confidence;

    Some(sigmoid(logit))
}

fn recent_regime_gate_passes(
    ctx: &Ctx,
    side: Side,
    price: f32,
    seconds_to_close: f32,
    whipsaw: WhipsawRiskSnapshot,
    tag: RecentRegimeTag,
    min_edge: f32,
) -> bool {
    recent_regime_win_probability(ctx, side, price, seconds_to_close, whipsaw, tag)
        .is_some_and(|p| p - price >= min_edge)
}

fn bump_model_fail(stats: &mut BonereaperV2GateStats, prefix: GatePrefix, support: ModelSupport) {
    match (prefix, support) {
        (_, ModelSupport::Supported) => {}
        (GatePrefix::LateConfirm, ModelSupport::Missing) => stats.late_confirm_model_missing += 1,
        (GatePrefix::LateConfirm, ModelSupport::SideMismatch) => {
            stats.late_confirm_model_side_fail += 1;
        }
        (GatePrefix::LateConfirm, ModelSupport::LowConfidence) => {
            stats.late_confirm_model_confidence_fail += 1;
        }
        (GatePrefix::LateConfirm, ModelSupport::HighRisk) => {
            stats.late_confirm_model_risk_fail += 1;
        }
        (GatePrefix::LateConfirm, ModelSupport::LowSideProbability) => {
            stats.late_confirm_model_side_p_fail += 1;
        }
        (GatePrefix::LateConfirm, ModelSupport::LowEdge) => {
            stats.late_confirm_model_edge_fail += 1;
        }
        (GatePrefix::HighSkew, ModelSupport::Missing) => stats.high_skew_model_missing += 1,
        (GatePrefix::HighSkew, ModelSupport::SideMismatch) => {
            stats.high_skew_model_side_fail += 1;
        }
        (GatePrefix::HighSkew, ModelSupport::LowConfidence) => {
            stats.high_skew_model_confidence_fail += 1;
        }
        (GatePrefix::HighSkew, ModelSupport::HighRisk) => stats.high_skew_model_risk_fail += 1,
        (GatePrefix::HighSkew, ModelSupport::LowSideProbability) => {
            stats.high_skew_model_side_p_fail += 1;
        }
        (GatePrefix::HighSkew, ModelSupport::LowEdge) => {
            stats.high_skew_model_edge_fail += 1;
        }
        (GatePrefix::LateFavourite, ModelSupport::Missing) => {
            stats.late_favourite_model_missing += 1;
        }
        (GatePrefix::LateFavourite, ModelSupport::SideMismatch) => {
            stats.late_favourite_model_side_fail += 1;
        }
        (GatePrefix::LateFavourite, ModelSupport::LowConfidence) => {
            stats.late_favourite_model_confidence_fail += 1;
        }
        (GatePrefix::LateFavourite, ModelSupport::HighRisk) => {
            stats.late_favourite_model_risk_fail += 1;
        }
        (GatePrefix::LateFavourite, ModelSupport::LowSideProbability) => {
            stats.late_favourite_model_side_p_fail += 1;
        }
        (GatePrefix::LateFavourite, ModelSupport::LowEdge) => {
            stats.late_favourite_model_edge_fail += 1;
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum GatePrefix {
    LateConfirm,
    HighSkew,
    LateFavourite,
}

fn side_is_book_favourite(event: &ReplayEvent, side: Side) -> bool {
    match side {
        Side::BuyYes => event.yes_mid >= 0.5,
        Side::BuyNo => event.yes_mid <= 0.5,
        _ => false,
    }
}

fn side_book_skew(event: &ReplayEvent, side: Side) -> f32 {
    match side {
        Side::BuyYes => event.yes_mid - 0.5,
        Side::BuyNo => 0.5 - event.yes_mid,
        _ => f32::NEG_INFINITY,
    }
}

fn buy_side_sign(side: Side) -> f32 {
    match side {
        Side::BuyYes => 1.0,
        Side::BuyNo => -1.0,
        _ => 0.0,
    }
}

fn tail_clip_notional(
    base_clip: f64,
    tail_clip_frac: f32,
    favourite_notional: f64,
    tail_notional: f64,
    tail_px: f64,
    coverage_frac: f32,
    remaining_tail_budget: f64,
) -> f64 {
    let remaining_tail_budget = remaining_tail_budget.max(0.0);
    if remaining_tail_budget <= 0.0 {
        return 0.0;
    }
    if coverage_frac > 0.0 {
        let target_tail_notional =
            favourite_notional * coverage_frac.clamp(0.0, 1.0) as f64 * tail_px.clamp(0.0, 1.0);
        return (target_tail_notional - tail_notional)
            .max(0.0)
            .min(remaining_tail_budget);
    }

    (base_clip * tail_clip_frac as f64).min(remaining_tail_budget)
}

fn tail_coverage_for_regime(
    base_coverage_frac: f32,
    avg_favourite_px: f64,
    seconds_to_close: f32,
    reversal_coverage_frac: f32,
    reversal_min_seconds_to_close: f32,
    reversal_max_seconds_to_close: f32,
    reversal_min_favourite_ask: f32,
) -> f32 {
    let in_reversal_window = reversal_coverage_frac > base_coverage_frac
        && avg_favourite_px as f32 >= reversal_min_favourite_ask
        && seconds_to_close >= reversal_min_seconds_to_close
        && seconds_to_close <= reversal_max_seconds_to_close;

    if in_reversal_window {
        reversal_coverage_frac
    } else {
        base_coverage_frac
    }
}

fn tail_regime_boost_active(
    whipsaw: WhipsawRiskSnapshot,
    min_whipsaw_score: f32,
    min_reversal_pressure: f32,
    min_realized_vol_180s_bps: f32,
    max_path_efficiency: f32,
) -> bool {
    (min_whipsaw_score < 1.0 && whipsaw.score >= min_whipsaw_score)
        || (min_reversal_pressure < 1.0 && whipsaw.reversal_pressure >= min_reversal_pressure)
        || (min_realized_vol_180s_bps.is_finite()
            && whipsaw.realized_vol_180s_bps >= min_realized_vol_180s_bps)
        || (max_path_efficiency >= 0.0 && whipsaw.path_efficiency <= max_path_efficiency)
}

fn effective_favourite_tail_exposure(
    held_favourite_shares: f64,
    emitted_favourite_shares: f64,
    emitted_favourite_notional: f64,
) -> Option<(f64, f64)> {
    if held_favourite_shares <= 0.0 || emitted_favourite_shares <= 0.0 {
        return None;
    }
    let avg_favourite_px = emitted_favourite_notional / emitted_favourite_shares;
    if !avg_favourite_px.is_finite() || avg_favourite_px <= 0.0 {
        return None;
    }
    let effective_shares = held_favourite_shares.min(emitted_favourite_shares);
    if effective_shares <= 0.0 {
        return None;
    }
    Some((effective_shares, effective_shares * avg_favourite_px))
}

impl Strategy for BonereaperV2 {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        trades: &TradeHistory,
    ) -> StrategyOutput {
        self.recent_mids.push(event.yes_mid);
        let book_dir = direction_score(event, &self.recent_mids, MICRO_DEV_SCALE);
        let (spot_mom, spot_fast_mom, spot_broad_mom) = if !spot.is_empty() {
            let stack = spot_momentum_stack(event.ts_ns, spot);
            (stack.blended_score, stack.fast_score, stack.broad_score)
        } else {
            (0.0, 0.0, 0.0)
        };
        let whipsaw = if !spot.is_empty() {
            WhipsawRiskSnapshot::from_history(event.ts_ns, spot)
        } else {
            WhipsawRiskSnapshot::default()
        };
        let trade_flow = if !trades.is_empty() {
            compute_trade_flow(event.ts_ns, TRADE_FLOW_LOOKBACK_NS, trades).flow_imbalance
        } else {
            0.0
        };
        let composite_dir =
            (0.30 * book_dir.composite + 0.55 * spot_mom + 0.15 * (-trade_flow)).clamp(-1.0, 1.0);

        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }
        let window_open_ns = ctx.market_close_ns - BETTING_WINDOW_SECS * 1_000_000_000;
        let secs_in = ((event.ts_ns - window_open_ns) as f64 / 1e9) as f32;
        if !(0.0..=BETTING_WINDOW_SECS as f32).contains(&secs_in) {
            return StrategyOutput::hold();
        }

        // Update skew sustain trackers (used by high-skew anti-spike guard).
        let skew_hi = 0.5 + self.cfg.high_skew_threshold;
        let skew_lo = 0.5 - self.cfg.high_skew_threshold;
        if event.yes_mid >= skew_hi {
            self.skew_high_first_ns.get_or_insert(event.ts_ns);
        } else {
            self.skew_high_first_ns = None;
        }
        if event.yes_mid <= skew_lo {
            self.skew_low_first_ns.get_or_insert(event.ts_ns);
        } else {
            self.skew_low_first_ns = None;
        }

        let mut orders: Vec<OrderRequest> = Vec::new();

        // ---- LANE 0: Market-neutral maker participation ----
        // This is intentionally separate from directional commitment. It posts
        // small paired bids when the implied pair cost is attractive and uses
        // inventory repair to avoid getting stranded one-sided.
        let seconds_to_close = BETTING_WINDOW_SECS as f32 - secs_in;
        if self.cfg.participation_clip_frac > 0.0
            && secs_in >= self.cfg.participation_start_secs
            && seconds_to_close > self.cfg.participation_stop_secs_before_close
        {
            if let Some((yes_px, no_px)) = maker_participation_native_prices(event, self.cfg.tick) {
                let pair_cost = yes_px + no_px;
                if pair_cost <= self.cfg.participation_max_pair_cost as f64 {
                    let delta = ctx.yes_shares - ctx.no_shares;
                    let repair_yes = delta <= -self.cfg.participation_repair_inventory_delta_shares;
                    let repair_no = delta >= self.cfg.participation_repair_inventory_delta_shares;
                    let too_long_yes = delta >= self.cfg.participation_max_inventory_delta_shares;
                    let too_long_no = -delta >= self.cfg.participation_max_inventory_delta_shares;
                    let refresh_ns =
                        (self.cfg.participation_refresh_secs.max(0.0) as f64 * 1e9) as i64;
                    let clip = self.cfg.max_clip_usdc * self.cfg.participation_clip_frac as f64;
                    let can_yes = !too_long_yes
                        && !repair_no
                        && self.participation_yes_emitted
                            < self.cfg.participation_max_orders_per_leg
                        && event.ts_ns - self.last_participation_yes_ns >= refresh_ns
                        && yes_px as f32 >= self.cfg.participation_min_price
                        && yes_px as f32 <= self.cfg.participation_max_price;
                    let can_no = !too_long_no
                        && !repair_yes
                        && self.participation_no_emitted
                            < self.cfg.participation_max_orders_per_leg
                        && event.ts_ns - self.last_participation_no_ns >= refresh_ns
                        && no_px as f32 >= self.cfg.participation_min_price
                        && no_px as f32 <= self.cfg.participation_max_price;

                    if can_yes {
                        let shares = shares_capped(clip, yes_px);
                        if shares > 0.0 {
                            orders.push(OrderRequest {
                                side: Side::BuyYes,
                                shares,
                                max_depth: 1,
                                limit_price: Some(yes_px as f32),
                                tag: "br2_participation_yes",
                            });
                            self.participation_yes_emitted += 1;
                            self.last_participation_yes_ns = event.ts_ns;
                        }
                    }
                    if can_no {
                        let shares = shares_capped(clip, no_px);
                        if shares > 0.0 {
                            orders.push(OrderRequest {
                                side: Side::BuyNo,
                                shares,
                                max_depth: 1,
                                limit_price: Some(no_px as f32),
                                tag: "br2_participation_no",
                            });
                            self.participation_no_emitted += 1;
                            self.last_participation_no_ns = event.ts_ns;
                        }
                    }
                }
            }
        }

        // ---- LANE 0b: Hedge-first arb-anchored base ----
        // Buy the YES+NO pair early as a taker when the combined cost is below
        // a locked-arb threshold. The engine pays $1 per winning share, so a
        // balanced pair bought for < $1 combined is +EV regardless of outcome.
        // This is the structural loss limiter the late directional lanes ride
        // on top of. Tagged `br2_participation_*` so it is exempt from the
        // directional model gate in the runner.
        if self.cfg.hedged_base_enabled
            && secs_in <= self.cfg.hedged_base_max_secs_in
            && (self.hedged_base_notional_emitted as f64)
                < self.cfg.hedged_base_max_notional_usdc as f64
        {
            let yes_px = buy_px(event, Side::BuyYes);
            let no_px = buy_px(event, Side::BuyNo);
            let pair_cost = yes_px + no_px;
            if yes_px > 0.0 && no_px > 0.0 && pair_cost < self.cfg.hedged_base_max_pair_cost as f64
            {
                let remaining = self.cfg.hedged_base_max_notional_usdc as f64
                    - self.hedged_base_notional_emitted as f64;
                let clip = (self.cfg.hedged_base_clip_usdc as f64)
                    .min(remaining)
                    .max(0.0);
                if clip > 0.0 {
                    // Arb lock requires EQUAL shares on both legs, not equal
                    // dollars: the engine pays $1 per winning share, so a pair
                    // of N YES and N NO shares bought for a combined per-share
                    // cost of `yes_px + no_px < 1` returns N*$1 on either
                    // outcome, beating the N*(yes_px + no_px) outlay. Size the
                    // common share count off the clip against the pricier leg
                    // so neither leg over-spends the per-add clip.
                    let pair_shares = shares_capped(clip, yes_px.max(no_px));
                    if pair_shares > 0.0 {
                        // Keep both legs balanced across ticks. If the held book
                        // has drifted so the minority leg is below
                        // `min_minority_leg_frac` of the majority leg, add only
                        // the starved leg this tick; otherwise add the full
                        // arb-locked pair.
                        let yes_book = ctx.yes_shares * yes_px;
                        let no_book = ctx.no_shares * no_px;
                        let frac = self.cfg.hedged_base_min_minority_leg_frac as f64;
                        let yes_starved = yes_book < frac * no_book;
                        let no_starved = no_book < frac * yes_book;
                        let mut emitted_notional = 0.0_f64;
                        let mut push_leg = |side: Side, px: f64, orders: &mut Vec<OrderRequest>| {
                            orders.push(OrderRequest {
                                side,
                                shares: pair_shares,
                                max_depth: 1,
                                limit_price: None,
                                tag: "br2_participation_hedged_base",
                            });
                            emitted_notional += pair_shares * px;
                        };
                        if yes_starved && !no_starved {
                            push_leg(Side::BuyYes, yes_px, &mut orders);
                        } else if no_starved && !yes_starved {
                            push_leg(Side::BuyNo, no_px, &mut orders);
                        } else {
                            push_leg(Side::BuyYes, yes_px, &mut orders);
                            push_leg(Side::BuyNo, no_px, &mut orders);
                        }
                        self.hedged_base_notional_emitted += emitted_notional as f32;
                    }
                }
            }
        }

        // ---- LANE 1: Early directional probe ----
        if !self.early_emitted
            && secs_in <= self.cfg.early_phase_end_secs
            && composite_dir.abs() >= self.cfg.min_composite_direction
        {
            let side = if composite_dir > 0.0 {
                Side::BuyYes
            } else {
                Side::BuyNo
            };
            let px = buy_px(event, side);
            if px > 0.0 {
                let clip = self.cfg.max_clip_usdc * self.cfg.early_clip_frac as f64;
                let shares = shares_capped(clip, px);
                if shares > 0.0 {
                    self.early_emitted = true;
                    orders.push(OrderRequest {
                        side,
                        shares,
                        max_depth: 1,
                        limit_price: None,
                        tag: "br2_early_dir",
                    });
                    self.ladder_side = Some(side);
                    self.last_ladder_mid = event.yes_mid;
                }
            }
        }

        // ---- LANE 2: Mid-ladder ----
        if secs_in > self.cfg.early_phase_end_secs
            && secs_in <= self.cfg.mid_phase_end_secs
            && self.mid_rungs < self.cfg.mid_ladder_max_rungs
            && composite_dir.abs() >= self.cfg.min_composite_direction
        {
            let target = if composite_dir > 0.0 {
                Side::BuyYes
            } else {
                Side::BuyNo
            };
            let same_side = self.ladder_side == Some(target);
            let book_moved = match target {
                Side::BuyYes => {
                    event.yes_mid - self.last_ladder_mid >= self.cfg.mid_ladder_min_step
                }
                Side::BuyNo => self.last_ladder_mid - event.yes_mid >= self.cfg.mid_ladder_min_step,
                _ => false,
            };
            if self.ladder_side.is_none() || (same_side && book_moved) {
                let px = buy_px(event, target);
                if px > 0.0 {
                    let clip = self.cfg.max_clip_usdc * self.cfg.mid_clip_frac as f64;
                    let shares = shares_capped(clip, px);
                    if shares > 0.0 {
                        orders.push(OrderRequest {
                            side: target,
                            shares,
                            max_depth: 1,
                            limit_price: None,
                            tag: "br2_mid_ladder",
                        });
                        self.ladder_side = Some(target);
                        self.last_ladder_mid = event.yes_mid;
                        self.mid_rungs += 1;
                    }
                }
            }
        }

        // ---- LANE 3: Late directional (workhorse) + paired tail hedge ----
        // Each late_confirm fire automatically emits a paired cheap-side tail
        // bet — convex hedge against the directional bet getting whipsawed.
        if secs_in > self.cfg.mid_phase_end_secs
            && self.late_fires < self.cfg.late_max_fires
            && composite_dir.abs() >= self.cfg.min_composite_direction
            && (event.ts_ns - self.last_late_ns) as f64 / 1e9 > self.cfg.late_refresh_secs as f64
            && self.directional_overlay_allowed()
        {
            self.gate_stats.late_confirm_checks += 1;
            let target = if composite_dir > 0.0 {
                Side::BuyYes
            } else {
                Side::BuyNo
            };
            let px = buy_px(event, target);
            if !self.directional_side_allowed(target) {
                self.gate_stats.late_confirm_side_lock_fail += 1;
            } else if px <= 0.0 {
                self.gate_stats.late_confirm_price_fail += 1;
            } else if !side_is_book_favourite(event, target) {
                self.gate_stats.late_confirm_book_favourite_fail += 1;
            } else if side_book_skew(event, target) < self.cfg.late_confirm_min_book_skew {
                self.gate_stats.late_confirm_book_skew_fail += 1;
            } else if whipsaw.score > self.cfg.late_confirm_max_whipsaw_score {
                self.gate_stats.late_confirm_whipsaw_fail += 1;
            } else if whipsaw.realized_vol_180s_bps
                < self.cfg.late_confirm_min_realized_vol_180s_bps
            {
                self.gate_stats.late_confirm_low_vol_fail += 1;
            } else if ctx.market_yes_range_so_far > self.cfg.late_confirm_max_observed_range {
                self.gate_stats.late_confirm_market_range_fail += 1;
            } else {
                let model_support = self.model_support_for_side(
                    ctx,
                    target,
                    self.cfg.late_confirm_min_model_confidence,
                    self.cfg.late_confirm_max_model_risk,
                    self.cfg.late_confirm_min_model_side_p,
                    px as f32,
                    self.cfg.late_confirm_min_model_edge,
                );
                if !model_support.is_supported() {
                    bump_model_fail(&mut self.gate_stats, GatePrefix::LateConfirm, model_support);
                } else if self.cfg.recent_regime_gate_enabled
                    && self.cfg.recent_regime_gate_late_confirm
                    && !recent_regime_gate_passes(
                        ctx,
                        target,
                        px as f32,
                        seconds_to_close,
                        whipsaw,
                        RecentRegimeTag::LateConfirm,
                        self.cfg.recent_regime_gate_min_edge,
                    )
                {
                    self.gate_stats.late_confirm_recent_regime_fail += 1;
                } else {
                    let clip = self.cfg.max_clip_usdc
                        * self.cfg.late_clip_frac as f64
                        * self.cfg.lane_size_late_confirm as f64
                        * self.regime_gate_lane_mult(ctx, whipsaw.score);
                    let reversal_size_mult =
                        self.reversal_size_mult(ctx, spot, event.ts_ns, target, px as f32);
                    let shares = shares_capped(clip * reversal_size_mult, px);
                    if shares > 0.0 {
                        orders.push(OrderRequest {
                            side: target,
                            shares,
                            max_depth: self.cfg.late_sweep_depth,
                            limit_price: Some(model_limited_buy_price(
                                ctx,
                                target,
                                1.0,
                                self.cfg.late_confirm_min_model_edge,
                            )),
                            tag: "br2_late_confirm",
                        });
                        self.late_fires += 1;
                        self.last_late_ns = event.ts_ns;
                        self.record_directional_exposure(target, shares, px);
                        self.gate_stats.late_confirm_emits += 1;

                        // Paired tail hedge: cheap-side bet, sized as a fraction of
                        // the directional clip. Skips its own ladder gates because
                        // it's an explicit hedge, but respects min/max ask floor.
                        if self.cfg.paired_tail_clip_frac > 0.0 {
                            let tail_side = match target {
                                Side::BuyYes => Side::BuyNo,
                                Side::BuyNo => Side::BuyYes,
                                _ => unreachable!(),
                            };
                            let seconds_to_close = BETTING_WINDOW_SECS as f32 - secs_in;
                            let tail_px = buy_px(event, tail_side);
                            let tail_px32 = tail_px as f32;
                            if tail_px32 >= self.cfg.tail_min_ask
                                && tail_px32 <= self.cfg.tail_max_ask
                                && tail_seconds_to_close_allowed(
                                    seconds_to_close,
                                    self.cfg.tail_min_seconds_to_close,
                                )
                            {
                                let tail_clip =
                                    self.cfg.max_clip_usdc * self.cfg.paired_tail_clip_frac as f64;
                                let tail_shares = shares_capped(tail_clip, tail_px);
                                if tail_shares > 0.0 {
                                    orders.push(OrderRequest {
                                        side: tail_side,
                                        shares: tail_shares,
                                        max_depth: 1,
                                        limit_price: Some(self.cfg.tail_max_ask),
                                        tag: "br2_paired_tail",
                                    });
                                }
                            }
                        }
                    } else {
                        self.gate_stats.late_confirm_shares_fail += 1;
                    }
                }
            }
        }

        // ---- LANE 4: High-skew load with whipsaw guards ----
        if secs_in >= self.cfg.late_favourite_start_secs
            && self.high_skew_clips < self.cfg.high_skew_max_clips
            && (event.ts_ns - self.last_high_skew_ns) as f64 / 1e9
                > self.cfg.high_skew_refresh_secs as f64
            && self.directional_overlay_allowed()
        {
            self.gate_stats.high_skew_checks += 1;
            let regime_ok = if self.cfg.high_skew_skip_whipsaw {
                let snap = BtcRegimeSnapshot::from_history(event.ts_ns, spot);
                !matches!(snap.regime(), Some(BtcRegime::Whipsaw))
            } else {
                true
            };
            let skew_signed = event.yes_mid - 0.5;
            let skew_mag = skew_signed.abs();
            if !regime_ok || whipsaw.score > self.cfg.high_skew_max_whipsaw_score {
                self.gate_stats.high_skew_regime_fail += 1;
                if whipsaw.score > self.cfg.high_skew_max_whipsaw_score {
                    self.gate_stats.high_skew_whipsaw_fail += 1;
                }
            } else if whipsaw.realized_vol_180s_bps < self.cfg.high_skew_min_realized_vol_180s_bps {
                self.gate_stats.high_skew_low_vol_fail += 1;
            } else if skew_mag < self.cfg.high_skew_threshold {
                self.gate_stats.high_skew_threshold_fail += 1;
            } else {
                let first_ns = if skew_signed > 0.0 {
                    self.skew_high_first_ns
                } else {
                    self.skew_low_first_ns
                };
                let sustained = first_ns
                    .map(|t0| {
                        (event.ts_ns - t0) as f64 / 1e9
                            >= self.cfg.high_skew_min_sustain_secs as f64
                    })
                    .unwrap_or(false);
                let spot_aligned = spot_fast_mom.signum() == skew_signed.signum()
                    && spot_fast_mom.abs() >= self.cfg.high_skew_min_spot_alignment;
                if !sustained {
                    self.gate_stats.high_skew_sustain_fail += 1;
                } else if !spot_aligned {
                    self.gate_stats.high_skew_spot_alignment_fail += 1;
                } else {
                    let side = if skew_signed > 0.0 {
                        Side::BuyYes
                    } else {
                        Side::BuyNo
                    };
                    let px = buy_px(event, side);
                    if !self.directional_side_allowed(side) {
                        self.gate_stats.high_skew_side_lock_fail += 1;
                    } else if px <= 0.0 || px as f32 > self.cfg.high_skew_max_ask {
                        self.gate_stats.high_skew_price_fail += 1;
                    } else {
                        let model_support = self.model_support_for_side(
                            ctx,
                            side,
                            self.cfg.late_favourite_min_model_confidence,
                            self.cfg.late_favourite_max_model_risk,
                            self.cfg.late_favourite_min_model_side_p,
                            px as f32,
                            self.cfg.late_favourite_min_model_edge,
                        );
                        if !model_support.is_supported() {
                            bump_model_fail(
                                &mut self.gate_stats,
                                GatePrefix::HighSkew,
                                model_support,
                            );
                        } else if self.cfg.recent_regime_gate_enabled
                            && self.cfg.recent_regime_gate_high_skew
                            && !recent_regime_gate_passes(
                                ctx,
                                side,
                                px as f32,
                                seconds_to_close,
                                whipsaw,
                                RecentRegimeTag::HighSkewLoad,
                                self.cfg.recent_regime_gate_min_edge,
                            )
                        {
                            self.gate_stats.high_skew_recent_regime_fail += 1;
                        } else {
                            let clip = self.cfg.max_clip_usdc
                                * self.cfg.high_skew_clip_frac as f64
                                * self.cfg.lane_size_high_skew as f64;
                            // Dynamic risk sizing (same as late favourite lane)
                            let risk_size_mult = ctx
                                .model_output
                                .map(|m| (1.0 - m.risk_score as f64).clamp(0.25, 1.0))
                                .unwrap_or(1.0);
                            let reversal_size_mult =
                                self.reversal_size_mult(ctx, spot, event.ts_ns, side, px as f32);
                            let shares =
                                shares_capped(clip * risk_size_mult * reversal_size_mult, px);
                            if shares > 0.0 {
                                orders.push(OrderRequest {
                                    side,
                                    shares,
                                    max_depth: self.cfg.high_skew_sweep_depth,
                                    limit_price: Some(model_limited_buy_price(
                                        ctx,
                                        side,
                                        self.cfg.high_skew_max_ask,
                                        self.cfg.late_favourite_min_model_edge,
                                    )),
                                    tag: "br2_high_skew_load",
                                });
                                self.high_skew_clips += 1;
                                self.last_high_skew_ns = event.ts_ns;
                                self.record_directional_exposure(side, shares, px);
                                self.gate_stats.high_skew_emits += 1;
                            } else {
                                self.gate_stats.high_skew_shares_fail += 1;
                            }
                        }
                    }
                }
            }
        }

        // ---- LANE 5: Late favourite load ----
        if secs_in >= self.cfg.late_favourite_start_secs && self.directional_overlay_allowed() {
            self.gate_stats.late_favourite_window_checks += 1;
            let refreshed = (event.ts_ns - self.last_late_favourite_ns) as f64 / 1e9
                > self.cfg.late_favourite_refresh_secs as f64;

            if self.late_favourite_clips >= self.cfg.late_favourite_max_clips {
                self.gate_stats.late_favourite_capacity_fail += 1;
            } else if !refreshed {
                self.gate_stats.late_favourite_refresh_fail += 1;
            } else {
                self.gate_stats.late_favourite_checks += 1;
                let skew_signed = event.yes_mid - 0.5;
                let skew_mag = skew_signed.abs();
                let side = if skew_signed > 0.0 {
                    Side::BuyYes
                } else {
                    Side::BuyNo
                };
                let effective_skew_threshold = late_favourite_effective_skew_threshold(
                    ctx,
                    side,
                    self.cfg.late_favourite_threshold,
                    self.cfg.high_skew_threshold,
                    self.cfg.late_favourite_min_model_confidence,
                    self.cfg.late_favourite_max_model_risk,
                    self.cfg.late_favourite_min_model_side_p,
                    self.cfg.disable_internal_model_gates,
                );
                let composite_aligned = composite_dir.signum() == skew_signed.signum()
                    && composite_dir.abs() >= self.cfg.late_favourite_min_composite_alignment;
                let spot_aligned = spot_fast_mom.signum() == skew_signed.signum()
                    && spot_fast_mom.abs() >= self.cfg.high_skew_min_spot_alignment;
                let favourite_sustained = if self.cfg.late_favourite_min_sustain_secs > 0.0 {
                    let first_ns = if skew_signed > 0.0 {
                        self.skew_high_first_ns
                    } else {
                        self.skew_low_first_ns
                    };
                    first_ns
                        .map(|t0| {
                            (event.ts_ns - t0) as f64 / 1e9
                                >= self.cfg.late_favourite_min_sustain_secs as f64
                        })
                        .unwrap_or(false)
                } else {
                    true
                };

                if skew_mag < effective_skew_threshold {
                    self.gate_stats.late_favourite_skew_fail += 1;
                } else if !self.directional_side_allowed(side) {
                    self.gate_stats.late_favourite_side_lock_fail += 1;
                } else if !favourite_sustained {
                    self.gate_stats.late_favourite_sustain_fail += 1;
                } else if whipsaw.score > self.cfg.late_favourite_max_whipsaw_score {
                    self.gate_stats.late_favourite_whipsaw_fail += 1;
                } else if whipsaw.reversal_pressure > self.cfg.late_favourite_max_reversal_pressure
                {
                    self.gate_stats.late_favourite_reversal_pressure_fail += 1;
                } else if whipsaw.path_efficiency < self.cfg.late_favourite_min_path_efficiency {
                    self.gate_stats.late_favourite_path_efficiency_fail += 1;
                } else if whipsaw.realized_vol_180s_bps
                    < self.cfg.late_favourite_min_realized_vol_180s_bps
                {
                    self.gate_stats.late_favourite_low_vol_fail += 1;
                } else if ctx.market_yes_range_so_far > self.cfg.late_favourite_max_observed_range {
                    self.gate_stats.late_favourite_market_range_fail += 1;
                } else {
                    let adverse_fast_momentum = buy_side_sign(side) * spot_fast_mom
                        < -self.cfg.late_favourite_max_adverse_fast_momentum;
                    let adverse_broad_momentum = buy_side_sign(side) * spot_broad_mom
                        < -self.cfg.late_favourite_max_adverse_broad_momentum;
                    let px = buy_px(event, side);
                    let entry_pullback = if self.cfg.late_favourite_max_entry_pullback < 1.0
                        && self.late_favourite_side == Some(side)
                        && self.late_favourite_peak_entry_px > 0.0
                    {
                        self.late_favourite_peak_entry_px - px
                    } else {
                        0.0
                    };
                    let avg_entry_drawdown = if self.cfg.late_favourite_max_avg_entry_drawdown < 1.0
                        && self.late_favourite_side == Some(side)
                        && self.late_favourite_side_shares_emitted > 0.0
                    {
                        (self.late_favourite_side_notional_emitted
                            / self.late_favourite_side_shares_emitted)
                            - px
                    } else {
                        0.0
                    };
                    let high_cert_favourite = px as f32 >= self.cfg.late_favourite_high_cert_ask;
                    if px <= 0.0
                        || px as f32 > self.cfg.late_favourite_max_ask
                        || (px as f32) < self.cfg.late_favourite_min_ask
                    {
                        self.gate_stats.late_favourite_price_fail += 1;
                    } else if adverse_fast_momentum || adverse_broad_momentum {
                        self.gate_stats.late_favourite_adverse_momentum_fail += 1;
                    } else if entry_pullback > self.cfg.late_favourite_max_entry_pullback as f64 {
                        self.gate_stats.late_favourite_entry_pullback_fail += 1;
                    } else if avg_entry_drawdown
                        > self.cfg.late_favourite_max_avg_entry_drawdown as f64
                    {
                        self.gate_stats.late_favourite_avg_entry_drawdown_fail += 1;
                    } else if !high_cert_favourite && !(composite_aligned || spot_aligned) {
                        self.gate_stats.late_favourite_alignment_fail += 1;
                    } else if self.cfg.late_favourite_min_model_direction_abs > 0.0
                        && ctx.model_output.is_some_and(|model| {
                            model.direction_score.abs()
                                < self.cfg.late_favourite_min_model_direction_abs
                        })
                    {
                        self.gate_stats.late_favourite_model_direction_fail += 1;
                    } else {
                        let min_model_edge = if high_cert_favourite {
                            self.cfg.late_favourite_high_cert_min_model_edge
                        } else {
                            self.cfg.late_favourite_min_model_edge
                        };
                        let range_throttle = range_throttle(
                            ctx.market_yes_range_so_far,
                            self.cfg.late_favourite_range_soft_throttle,
                            self.cfg.late_favourite_range_hard_throttle,
                        );
                        let throttled_min_confidence = self.cfg.late_favourite_min_model_confidence
                            + self.cfg.late_favourite_range_extra_confidence * range_throttle;
                        let throttled_min_edge = min_model_edge
                            + self.cfg.late_favourite_range_extra_edge * range_throttle;
                        let bypass_high_cert_edge = high_cert_favourite
                            && self.cfg.late_favourite_high_cert_bypass_model_edge;
                        let model_support = self.model_support_for_side(
                            ctx,
                            side,
                            throttled_min_confidence,
                            self.cfg.late_favourite_max_model_risk,
                            self.cfg.late_favourite_min_model_side_p,
                            px as f32,
                            if bypass_high_cert_edge {
                                -1.0
                            } else {
                                throttled_min_edge
                            },
                        );
                        if !model_support.is_supported() {
                            bump_model_fail(
                                &mut self.gate_stats,
                                GatePrefix::LateFavourite,
                                model_support,
                            );
                        } else if self.cfg.recent_regime_gate_enabled
                            && self.cfg.recent_regime_gate_late_favourite
                            && !recent_regime_gate_passes(
                                ctx,
                                side,
                                px as f32,
                                seconds_to_close,
                                whipsaw,
                                RecentRegimeTag::LateFavouriteLoad,
                                self.cfg.recent_regime_gate_min_edge,
                            )
                        {
                            self.gate_stats.late_favourite_recent_regime_fail += 1;
                        } else {
                            let remaining_clips = self
                                .cfg
                                .late_favourite_max_clips
                                .saturating_sub(self.late_favourite_clips);
                            let base_levels = late_favourite_ladder_levels(px, secs_in);
                            let mut levels = late_favourite_high_cert_max_levels(px, base_levels)
                                .min(self.cfg.late_favourite_sweep_depth.max(1))
                                .min(remaining_clips.max(1));
                            let price_taper = late_favourite_high_cert_price_taper(px);
                            let clip_frac = if high_cert_favourite {
                                self.cfg.late_favourite_high_cert_clip_frac
                            } else {
                                self.cfg.late_favourite_clip_frac
                            };
                            let edge_taper = if high_cert_favourite {
                                if bypass_high_cert_edge {
                                    1.0
                                } else {
                                    model_side_probability(ctx, side)
                                        .map(|side_p| {
                                            late_favourite_high_cert_edge_taper(
                                                side_p - px as f32,
                                                throttled_min_edge,
                                                self.cfg.late_favourite_high_cert_full_clip_edge,
                                            )
                                        })
                                        .unwrap_or(0.0)
                                }
                            } else {
                                1.0
                            };
                            let fragile_edge = model_side_probability(ctx, side)
                                .map(|side_p| late_favourite_fragile_high_cert_edge(side_p, px))
                                .unwrap_or(0.0);
                            let fragile_effective_price =
                                late_favourite_fragile_high_cert_effective_price(px);
                            let fragile_taper = late_favourite_fragile_high_cert_taper(
                                fragile_effective_price as f64,
                                fragile_edge,
                                whipsaw.path_efficiency,
                                self.cfg.late_favourite_fragile_high_cert_ask,
                                self.cfg.late_favourite_fragile_high_cert_max_edge,
                                self.cfg
                                    .late_favourite_fragile_high_cert_max_path_efficiency,
                                self.cfg.late_favourite_fragile_high_cert_size_frac,
                            );
                            if fragile_taper < 1.0 {
                                levels = levels.min(1);
                            }
                            let clip = self.cfg.max_clip_usdc
                                * clip_frac as f64
                                * self.cfg.lane_size_late_favourite as f64
                                * self.regime_gate_lane_mult(ctx, whipsaw.score);
                            let range_size_taper = (1.0 - range_throttle as f64).clamp(0.0, 1.0);
                            let desired_notional = clip
                                * levels as f64
                                * price_taper
                                * edge_taper
                                * range_size_taper
                                * fragile_taper;

                            // Dynamic risk-based sizing: higher model risk_score (now includes BTC regime risk)
                            // reduces clip size. This turns the new regime features into proportional sizing
                            // instead of only hard gates.
                            let risk_size_mult = ctx
                                .model_output
                                .map(|m| (1.0 - m.risk_score as f64).clamp(0.25, 1.0))
                                .unwrap_or(1.0);

                            let reversal_size_mult =
                                self.reversal_size_mult(ctx, spot, event.ts_ns, side, px as f32);
                            let desired_notional =
                                desired_notional * risk_size_mult * reversal_size_mult;
                            let shares = shares_capped(desired_notional, px);
                            if shares > 0.0 {
                                orders.push(OrderRequest {
                                    side,
                                    shares,
                                    max_depth: levels,
                                    limit_price: Some(if bypass_high_cert_edge {
                                        self.cfg.late_favourite_max_ask
                                    } else {
                                        model_limited_buy_price(
                                            ctx,
                                            side,
                                            self.cfg.late_favourite_max_ask,
                                            throttled_min_edge,
                                        )
                                    }),
                                    tag: "br2_late_favourite_load",
                                });
                                self.late_favourite_clips += levels;
                                self.last_late_favourite_ns = event.ts_ns;
                                self.record_directional_exposure(side, shares, px);
                                if self.late_favourite_side != Some(side) {
                                    self.late_favourite_peak_entry_px = px;
                                    self.late_favourite_side_shares_emitted = shares;
                                    self.late_favourite_side_notional_emitted = shares * px;
                                } else {
                                    self.late_favourite_peak_entry_px =
                                        self.late_favourite_peak_entry_px.max(px);
                                    self.late_favourite_side_shares_emitted += shares;
                                    self.late_favourite_side_notional_emitted += shares * px;
                                }
                                self.late_favourite_side = Some(side);
                                self.late_favourite_shares_emitted += shares;
                                self.late_favourite_notional_emitted += shares * px;
                                self.gate_stats.late_favourite_emits += 1;
                            } else {
                                self.gate_stats.late_favourite_shares_fail += 1;
                            }
                        }
                    }
                }
            }
        }

        // ---- LANE 6: Cheap-tail ladder anchored to favourite-side exposure ----
        // This is not a standalone long-shot strategy. It only spends a small
        // fraction of existing directional favourite spend/upside.
        if self.tail_clips < self.cfg.tail_max_clips
            && (event.ts_ns - self.last_tail_ns) as f64 / 1e9 > self.cfg.tail_refresh_secs as f64
        {
            let skew_mag = (event.yes_mid - 0.5).abs();
            let seconds_to_close = BETTING_WINDOW_SECS as f32 - secs_in;
            let starting_fresh = self.tail_clips == 0;
            let advanced = skew_mag - self.last_tail_skew_mag >= self.cfg.tail_min_skew_step;
            if skew_mag >= self.cfg.tail_extreme_threshold
                && (starting_fresh || advanced)
                && self.directional_notional_emitted > 0.0
                && tail_observed_range_allowed(
                    ctx.market_yes_range_so_far,
                    self.cfg.tail_min_observed_range,
                )
                && tail_seconds_to_close_allowed(
                    seconds_to_close,
                    self.cfg.tail_min_seconds_to_close,
                )
            {
                let Some(favourite_side) = self.directional_side else {
                    return StrategyOutput { orders };
                };
                let Some(tail_side) = opposite_buy_side(favourite_side) else {
                    return StrategyOutput { orders };
                };
                let Some((effective_favourite_shares, effective_favourite_notional)) =
                    effective_favourite_tail_exposure(
                        side_shares(ctx, favourite_side),
                        self.directional_shares_emitted,
                        self.directional_notional_emitted,
                    )
                else {
                    return StrategyOutput { orders };
                };
                let tail_px = buy_px(event, tail_side);
                let px32 = tail_px as f32;
                if px32 >= self.cfg.tail_min_ask && px32 <= self.cfg.tail_max_ask {
                    let avg_favourite_px =
                        self.directional_notional_emitted / self.directional_shares_emitted;
                    let favourite_bid = sell_px(event, favourite_side);
                    if tail_favourite_unrealized_edge_allowed(
                        favourite_bid,
                        avg_favourite_px,
                        self.cfg.tail_min_favourite_unrealized_edge,
                    ) {
                        let regime_boost = tail_regime_boost_active(
                            whipsaw,
                            self.cfg.tail_regime_boost_min_whipsaw_score,
                            self.cfg.tail_regime_boost_min_reversal_pressure,
                            self.cfg.tail_regime_boost_min_realized_vol_180s_bps,
                            self.cfg.tail_regime_boost_max_path_efficiency,
                        );
                        let favourite_win_upside =
                            effective_favourite_shares * (1.0 - avg_favourite_px);
                        let tail_budget_favourite_spend_frac = if regime_boost {
                            self.cfg
                                .tail_budget_favourite_spend_frac
                                .max(self.cfg.tail_regime_boost_budget_spend_frac)
                        } else {
                            self.cfg.tail_budget_favourite_spend_frac
                        };
                        let tail_budget_favourite_upside_frac = if regime_boost {
                            self.cfg
                                .tail_budget_favourite_upside_frac
                                .max(self.cfg.tail_regime_boost_budget_upside_frac)
                        } else {
                            self.cfg.tail_budget_favourite_upside_frac
                        };
                        let cap_by_fav_spend =
                            effective_favourite_notional * tail_budget_favourite_spend_frac as f64;
                        let cap_by_win_upside =
                            favourite_win_upside * tail_budget_favourite_upside_frac as f64;
                        let remaining_tail_budget =
                            cap_by_fav_spend.min(cap_by_win_upside) - self.tail_notional_emitted;
                        let effective_base_coverage = self.reversal_modulated_coverage(
                            ctx,
                            spot,
                            event.ts_ns,
                            favourite_side,
                            buy_px(event, favourite_side) as f32,
                            self.cfg.tail_target_favourite_loss_coverage_frac,
                        );
                        let mut coverage_frac = tail_coverage_for_regime(
                            effective_base_coverage,
                            avg_favourite_px,
                            seconds_to_close,
                            self.cfg.tail_reversal_coverage_frac,
                            self.cfg.tail_reversal_min_seconds_to_close,
                            self.cfg.tail_reversal_max_seconds_to_close,
                            self.cfg.tail_reversal_min_favourite_ask,
                        );
                        if regime_boost {
                            coverage_frac =
                                coverage_frac.max(self.cfg.tail_regime_boost_coverage_frac);
                        }
                        let clip = tail_clip_notional(
                            self.cfg.max_clip_usdc,
                            self.cfg.tail_clip_frac,
                            effective_favourite_notional,
                            self.tail_notional_emitted,
                            tail_px,
                            coverage_frac,
                            remaining_tail_budget,
                        );
                        let shares = if clip > 0.0 {
                            shares_capped(clip, tail_px)
                        } else {
                            0.0
                        };
                        if shares > 0.0 {
                            orders.push(OrderRequest {
                                side: tail_side,
                                shares,
                                max_depth: self.cfg.tail_sweep_depth.max(1),
                                limit_price: Some(self.cfg.tail_max_ask),
                                tag: "br2_convex_tail",
                            });
                            self.tail_clips += 1;
                            self.tail_notional_emitted += shares * tail_px;
                            self.last_tail_skew_mag = skew_mag;
                            self.last_tail_ns = event.ts_ns;
                        }
                    }
                }
            }
        }

        StrategyOutput { orders }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_model::ModelOutput;
    use pm_types::{BookLevel, MarketId, ReplayFlags, SpotTick};

    fn test_event(ts_ns: i64, yes_mid: f32, yes_bid: f32, yes_ask: f32) -> ReplayEvent {
        ReplayEvent {
            ts_ns,
            market_id: MarketId(1),
            yes_mid,
            yes_bid,
            yes_ask,
            volume: 0.0,
            bids: [BookLevel::default(); pm_types::TAPE_DEPTH],
            asks: [BookLevel::default(); pm_types::TAPE_DEPTH],
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    fn test_ctx(ts_close_ns: i64, direction_score: f32, calibrated_p: f32) -> Ctx {
        Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 1000.0,
            market_yes_range_so_far: 0.0,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: Some(ModelOutput {
                direction_score,
                confidence_score: 0.95,
                calibrated_p,
                risk_score: 0.10,
            }),
            model_attribution: None,
            market_close_ns: ts_close_ns,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
            ..Ctx::default()
        }
    }

    fn test_spot_uptrend(now_ns: i64) -> SpotHistory {
        SpotHistory::new(vec![
            SpotTick {
                ts_ns: now_ns - 120_000_000_000,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns - 60_000_000_000,
                price: 103.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 106.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
        ])
    }

    fn reversal_coeffs_intercept_only(intercept: f64) -> ReversalScoreCoeffs {
        ReversalScoreCoeffs {
            intercept,
            terms: Vec::new(),
        }
    }

    #[test]
    fn reversal_score_matches_standardized_logistic() {
        let coeffs = ReversalScoreCoeffs {
            intercept: 0.5,
            terms: vec![
                ReversalScoreTerm {
                    feature: ReversalScoreFeature::SpotRet5s,
                    mean: 1.0,
                    std: 2.0,
                    coef: 1.5,
                },
                ReversalScoreTerm {
                    feature: ReversalScoreFeature::RiskScore,
                    mean: 0.2,
                    std: 0.1,
                    coef: -0.8,
                },
            ],
        };
        let values = ReversalScoreFeatureValues {
            spot_ret_5s: 3.0,
            risk_score: 0.5,
            ..ReversalScoreFeatureValues::default()
        };
        // logit = 0.5 + 1.5*(3-1)/2 + (-0.8)*(0.5-0.2)/0.1 = 0.5 + 1.5 - 2.4 = -0.4
        let expected = 1.0 / (1.0 + (0.4f64).exp());
        let got = coeffs.score(&values) as f64;
        assert!(
            (got - expected).abs() < 1e-6,
            "got={got} expected={expected}"
        );
    }

    #[test]
    fn reversal_score_inert_orders_byte_identical_to_baseline() {
        let close_ns = 300_000_000_000;
        let event = test_event(250_000_000_000, 0.90, 0.89, 0.91);
        let spot = test_spot_uptrend(250_000_000_000);

        let base_cfg = || BonereaperV2Config {
            max_clip_usdc: 50.0,
            late_clip_frac: 1.0,
            late_max_fires: 1,
            late_confirm_min_model_edge: 0.03,
            late_confirm_min_model_confidence: 0.80,
            late_confirm_min_model_side_p: 0.80,
            late_confirm_min_book_skew: 0.05,
            late_confirm_min_realized_vol_180s_bps: 0.0,
            high_skew_max_clips: 0,
            late_favourite_max_clips: 0,
            tail_clip_frac: 0.10,
            tail_max_clips: 1,
            tail_min_ask: 0.01,
            tail_max_ask: 0.20,
            tail_min_seconds_to_close: 0.0,
            tail_target_favourite_loss_coverage_frac: 0.50,
            tail_budget_favourite_spend_frac: 1.0,
            tail_budget_favourite_upside_frac: 1.0,
            ..BonereaperV2Config::default()
        };

        let mut ctx = test_ctx(close_ns, 0.9, 0.98);
        ctx.yes_shares = 70.0;
        let mut baseline = BonereaperV2::new(base_cfg());
        let baseline_out = baseline.on_event(&event, &ctx, &spot, &TradeHistory::default());

        // Enabled + coeffs present but every knob at its inert sentinel.
        let mut ctx2 = test_ctx(close_ns, 0.9, 0.98);
        ctx2.yes_shares = 70.0;
        let mut inert = BonereaperV2::new(BonereaperV2Config {
            reversal_score_enabled: true,
            reversal_score_coeffs: Some(reversal_coeffs_intercept_only(2.0)),
            reversal_score_cov_min: f32::NAN,
            reversal_score_cov_max: f32::NAN,
            reversal_score_size_floor: 1.0,
            reversal_score_size_ceiling: 1.0,
            ..base_cfg()
        });
        let inert_out = inert.on_event(&event, &ctx2, &spot, &TradeHistory::default());

        assert_eq!(baseline_out.orders.len(), inert_out.orders.len());
        for (b, i) in baseline_out.orders.iter().zip(inert_out.orders.iter()) {
            assert_eq!(b.side, i.side);
            assert_eq!(b.tag, i.tag);
            assert_eq!(b.max_depth, i.max_depth);
            assert_eq!(b.shares.to_bits(), i.shares.to_bits(), "shares differ");
            assert_eq!(
                b.limit_price.map(f32::to_bits),
                i.limit_price.map(f32::to_bits),
                "limit_price differs"
            );
        }
    }

    #[test]
    fn reversal_score_modulates_coverage_and_size_by_fragility() {
        let close_ns = 300_000_000_000;
        let ctx = test_ctx(close_ns, 0.9, 0.98);
        let spot = test_spot_uptrend(250_000_000_000);
        let ts = 250_000_000_000;
        let base_coverage = 0.50f32;

        let cfg = |intercept: f64| BonereaperV2Config {
            reversal_score_enabled: true,
            reversal_score_coeffs: Some(reversal_coeffs_intercept_only(intercept)),
            reversal_score_cov_min: 0.40,
            reversal_score_cov_max: 1.00,
            reversal_score_size_floor: 0.25,
            reversal_score_size_ceiling: 1.00,
            ..BonereaperV2Config::default()
        };

        // High score (intercept large +) => sigmoid ~1 => coverage near cov_max,
        // size near size_floor.
        let fragile = BonereaperV2::new(cfg(8.0));
        let frag_cov =
            fragile.reversal_modulated_coverage(&ctx, &spot, ts, Side::BuyYes, 0.90, base_coverage);
        let frag_size = fragile.reversal_size_mult(&ctx, &spot, ts, Side::BuyYes, 0.90);

        // Low score (intercept large -) => sigmoid ~0 => coverage near cov_min,
        // size near size_ceiling.
        let stable = BonereaperV2::new(cfg(-8.0));
        let stable_cov =
            stable.reversal_modulated_coverage(&ctx, &spot, ts, Side::BuyYes, 0.90, base_coverage);
        let stable_size = stable.reversal_size_mult(&ctx, &spot, ts, Side::BuyYes, 0.90);

        assert!(
            frag_cov > stable_cov,
            "frag_cov={frag_cov} stable_cov={stable_cov}"
        );
        assert!(frag_cov > 0.95, "frag_cov={frag_cov}");
        assert!(stable_cov < 0.45, "stable_cov={stable_cov}");
        assert!(
            frag_size < stable_size,
            "frag_size={frag_size} stable_size={stable_size}"
        );
        assert!(frag_size < 0.30, "frag_size={frag_size}");
        assert!(stable_size > 0.95, "stable_size={stable_size}");
    }

    #[test]
    fn late_favourite_ladder_scales_by_price_and_final_window() {
        assert_eq!(late_favourite_ladder_levels(0.72, 200.0), 1);
        assert_eq!(late_favourite_ladder_levels(0.77, 200.0), 2);
        assert_eq!(late_favourite_ladder_levels(0.84, 200.0), 3);
        assert_eq!(late_favourite_ladder_levels(0.91, 120.0), 4);
        assert_eq!(late_favourite_ladder_levels(0.91, 190.0), 5);
    }

    #[test]
    fn participation_sleeve_emits_paired_maker_quotes_when_pair_cost_is_good() {
        let close_ns = 300_000_000_000;
        let mut strat = BonereaperV2::new(BonereaperV2Config {
            max_clip_usdc: 10.0,
            participation_clip_frac: 0.50,
            participation_max_pair_cost: 0.99,
            ..BonereaperV2Config::default()
        });
        let out = strat.on_event(
            &test_event(10_000_000_000, 0.495, 0.49, 0.50),
            &test_ctx(close_ns, 0.0, 0.55),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 2);
        assert!(
            out.orders
                .iter()
                .any(|o| o.tag == "br2_participation_yes" && o.limit_price == Some(0.49))
        );
        assert!(
            out.orders
                .iter()
                .any(|o| o.tag == "br2_participation_no" && o.limit_price == Some(0.50))
        );
    }

    #[test]
    fn participation_sleeve_repairs_inventory_instead_of_adding_heavy_side() {
        let close_ns = 300_000_000_000;
        let mut ctx = test_ctx(close_ns, 0.0, 0.55);
        ctx.yes_shares = 20.0;
        let mut strat = BonereaperV2::new(BonereaperV2Config {
            max_clip_usdc: 10.0,
            participation_clip_frac: 0.50,
            participation_max_pair_cost: 0.99,
            participation_repair_inventory_delta_shares: 5.0,
            ..BonereaperV2Config::default()
        });
        let out = strat.on_event(
            &test_event(10_000_000_000, 0.495, 0.49, 0.50),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].tag, "br2_participation_no");
    }

    #[test]
    fn expensive_directional_lanes_do_not_flip_sides_after_commit() {
        let close_ns = 300_000_000_000;
        let mut strat = BonereaperV2::new(BonereaperV2Config {
            max_clip_usdc: 20.0,
            late_max_fires: 0,
            high_skew_max_clips: 0,
            late_favourite_start_secs: 180.0,
            late_favourite_max_clips: 12,
            late_favourite_min_ask: 0.70,
            late_favourite_max_ask: 0.97,
            late_favourite_min_model_edge: 0.0,
            late_favourite_high_cert_min_model_edge: 0.0,
            ..BonereaperV2Config::default()
        });

        let first = strat.on_event(
            &test_event(240_000_000_000, 0.92, 0.91, 0.93),
            &test_ctx(close_ns, 0.9, 0.94),
            &test_spot_uptrend(250_000_000_000),
            &TradeHistory::default(),
        );
        assert!(
            first
                .orders
                .iter()
                .any(|o| o.side == Side::BuyYes && o.tag == "br2_late_favourite_load")
        );

        let second = strat.on_event(
            &test_event(250_000_000_000, 0.08, 0.07, 0.09),
            &test_ctx(close_ns, -0.9, 0.94),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(
            second
                .orders
                .iter()
                .all(|o| o.tag != "br2_late_favourite_load")
        );
        assert_eq!(strat.gate_stats().late_favourite_side_lock_fail, 1);
    }

    #[test]
    fn high_cert_late_favourite_tapers_size_as_upside_collapses() {
        assert_eq!(late_favourite_high_cert_price_taper(0.94), 1.0);
        assert_eq!(late_favourite_high_cert_price_taper(0.95), 1.0);
        assert!((late_favourite_high_cert_price_taper(0.97) - 0.59).abs() < 1e-9);
        assert_eq!(late_favourite_high_cert_price_taper(0.99), 0.18);

        assert_eq!(late_favourite_high_cert_max_levels(0.94, 5), 5);
        assert_eq!(late_favourite_high_cert_max_levels(0.95, 5), 3);
        assert_eq!(late_favourite_high_cert_max_levels(0.97, 5), 2);
        assert_eq!(late_favourite_high_cert_max_levels(0.99, 5), 1);
    }

    #[test]
    fn lane_size_multiplier_scales_late_favourite_clip() {
        let close_ns = 300_000_000_000;
        let fire = |mult: f32| {
            let mut strat = BonereaperV2::new(BonereaperV2Config {
                max_clip_usdc: 20.0,
                late_max_fires: 0,
                high_skew_max_clips: 0,
                late_favourite_start_secs: 180.0,
                late_favourite_max_clips: 12,
                late_favourite_min_ask: 0.70,
                late_favourite_max_ask: 0.97,
                late_favourite_min_model_edge: 0.0,
                late_favourite_high_cert_min_model_edge: 0.0,
                lane_size_late_favourite: mult,
                ..BonereaperV2Config::default()
            });
            let out = strat.on_event(
                &test_event(240_000_000_000, 0.92, 0.91, 0.93),
                &test_ctx(close_ns, 0.9, 0.94),
                &test_spot_uptrend(250_000_000_000),
                &TradeHistory::default(),
            );
            out.orders
                .iter()
                .filter(|o| o.tag == "br2_late_favourite_load")
                .map(|o| o.shares)
                .sum::<f64>()
        };
        let full = fire(1.0);
        let cut = fire(0.2);
        assert!(full > 0.0, "baseline lane should fire");
        assert!(cut > 0.0, "cut lane should still fire (size, not gate)");
        assert!(
            (cut - full * 0.2).abs() < full * 0.05,
            "0.2x multiplier should cut shares to ~20%: full={full} cut={cut}"
        );
    }

    #[test]
    fn high_cert_late_favourite_tapers_size_by_model_edge() {
        assert_eq!(
            late_favourite_high_cert_edge_taper(0.010, 0.010, 0.030),
            0.0
        );
        assert!((late_favourite_high_cert_edge_taper(0.020, 0.010, 0.030) - 0.5).abs() < 1e-6);
        assert_eq!(
            late_favourite_high_cert_edge_taper(0.030, 0.010, 0.030),
            1.0
        );
        assert_eq!(
            late_favourite_high_cert_edge_taper(0.040, 0.010, 0.030),
            1.0
        );
        assert_eq!(
            late_favourite_high_cert_edge_taper(0.004, 0.005, 0.030),
            0.0
        );
        assert_eq!(late_favourite_high_cert_edge_taper(0.010, 0.010, 0.0), 1.0);
    }

    #[test]
    fn fragile_high_cert_taper_only_hits_low_edge_weak_path_regime() {
        assert_eq!(
            late_favourite_fragile_high_cert_taper(0.924, 0.004, 0.40, 0.923, 0.005, 0.50, 0.5),
            0.5
        );
        assert_eq!(
            late_favourite_fragile_high_cert_taper(0.922, 0.004, 0.40, 0.923, 0.005, 0.50, 0.5),
            1.0
        );
        assert_eq!(
            late_favourite_fragile_high_cert_taper(0.924, 0.006, 0.40, 0.923, 0.005, 0.50, 0.5),
            1.0
        );
        assert_eq!(
            late_favourite_fragile_high_cert_taper(0.924, 0.004, 0.60, 0.923, 0.005, 0.50, 0.5),
            1.0
        );
        assert_eq!(
            late_favourite_fragile_high_cert_taper(0.924, 0.004, 0.40, 1.0, 0.005, 0.50, 0.5),
            1.0
        );
    }

    #[test]
    fn fragile_high_cert_edge_accounts_for_expected_sweep_cost() {
        assert!((late_favourite_fragile_high_cert_effective_price(0.8999) - 0.9249).abs() < 1e-6);
        assert!((late_favourite_fragile_high_cert_effective_price(0.900) - 0.925).abs() < 1e-6);
        assert!((late_favourite_fragile_high_cert_edge(0.929, 0.900) - 0.004).abs() < 1e-6);
        assert!((late_favourite_fragile_high_cert_edge(0.929, 0.880) - 0.049).abs() < 1e-6);
        assert_eq!(
            late_favourite_fragile_high_cert_taper(
                late_favourite_fragile_high_cert_effective_price(0.900) as f64,
                late_favourite_fragile_high_cert_edge(0.929, 0.900),
                0.40,
                0.923,
                0.005,
                0.50,
                0.5,
            ),
            0.5
        );
    }

    #[test]
    fn range_throttle_interpolates_between_soft_and_hard() {
        assert_eq!(range_throttle(0.80, 0.90, 0.98), 0.0);
        assert!((range_throttle(0.94, 0.90, 0.98) - 0.5).abs() < 1e-6);
        assert_eq!(range_throttle(0.99, 0.90, 0.98), 1.0);
        assert_eq!(range_throttle(0.99, 1.0, 1.0), 0.0);
    }

    #[test]
    fn model_support_can_relax_late_favourite_skew_to_high_skew_floor() {
        let ctx = Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 100.0,
            market_yes_range_so_far: 0.0,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: Some(ModelOutput {
                direction_score: 0.8,
                confidence_score: 0.82,
                calibrated_p: 0.76,
                risk_score: 0.20,
            }),
            model_attribution: None,
            market_close_ns: 0,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
            ..Ctx::default()
        };

        let threshold = late_favourite_effective_skew_threshold(
            &ctx,
            Side::BuyYes,
            0.22,
            0.16,
            0.68,
            0.72,
            0.62,
            false,
        );
        assert!((threshold - 0.16).abs() < 1e-6);
    }

    #[test]
    fn model_skew_relief_requires_matching_side_and_active_model_gates() {
        let ctx = Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 100.0,
            market_yes_range_so_far: 0.0,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: Some(ModelOutput {
                direction_score: -0.8,
                confidence_score: 0.82,
                calibrated_p: 0.76,
                risk_score: 0.20,
            }),
            model_attribution: None,
            market_close_ns: 0,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
            ..Ctx::default()
        };

        assert_eq!(
            late_favourite_effective_skew_threshold(
                &ctx,
                Side::BuyYes,
                0.22,
                0.16,
                0.68,
                0.72,
                0.62,
                false,
            ),
            0.22
        );
        assert_eq!(
            late_favourite_effective_skew_threshold(
                &ctx,
                Side::BuyNo,
                0.22,
                0.16,
                0.68,
                0.72,
                0.62,
                true,
            ),
            0.22
        );
    }

    #[test]
    fn late_favourite_whipsaw_component_stats_accumulate() {
        let mut a = BonereaperV2GateStats {
            late_favourite_sustain_fail: 11,
            late_favourite_reversal_pressure_fail: 2,
            late_favourite_path_efficiency_fail: 3,
            late_favourite_model_direction_fail: 5,
            ..BonereaperV2GateStats::default()
        };
        a.add_assign(BonereaperV2GateStats {
            late_favourite_sustain_fail: 13,
            late_favourite_reversal_pressure_fail: 5,
            late_favourite_path_efficiency_fail: 7,
            late_favourite_model_direction_fail: 8,
            ..BonereaperV2GateStats::default()
        });
        assert_eq!(a.late_favourite_sustain_fail, 24);
        assert_eq!(a.late_favourite_reversal_pressure_fail, 7);
        assert_eq!(a.late_favourite_path_efficiency_fail, 10);
        assert_eq!(a.late_favourite_model_direction_fail, 13);
    }

    #[test]
    fn buy_side_sign_matches_resolution_direction() {
        assert_eq!(buy_side_sign(Side::BuyYes), 1.0);
        assert_eq!(buy_side_sign(Side::BuyNo), -1.0);
    }

    #[test]
    fn model_limited_buy_price_caps_sweeps_to_model_edge() {
        let ctx = Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 100.0,
            market_yes_range_so_far: 0.0,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: Some(ModelOutput {
                direction_score: 0.8,
                confidence_score: 0.9,
                calibrated_p: 0.88,
                risk_score: 0.1,
            }),
            model_attribution: None,
            market_close_ns: 0,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
            ..Ctx::default()
        };

        assert!((model_limited_buy_price(&ctx, Side::BuyYes, 0.93, 0.02) - 0.86).abs() < 1e-6);
        assert!((model_limited_buy_price(&ctx, Side::BuyNo, 0.93, 0.02) - 0.10).abs() < 1e-6);
    }

    #[test]
    fn expensive_directional_orders_carry_model_edge_limit() {
        let close_ns = 300_000_000_000;
        let mut strat = BonereaperV2::new(BonereaperV2Config {
            max_clip_usdc: 20.0,
            late_clip_frac: 1.0,
            late_max_fires: 1,
            late_confirm_min_model_edge: 0.03,
            late_confirm_min_model_confidence: 0.80,
            late_confirm_min_model_side_p: 0.80,
            late_confirm_min_book_skew: 0.05,
            high_skew_max_clips: 0,
            late_favourite_max_clips: 0,
            ..BonereaperV2Config::default()
        });

        let out = strat.on_event(
            &test_event(250_000_000_000, 0.81, 0.80, 0.82),
            &test_ctx(close_ns, 0.9, 0.88),
            &test_spot_uptrend(250_000_000_000),
            &TradeHistory::default(),
        );
        let order = out
            .orders
            .iter()
            .find(|order| order.tag == "br2_late_confirm")
            .expect("late confirm order");
        assert_eq!(order.side, Side::BuyYes);
        assert!((order.limit_price.expect("limit price") - 0.85).abs() < 1e-6);
    }

    #[test]
    fn late_confirm_observed_range_gate_blocks_stretched_markets() {
        let close_ns = 300_000_000_000;
        let mut strat = BonereaperV2::new(BonereaperV2Config {
            max_clip_usdc: 20.0,
            late_clip_frac: 1.0,
            late_max_fires: 1,
            late_confirm_min_model_edge: 0.03,
            late_confirm_min_model_confidence: 0.80,
            late_confirm_min_model_side_p: 0.80,
            late_confirm_min_book_skew: 0.05,
            late_confirm_max_observed_range: 0.50,
            high_skew_max_clips: 0,
            late_favourite_max_clips: 0,
            ..BonereaperV2Config::default()
        });
        let mut ctx = test_ctx(close_ns, 0.9, 0.98);
        ctx.market_yes_range_so_far = 0.62;

        let out = strat.on_event(
            &test_event(250_000_000_000, 0.81, 0.80, 0.82),
            &ctx,
            &test_spot_uptrend(250_000_000_000),
            &TradeHistory::default(),
        );

        assert!(
            out.orders
                .iter()
                .all(|order| order.tag != "br2_late_confirm")
        );
        assert_eq!(strat.gate_stats().late_confirm_market_range_fail, 1);
    }

    #[test]
    fn convex_tail_anchors_to_late_confirm_exposure() {
        let close_ns = 300_000_000_000;
        let mut ctx = test_ctx(close_ns, 0.9, 0.98);
        ctx.yes_shares = 70.0;
        let mut strat = BonereaperV2::new(BonereaperV2Config {
            max_clip_usdc: 50.0,
            late_clip_frac: 1.0,
            late_max_fires: 1,
            late_confirm_min_model_edge: 0.03,
            late_confirm_min_model_confidence: 0.80,
            late_confirm_min_model_side_p: 0.80,
            late_confirm_min_book_skew: 0.05,
            late_confirm_min_realized_vol_180s_bps: 0.0,
            high_skew_max_clips: 0,
            late_favourite_max_clips: 0,
            tail_clip_frac: 0.10,
            tail_max_clips: 1,
            tail_min_ask: 0.01,
            tail_max_ask: 0.20,
            tail_min_seconds_to_close: 0.0,
            tail_target_favourite_loss_coverage_frac: 0.50,
            tail_budget_favourite_spend_frac: 1.0,
            tail_budget_favourite_upside_frac: 1.0,
            ..BonereaperV2Config::default()
        });

        let out = strat.on_event(
            &test_event(250_000_000_000, 0.90, 0.89, 0.91),
            &ctx,
            &test_spot_uptrend(250_000_000_000),
            &TradeHistory::default(),
        );

        assert!(
            out.orders
                .iter()
                .any(|order| order.tag == "br2_late_confirm"),
            "orders={:?} gates={:?}",
            out.orders,
            strat.gate_stats()
        );
        assert!(
            out.orders
                .iter()
                .any(|order| order.tag == "br2_convex_tail" && order.side == Side::BuyNo),
            "{:?}",
            out.orders
        );
    }

    #[test]
    fn gate_stats_accumulate_late_favourite_pullback_failures() {
        let mut a = BonereaperV2GateStats {
            late_favourite_entry_pullback_fail: 3,
            late_favourite_avg_entry_drawdown_fail: 2,
            ..BonereaperV2GateStats::default()
        };
        a.add_assign(BonereaperV2GateStats {
            late_favourite_entry_pullback_fail: 5,
            late_favourite_avg_entry_drawdown_fail: 7,
            ..BonereaperV2GateStats::default()
        });
        assert_eq!(a.late_favourite_entry_pullback_fail, 8);
        assert_eq!(a.late_favourite_avg_entry_drawdown_fail, 9);
    }

    #[test]
    fn tail_clip_can_target_favourite_loss_coverage() {
        let clip = tail_clip_notional(40.0, 0.15, 240.0, 8.0, 0.08, 0.80, 60.0);
        assert!((clip - 7.36).abs() < 1e-6);
    }

    #[test]
    fn tail_clip_uses_legacy_clip_when_coverage_disabled() {
        let clip = tail_clip_notional(40.0, 0.15, 240.0, 8.0, 0.08, 0.0, 60.0);
        assert!((clip - 6.0).abs() < 1e-6);
    }

    #[test]
    fn tail_clip_respects_budget_under_coverage_target() {
        let clip = tail_clip_notional(40.0, 0.15, 240.0, 0.0, 0.08, 0.80, 5.0);
        assert!((clip - 5.0).abs() < 1e-9);
    }

    #[test]
    fn tail_unrealized_edge_requires_favourite_profit() {
        assert!(tail_favourite_unrealized_edge_allowed(0.88, 0.84, 0.03));
        assert!(!tail_favourite_unrealized_edge_allowed(0.86, 0.84, 0.03));
        assert!(tail_favourite_unrealized_edge_allowed(0.82, 0.84, 0.0));
    }

    #[test]
    fn tail_exposure_uses_held_favourite_shares_not_emitted_shares() {
        let (shares, notional) = effective_favourite_tail_exposure(40.0, 100.0, 75.0).unwrap();
        assert!((shares - 40.0).abs() < 1e-9);
        assert!((notional - 30.0).abs() < 1e-9);
        assert!(effective_favourite_tail_exposure(0.0, 100.0, 75.0).is_none());
        assert!(effective_favourite_tail_exposure(40.0, 0.0, 75.0).is_none());
    }

    #[test]
    fn tail_coverage_boost_targets_high_cert_reversal_window() {
        assert_eq!(
            tail_coverage_for_regime(0.50, 0.92, 20.0, 1.00, 10.0, 35.0, 0.895),
            1.00
        );
        assert_eq!(
            tail_coverage_for_regime(0.50, 0.97, 35.0, 1.00, 10.0, 35.0, 0.895),
            1.00
        );
        assert_eq!(
            tail_coverage_for_regime(0.50, 0.92, 5.0, 1.00, 10.0, 35.0, 0.895),
            0.50
        );
        assert_eq!(
            tail_coverage_for_regime(0.50, 0.88, 20.0, 1.00, 10.0, 35.0, 0.895),
            0.50
        );
    }

    #[test]
    fn tail_regime_boost_is_disabled_until_thresholds_are_set() {
        let whipsaw = WhipsawRiskSnapshot {
            score: 0.80,
            reversal_pressure: 0.70,
            path_efficiency: 0.05,
            realized_vol_180s_bps: 3.0,
            ..WhipsawRiskSnapshot::default()
        };
        assert!(!tail_regime_boost_active(
            whipsaw,
            1.0,
            1.0,
            f32::INFINITY,
            -1.0
        ));
        assert!(tail_regime_boost_active(
            whipsaw,
            0.75,
            1.0,
            f32::INFINITY,
            -1.0
        ));
        assert!(tail_regime_boost_active(
            whipsaw,
            1.0,
            0.65,
            f32::INFINITY,
            -1.0
        ));
        assert!(tail_regime_boost_active(whipsaw, 1.0, 1.0, 2.5, -1.0));
        assert!(tail_regime_boost_active(
            whipsaw,
            1.0,
            1.0,
            f32::INFINITY,
            0.10
        ));
    }

    #[test]
    fn tail_regime_boost_can_expand_coverage_budget() {
        let base_budget = (240.0_f64 * 0.20).min(50.0 * 0.25);
        let boosted_budget = (240.0_f64 * 0.35).min(50.0 * 0.50);
        let base_clip = tail_clip_notional(40.0, 0.10, 240.0, 0.0, 0.08, 0.50, base_budget);
        let boosted_clip = tail_clip_notional(40.0, 0.10, 240.0, 0.0, 0.08, 1.00, boosted_budget);
        assert!(boosted_clip > base_clip);
        assert!((base_clip - 9.6).abs() < 1e-6);
        assert!((boosted_clip - 19.2).abs() < 1e-6);
    }

    #[test]
    fn tail_observed_range_gate_requires_configured_expansion() {
        assert!(tail_observed_range_allowed(0.74, 0.0));
        assert!(!tail_observed_range_allowed(0.74, 0.75));
        assert!(tail_observed_range_allowed(0.75, 0.75));
        assert!(!tail_observed_range_allowed(0.95, 2.0));
    }

    #[test]
    fn tail_seconds_to_close_gate_blocks_buzzer_tails() {
        assert!(tail_seconds_to_close_allowed(9.9, 0.0));
        assert!(!tail_seconds_to_close_allowed(9.9, 10.0));
        assert!(tail_seconds_to_close_allowed(10.0, 10.0));
        assert!(tail_seconds_to_close_allowed(0.0, -5.0));
    }

    // Hedge-first config used by the tests below. Everything else is default.
    // NOTE: in this engine the NO leg is synthesized as `1 - yes_bid`, so the
    // taker pair cost is `yes_ask + (1 - yes_bid) = 1 + (yes_ask - yes_bid)`.
    // That only drops below the 0.98 arb-lock threshold when the YES book is
    // crossed (`yes_ask < yes_bid`), which is precisely the dislocation a
    // hedge-first operator wants to lock. The test event below uses such a
    // crossed book to exercise the lane.
    fn hedged_first_cfg() -> BonereaperV2Config {
        BonereaperV2Config {
            hedged_base_enabled: true,
            hedged_base_max_secs_in: 240.0,
            hedged_base_max_pair_cost: 0.98,
            hedged_base_min_minority_leg_frac: 0.20,
            hedged_base_clip_usdc: 5.0,
            hedged_base_max_notional_usdc: 40.0,
            late_directional_overlay_frac: 0.064,
            ..BonereaperV2Config::default()
        }
    }

    #[test]
    fn hedged_base_locks_arb_pair_and_is_plus_ev_on_either_outcome() {
        let close_ns = 300_000_000_000;
        let mut strat = BonereaperV2::new(hedged_first_cfg());
        // Crossed book: yes_ask 0.46 < yes_bid 0.49 -> NO leg = 1 - 0.49 = 0.51.
        // Pair cost = 0.46 + 0.51 = 0.97 < 0.98 -> arb locked.
        let event = test_event(10_000_000_000, 0.475, 0.49, 0.46);
        let out = strat.on_event(
            &event,
            &test_ctx(close_ns, 0.0, 0.50),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );

        // Both legs emitted as takers (no limit price) under the participation
        // tag so the runner exempts them from the directional model gate.
        let yes = out
            .orders
            .iter()
            .find(|o| o.side == Side::BuyYes)
            .expect("yes leg emitted");
        let no = out
            .orders
            .iter()
            .find(|o| o.side == Side::BuyNo)
            .expect("no leg emitted");
        assert_eq!(yes.tag, "br2_participation_hedged_base");
        assert_eq!(no.tag, "br2_participation_hedged_base");
        assert!(yes.limit_price.is_none());
        assert!(no.limit_price.is_none());
        assert!(yes.tag.starts_with("br2_participation_"));

        let yes_px = 0.46_f64;
        let no_px = 1.0 - 0.49_f64;
        let total_cost = yes.shares * yes_px + no.shares * no_px;
        // Engine pays $1 per winning share. The pair is +EV regardless of
        // which side resolves: each leg's $1 payout exceeds the combined cost.
        let pnl_if_yes = yes.shares * 1.0 - total_cost;
        let pnl_if_no = no.shares * 1.0 - total_cost;
        assert!(pnl_if_yes > 0.0, "pnl_if_yes={pnl_if_yes}");
        assert!(pnl_if_no > 0.0, "pnl_if_no={pnl_if_no}");
    }

    #[test]
    fn hedged_base_does_not_open_on_normal_uncrossed_book() {
        let close_ns = 300_000_000_000;
        let mut strat = BonereaperV2::new(hedged_first_cfg());
        // Normal book: pair cost = 0.51 + (1 - 0.49) = 1.02 >= 0.98 -> no arb.
        let out = strat.on_event(
            &test_event(10_000_000_000, 0.50, 0.49, 0.51),
            &test_ctx(close_ns, 0.0, 0.50),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(
            out.orders
                .iter()
                .all(|o| o.tag != "br2_participation_hedged_base")
        );
    }

    #[test]
    fn hedged_base_disabled_is_byte_identical_to_baseline() {
        let close_ns = 300_000_000_000;
        // Same crossed book that would trigger the lane when enabled.
        let event = test_event(10_000_000_000, 0.475, 0.49, 0.46);
        let ctx = test_ctx(close_ns, 0.0, 0.50);

        let mut baseline = BonereaperV2::new(BonereaperV2Config::default());
        let baseline_out = baseline.on_event(
            &event,
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );

        // Hedge-first knobs set EXCEPT the enable flag: must be inert.
        let mut disabled = BonereaperV2::new(BonereaperV2Config {
            hedged_base_enabled: false,
            hedged_base_max_notional_usdc: 40.0,
            hedged_base_clip_usdc: 5.0,
            late_directional_overlay_frac: 0.064,
            ..BonereaperV2Config::default()
        });
        let disabled_out = disabled.on_event(
            &event,
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );

        assert_eq!(baseline_out.orders.len(), disabled_out.orders.len());
        for (a, b) in baseline_out.orders.iter().zip(disabled_out.orders.iter()) {
            assert_eq!(a.side, b.side);
            assert_eq!(a.shares, b.shares);
            assert_eq!(a.max_depth, b.max_depth);
            assert_eq!(a.limit_price, b.limit_price);
            assert_eq!(a.tag, b.tag);
        }
        // And specifically: no hedged-base orders exist in either output.
        assert!(
            disabled_out
                .orders
                .iter()
                .all(|o| o.tag != "br2_participation_hedged_base")
        );
    }

    #[test]
    fn regime_gate_disabled_is_byte_identical_to_baseline() {
        let close_ns = 300_000_000_000;
        // Drive across the late window with a strong favourite so the
        // directional lanes can fire; replay several events.
        let ctx = Ctx {
            prior_market_range_1d: 0.90,
            prior_market_range_3d: 0.90,
            prior_market_range_7d: 0.90,
            ..test_ctx(close_ns, 0.40, 0.85)
        };
        let events: Vec<_> = (0..40)
            .map(|i| test_event(i * 5_000_000_000, 0.85, 0.84, 0.86))
            .collect();
        let spot = test_spot_uptrend(close_ns);

        let mut baseline = BonereaperV2::new(BonereaperV2Config::default());
        // Regime-gate knobs set aggressively EXCEPT the enable flag: must be inert.
        let mut disabled = BonereaperV2::new(BonereaperV2Config {
            regime_gate_enabled: false,
            regime_gate_window: 3,
            regime_gate_threshold: 0.10,
            regime_gate_lane_floor: 0.0,
            regime_gate_whipsaw_weight: 1.0,
            ..BonereaperV2Config::default()
        });

        for e in &events {
            let b = baseline.on_event(e, &ctx, &spot, &TradeHistory::default());
            let d = disabled.on_event(e, &ctx, &spot, &TradeHistory::default());
            assert_eq!(b.orders.len(), d.orders.len());
            for (a, c) in b.orders.iter().zip(d.orders.iter()) {
                assert_eq!(a.side, c.side);
                assert_eq!(a.shares, c.shares);
                assert_eq!(a.max_depth, c.max_depth);
                assert_eq!(a.limit_price, c.limit_price);
                assert_eq!(a.tag, c.tag);
            }
        }
    }

    #[test]
    fn regime_gate_lane_mult_amputates_only_when_whippy() {
        let close_ns = 300_000_000_000;
        let whippy = BonereaperV2::new(BonereaperV2Config {
            regime_gate_enabled: true,
            regime_gate_window: 3,
            regime_gate_threshold: 0.50,
            regime_gate_lane_floor: 0.0,
            regime_gate_soft_band: 0.0,
            regime_gate_whipsaw_weight: 0.0,
            ..BonereaperV2Config::default()
        });
        // Trending regime: trailing range below threshold => full size.
        let calm = Ctx {
            prior_market_range_3d: 0.20,
            ..test_ctx(close_ns, 0.0, 0.50)
        };
        assert_eq!(whippy.regime_gate_lane_mult(&calm, 0.0), 1.0);
        // Whippy regime: trailing range above threshold => amputated to floor.
        let choppy = Ctx {
            prior_market_range_3d: 0.80,
            ..test_ctx(close_ns, 0.0, 0.50)
        };
        assert_eq!(whippy.regime_gate_lane_mult(&choppy, 0.0), 0.0);
        // Warmup (no prior history) => full size regardless.
        let warmup = Ctx {
            prior_market_range_3d: 0.0,
            ..test_ctx(close_ns, 0.0, 0.50)
        };
        assert_eq!(whippy.regime_gate_lane_mult(&warmup, 0.0), 1.0);

        // Soft band ramps linearly.
        let soft = BonereaperV2::new(BonereaperV2Config {
            regime_gate_enabled: true,
            regime_gate_threshold: 0.50,
            regime_gate_soft_band: 0.20,
            regime_gate_lane_floor: 0.0,
            ..BonereaperV2Config::default()
        });
        let mid = Ctx {
            prior_market_range_3d: 0.40, // halfway through the [0.30, 0.50] band
            ..test_ctx(close_ns, 0.0, 0.50)
        };
        let m = soft.regime_gate_lane_mult(&mid, 0.0);
        assert!((m - 0.5).abs() < 1e-6, "expected ~0.5 got {m}");
    }

    #[test]
    fn late_directional_overlay_cap_does_not_constrain_baseline() {
        // With hedged_base disabled (default), directional_overlay_allowed is
        // always true even though the default overlay_frac * 0 base = 0 cap
        // would otherwise block everything. This guards the default-off
        // invariant for the directional lanes.
        let strat = BonereaperV2::new(BonereaperV2Config::default());
        assert!(strat.directional_overlay_allowed());
    }
}
