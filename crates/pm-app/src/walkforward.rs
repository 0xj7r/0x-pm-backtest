//! Walk-forward harness — run many markets through one or more strategies and
//! aggregate the per-market results into a summary table.
//!
//! Concurrency (custom high-fidelity path):
//!   * tokio + buffer_unordered for parallel data loading (parquets + spot + trades).
//!   * rayon par_iter across markets/days for CPU work when !portfolio_mode.
//!   * Portfolio mode is strictly serial (required for equity compounding).
//!   * Per-strategy rayon inside each market for additional speedup on multi-strategy runs.

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, NaiveDate};
use futures::StreamExt;
use pm_model::{
    MarketAsset, MetaFeatureWeight, MetaTrainingConfig, MetaTrainingSample, MetaTrainingStats,
    ModelMarketContext, ModelState, OnlineMetaCalibrator, OnlineMetaCalibratorSnapshot,
    SkewWinRateTable,
};
use pm_risk::PortfolioLimits;
use pm_strategy::{
    BonereaperLite, BonereaperV2, BuyYesAtOpen, DeltaNeutralMm, LateBigBet, LateConfirmation,
    LateConvexTail, NoopStrategy, PairedMmDense, ReactiveDirectional, SpotMomentumFollower,
    UnlawfulRecycler,
    back_to_explore::{BackToExploreConfig, BackToExploreTaker},
    bonereaper::BonereaperLiteConfig,
    bonereaper_v2::{BonereaperV2Config, BonereaperV2GateStats, ReversalScoreCoeffs},
    delta_neutral_mm::DeltaNeutralMmConfig,
    late_big_bet::LateBigBetConfig,
    late_confirmation::LateConfirmationConfig,
    late_convex_tail::LateConvexTailConfig,
    paired_mm::PairedMmDenseConfig,
    reactive::ReactiveDirectionalConfig,
    spot_follower::SpotMomentumFollowerConfig,
    unlawful_recycler::UnlawfulRecyclerConfig,
};
use pm_telonex_loader::{
    Channel, TelonexStore, load_binance_agg_trades_async, load_book_snapshot_async,
    load_pm_trades_async, resolve_binance_day, resolve_pm_trades_day,
};
use pm_types::{MarketId, ReplayEvent, SpotHistory, SpotTick, TradeHistory};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::discovery::{MarketHandle, parse_close_ts, spot_cache_key, spot_symbol_for_market};
use crate::runner::{RunnerConfig, run_backtest};

const DEFAULT_META_MAX_FIT_SAMPLES: usize = 120_000;
const DEFAULT_META_MAX_VALIDATION_SAMPLES: usize = 60_000;
const DEFAULT_META_MAX_OOS_EVALUATION_SAMPLES: usize = 120_000;
const DEFAULT_META_MAX_SAMPLES_PER_MARKET: usize = 64;

/// Minimal profile for BonereaperV2. Loaded from TOML via --profile.
/// Fields are optional so partial profiles can override only the knobs under test.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "snake_case")]
pub struct BonereaperV2Profile {
    /// Run-level research speed knob. 0 preserves every raw book event; values
    /// such as 1000 keep one event per second plus first/last event.
    pub replay_sample_ms: Option<u64>,
    pub disable_internal_model_gates: Option<bool>,
    pub participation_clip_frac: Option<f32>,
    pub participation_max_pair_cost: Option<f32>,
    pub participation_max_orders_per_leg: Option<usize>,
    pub participation_max_inventory_delta_shares: Option<f64>,
    pub participation_repair_inventory_delta_shares: Option<f64>,
    pub participation_refresh_secs: Option<f32>,
    pub participation_stop_secs_before_close: Option<f32>,
    pub hedged_base_enabled: Option<bool>,
    pub hedged_base_max_secs_in: Option<f32>,
    pub hedged_base_max_pair_cost: Option<f32>,
    pub hedged_base_min_minority_leg_frac: Option<f32>,
    pub hedged_base_clip_usdc: Option<f32>,
    pub hedged_base_max_notional_usdc: Option<f32>,
    pub late_directional_overlay_frac: Option<f32>,
    pub min_composite_direction: Option<f32>,
    pub late_clip_frac: Option<f32>,
    pub late_max_fires: Option<usize>,
    pub late_confirm_min_model_edge: Option<f32>,
    pub late_confirm_min_realized_vol_180s_bps: Option<f32>,
    pub late_confirm_max_observed_range: Option<f32>,
    pub recent_regime_gate_enabled: Option<bool>,
    pub recent_regime_gate_min_edge: Option<f32>,
    pub recent_regime_gate_late_confirm: Option<bool>,
    pub recent_regime_gate_high_skew: Option<bool>,
    pub recent_regime_gate_late_favourite: Option<bool>,
    pub high_skew_clip_frac: Option<f32>,
    pub lane_size_late_favourite: Option<f32>,
    pub lane_size_late_confirm: Option<f32>,
    pub lane_size_high_skew: Option<f32>,
    pub regime_gate_enabled: Option<bool>,
    pub regime_gate_window: Option<u8>,
    pub regime_gate_threshold: Option<f32>,
    pub regime_gate_soft_band: Option<f32>,
    pub regime_gate_lane_floor: Option<f32>,
    pub regime_gate_whipsaw_weight: Option<f32>,
    pub high_skew_max_clips: Option<usize>,
    pub high_skew_max_whipsaw_score: Option<f32>,
    pub high_skew_min_realized_vol_180s_bps: Option<f32>,
    pub late_favourite_start_secs: Option<f32>,
    pub late_favourite_threshold: Option<f32>,
    pub late_favourite_min_ask: Option<f32>,
    pub late_favourite_max_ask: Option<f32>,
    pub late_favourite_clip_frac: Option<f32>,
    pub late_favourite_high_cert_clip_frac: Option<f32>,
    pub late_favourite_high_cert_full_clip_edge: Option<f32>,
    pub late_favourite_max_clips: Option<usize>,
    pub late_favourite_sweep_depth: Option<usize>,
    pub late_favourite_min_model_confidence: Option<f32>,
    pub late_favourite_max_model_risk: Option<f32>,
    pub late_favourite_min_model_side_p: Option<f32>,
    pub late_favourite_min_model_edge: Option<f32>,
    pub late_favourite_high_cert_min_model_edge: Option<f32>,
    pub late_favourite_fragile_high_cert_ask: Option<f32>,
    pub late_favourite_fragile_high_cert_max_edge: Option<f32>,
    pub late_favourite_fragile_high_cert_max_path_efficiency: Option<f32>,
    pub late_favourite_fragile_high_cert_size_frac: Option<f32>,
    pub late_favourite_range_soft_throttle: Option<f32>,
    pub late_favourite_range_hard_throttle: Option<f32>,
    pub late_favourite_range_extra_edge: Option<f32>,
    pub late_favourite_range_extra_confidence: Option<f32>,
    pub late_favourite_max_whipsaw_score: Option<f32>,
    pub late_favourite_max_reversal_pressure: Option<f32>,
    pub late_favourite_min_path_efficiency: Option<f32>,
    pub late_favourite_min_realized_vol_180s_bps: Option<f32>,
    pub tail_clip_frac: Option<f32>,
    pub tail_max_clips: Option<usize>,
    pub tail_sweep_depth: Option<usize>,
    pub tail_min_ask: Option<f32>,
    pub tail_max_ask: Option<f32>,
    pub tail_min_seconds_to_close: Option<f32>,
    pub tail_min_favourite_unrealized_edge: Option<f32>,
    pub tail_extreme_threshold: Option<f32>,
    pub tail_min_skew_step: Option<f32>,
    pub tail_target_favourite_loss_coverage_frac: Option<f32>,
    pub tail_budget_favourite_spend_frac: Option<f32>,
    pub tail_budget_favourite_upside_frac: Option<f32>,
    pub tail_reversal_coverage_frac: Option<f32>,
    pub tail_reversal_min_seconds_to_close: Option<f32>,
    pub tail_reversal_max_seconds_to_close: Option<f32>,
    pub tail_reversal_min_favourite_ask: Option<f32>,
    pub tail_regime_boost_coverage_frac: Option<f32>,
    pub tail_regime_boost_budget_spend_frac: Option<f32>,
    pub tail_regime_boost_budget_upside_frac: Option<f32>,
    pub tail_regime_boost_min_whipsaw_score: Option<f32>,
    pub tail_regime_boost_min_reversal_pressure: Option<f32>,
    pub tail_regime_boost_min_realized_vol_180s_bps: Option<f32>,
    pub tail_regime_boost_max_path_efficiency: Option<f32>,
    pub model_btc_whipsaw_risk_weight: Option<f32>,
    pub model_btc_path_inefficiency_risk_weight: Option<f32>,
    pub model_btc_reversal_pressure_risk_weight: Option<f32>,
    pub reversal_score_enabled: Option<bool>,
    pub reversal_score_coeffs_path: Option<PathBuf>,
    pub reversal_score_cov_min: Option<f32>,
    pub reversal_score_cov_max: Option<f32>,
    pub reversal_score_size_floor: Option<f32>,
    pub reversal_score_size_ceiling: Option<f32>,
}

impl BonereaperV2Profile {
    pub fn load(path: &Path) -> Result<Self> {
        StrategyProfileFile::load(path)?.into_bonereaper_v2()
    }

    pub fn apply_to_walkforward_config(&self, cfg: &mut WalkForwardConfig) {
        macro_rules! apply {
            ($profile_field:ident, $cfg_field:ident) => {
                if let Some(value) = self.$profile_field {
                    cfg.$cfg_field = value;
                }
            };
        }

        apply!(
            disable_internal_model_gates,
            br2_disable_internal_model_gates
        );
        apply!(replay_sample_ms, replay_sample_ms);
        apply!(participation_clip_frac, br2_participation_clip_frac);
        apply!(participation_max_pair_cost, br2_participation_max_pair_cost);
        apply!(
            participation_max_orders_per_leg,
            br2_participation_max_orders_per_leg
        );
        apply!(
            participation_max_inventory_delta_shares,
            br2_participation_max_inventory_delta_shares
        );
        apply!(
            participation_repair_inventory_delta_shares,
            br2_participation_repair_inventory_delta_shares
        );
        apply!(participation_refresh_secs, br2_participation_refresh_secs);
        apply!(
            participation_stop_secs_before_close,
            br2_participation_stop_secs_before_close
        );
        apply!(hedged_base_enabled, br2_hedged_base_enabled);
        apply!(hedged_base_max_secs_in, br2_hedged_base_max_secs_in);
        apply!(hedged_base_max_pair_cost, br2_hedged_base_max_pair_cost);
        apply!(
            hedged_base_min_minority_leg_frac,
            br2_hedged_base_min_minority_leg_frac
        );
        apply!(hedged_base_clip_usdc, br2_hedged_base_clip_usdc);
        apply!(
            hedged_base_max_notional_usdc,
            br2_hedged_base_max_notional_usdc
        );
        apply!(
            late_directional_overlay_frac,
            br2_late_directional_overlay_frac
        );
        apply!(min_composite_direction, br2_min_composite_direction);
        apply!(late_clip_frac, br2_late_clip_frac);
        apply!(late_max_fires, br2_late_max_fires);
        apply!(late_confirm_min_model_edge, br2_late_confirm_min_model_edge);
        apply!(
            late_confirm_min_realized_vol_180s_bps,
            br2_late_confirm_min_realized_vol_180s_bps
        );
        apply!(
            late_confirm_max_observed_range,
            br2_late_confirm_max_observed_range
        );
        apply!(recent_regime_gate_enabled, br2_recent_regime_gate_enabled);
        apply!(recent_regime_gate_min_edge, br2_recent_regime_gate_min_edge);
        apply!(
            recent_regime_gate_late_confirm,
            br2_recent_regime_gate_late_confirm
        );
        apply!(
            recent_regime_gate_high_skew,
            br2_recent_regime_gate_high_skew
        );
        apply!(
            recent_regime_gate_late_favourite,
            br2_recent_regime_gate_late_favourite
        );
        apply!(high_skew_clip_frac, br2_high_skew_clip_frac);
        apply!(lane_size_late_favourite, br2_lane_size_late_favourite);
        apply!(lane_size_late_confirm, br2_lane_size_late_confirm);
        apply!(lane_size_high_skew, br2_lane_size_high_skew);
        apply!(regime_gate_enabled, br2_regime_gate_enabled);
        apply!(regime_gate_window, br2_regime_gate_window);
        apply!(regime_gate_threshold, br2_regime_gate_threshold);
        apply!(regime_gate_soft_band, br2_regime_gate_soft_band);
        apply!(regime_gate_lane_floor, br2_regime_gate_lane_floor);
        apply!(regime_gate_whipsaw_weight, br2_regime_gate_whipsaw_weight);
        apply!(high_skew_max_clips, br2_high_skew_max_clips);
        apply!(high_skew_max_whipsaw_score, br2_high_skew_max_whipsaw_score);
        apply!(
            high_skew_min_realized_vol_180s_bps,
            br2_high_skew_min_realized_vol_180s_bps
        );
        apply!(late_favourite_start_secs, br2_late_favourite_start_secs);
        apply!(late_favourite_threshold, br2_late_favourite_threshold);
        apply!(late_favourite_min_ask, br2_late_favourite_min_ask);
        apply!(late_favourite_max_ask, br2_late_favourite_max_ask);
        apply!(late_favourite_clip_frac, br2_late_favourite_clip_frac);
        apply!(
            late_favourite_high_cert_clip_frac,
            br2_late_favourite_high_cert_clip_frac
        );
        apply!(
            late_favourite_high_cert_full_clip_edge,
            br2_late_favourite_high_cert_full_clip_edge
        );
        apply!(late_favourite_max_clips, br2_late_favourite_max_clips);
        apply!(late_favourite_sweep_depth, br2_late_favourite_sweep_depth);
        apply!(
            late_favourite_min_model_confidence,
            br2_late_favourite_min_model_confidence
        );
        apply!(
            late_favourite_max_model_risk,
            br2_late_favourite_max_model_risk
        );
        apply!(
            late_favourite_min_model_side_p,
            br2_late_favourite_min_model_side_p
        );
        apply!(
            late_favourite_min_model_edge,
            br2_late_favourite_min_model_edge
        );
        apply!(
            late_favourite_high_cert_min_model_edge,
            br2_late_favourite_high_cert_min_model_edge
        );
        apply!(
            late_favourite_fragile_high_cert_ask,
            br2_late_favourite_fragile_high_cert_ask
        );
        apply!(
            late_favourite_fragile_high_cert_max_edge,
            br2_late_favourite_fragile_high_cert_max_edge
        );
        apply!(
            late_favourite_fragile_high_cert_max_path_efficiency,
            br2_late_favourite_fragile_high_cert_max_path_efficiency
        );
        apply!(
            late_favourite_fragile_high_cert_size_frac,
            br2_late_favourite_fragile_high_cert_size_frac
        );
        apply!(
            late_favourite_range_soft_throttle,
            br2_late_favourite_range_soft_throttle
        );
        apply!(
            late_favourite_range_hard_throttle,
            br2_late_favourite_range_hard_throttle
        );
        apply!(
            late_favourite_range_extra_edge,
            br2_late_favourite_range_extra_edge
        );
        apply!(
            late_favourite_range_extra_confidence,
            br2_late_favourite_range_extra_confidence
        );
        apply!(
            late_favourite_max_whipsaw_score,
            br2_late_favourite_max_whipsaw_score
        );
        apply!(
            late_favourite_max_reversal_pressure,
            br2_late_favourite_max_reversal_pressure
        );
        apply!(
            late_favourite_min_path_efficiency,
            br2_late_favourite_min_path_efficiency
        );
        apply!(
            late_favourite_min_realized_vol_180s_bps,
            br2_late_favourite_min_realized_vol_180s_bps
        );
        apply!(tail_clip_frac, br2_tail_clip_frac);
        apply!(tail_max_clips, br2_tail_max_clips);
        apply!(tail_sweep_depth, br2_tail_sweep_depth);
        apply!(tail_min_ask, br2_tail_min_ask);
        apply!(tail_max_ask, br2_tail_max_ask);
        apply!(tail_min_seconds_to_close, br2_tail_min_seconds_to_close);
        apply!(
            tail_min_favourite_unrealized_edge,
            br2_tail_min_favourite_unrealized_edge
        );
        apply!(tail_extreme_threshold, br2_tail_extreme_threshold);
        apply!(tail_min_skew_step, br2_tail_min_skew_step);
        apply!(
            tail_target_favourite_loss_coverage_frac,
            br2_tail_target_favourite_loss_coverage_frac
        );
        apply!(
            tail_budget_favourite_spend_frac,
            br2_tail_budget_favourite_spend_frac
        );
        apply!(
            tail_budget_favourite_upside_frac,
            br2_tail_budget_favourite_upside_frac
        );
        apply!(tail_reversal_coverage_frac, br2_tail_reversal_coverage_frac);
        apply!(
            tail_reversal_min_seconds_to_close,
            br2_tail_reversal_min_seconds_to_close
        );
        apply!(
            tail_reversal_max_seconds_to_close,
            br2_tail_reversal_max_seconds_to_close
        );
        apply!(
            tail_reversal_min_favourite_ask,
            br2_tail_reversal_min_favourite_ask
        );
        apply!(
            tail_regime_boost_coverage_frac,
            br2_tail_regime_boost_coverage_frac
        );
        apply!(
            tail_regime_boost_budget_spend_frac,
            br2_tail_regime_boost_budget_spend_frac
        );
        apply!(
            tail_regime_boost_budget_upside_frac,
            br2_tail_regime_boost_budget_upside_frac
        );
        apply!(
            tail_regime_boost_min_whipsaw_score,
            br2_tail_regime_boost_min_whipsaw_score
        );
        apply!(
            tail_regime_boost_min_reversal_pressure,
            br2_tail_regime_boost_min_reversal_pressure
        );
        apply!(
            tail_regime_boost_min_realized_vol_180s_bps,
            br2_tail_regime_boost_min_realized_vol_180s_bps
        );
        apply!(
            tail_regime_boost_max_path_efficiency,
            br2_tail_regime_boost_max_path_efficiency
        );
        apply!(model_btc_whipsaw_risk_weight, model_btc_whipsaw_risk_weight);
        apply!(
            model_btc_path_inefficiency_risk_weight,
            model_btc_path_inefficiency_risk_weight
        );
        apply!(
            model_btc_reversal_pressure_risk_weight,
            model_btc_reversal_pressure_risk_weight
        );
        apply!(reversal_score_enabled, br2_reversal_score_enabled);
        apply!(reversal_score_cov_min, br2_reversal_score_cov_min);
        apply!(reversal_score_cov_max, br2_reversal_score_cov_max);
        apply!(reversal_score_size_floor, br2_reversal_score_size_floor);
        apply!(reversal_score_size_ceiling, br2_reversal_score_size_ceiling);
        if let Some(path) = &self.reversal_score_coeffs_path {
            cfg.br2_reversal_score_coeffs_path = Some(path.clone());
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct StrategyProfileFile {
    strategy: Option<StrategyProfileSelection>,
    bonereaper_v2: Option<BonereaperV2Profile>,
    back_to_explore: Option<BackToExploreProfile>,
}

#[derive(Debug, Clone, Serialize, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct StrategyProfileSelection {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Default, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub struct BackToExploreProfile {
    /// Use this as a world-class baseline clip, then applies `max(4.0, clip)` in
    /// the runner. Keep non-zero so small accounts still receive action.
    pub base_clip_usdc: Option<f64>,
    pub max_clip_usdc: Option<f64>,
    pub min_pair_cost_for_two_sided: Option<f64>,
    pub pair_clip_multiplier: Option<f64>,
    pub directional_strength_mult: Option<f64>,
    pub residual_taper_start_frac: Option<f64>,
    pub residual_min_clip_multiplier: Option<f64>,
    pub range_soft_throttle: Option<f32>,
    pub range_hard_throttle: Option<f32>,
    pub range_min_clip_multiplier: Option<f64>,
    pub range_repair_min_clip_multiplier: Option<f64>,
    pub range_chop_min_range: Option<f32>,
    pub range_clean_path_efficiency: Option<f32>,
    pub range_chop_sign_flip_rate: Option<f32>,
    pub range_reversal_pressure: Option<f32>,
    pub clean_path_directional_clip_multiplier: Option<f64>,
    pub reversal_pressure_clip_multiplier: Option<f64>,
    pub reversal_pressure_directional_min_signal: Option<f64>,
    pub reversal_pressure_directional_min_edge: Option<f64>,
    pub reversal_pressure_directional_clip_multiplier: Option<f64>,
    pub max_residual_shares: Option<f64>,
    pub min_clip_multiplier_to_emit: Option<f64>,
    pub refresh_secs: Option<f64>,
    pub stop_secs_before_close: Option<f32>,
    pub min_entry_price: Option<f32>,
    pub max_entry_price: Option<f32>,
    pub sweep_depth: Option<usize>,
    pub high_activity_hours: Option<Vec<u8>>,
    pub base_participation_rate: Option<f64>,
    pub two_sided_preference: Option<f64>,
    pub target_risk_per_clip_frac: Option<f64>,
    pub base_target_net_shares: Option<f64>,
    pub good_hour_target_net_mult: Option<f64>,
    /// Optional JSONL of per-market risk scales exported by
    /// `scripts/bte_cluster_policy_search.py --out-scale-jsonl`.
    pub policy_scales_jsonl: Option<PathBuf>,
    pub debug_signals: Option<bool>,
}

impl BackToExploreProfile {
    pub fn apply_to_walkforward_config(&self, cfg: &mut WalkForwardConfig) {
        macro_rules! apply {
            ($profile_field:ident, $cfg_field:ident) => {
                if let Some(value) = self.$profile_field {
                    cfg.$cfg_field = value;
                }
            };
        }

        apply!(base_clip_usdc, back_to_explore_base_clip_usdc);
        apply!(max_clip_usdc, back_to_explore_max_clip_usdc);
        apply!(
            min_pair_cost_for_two_sided,
            back_to_explore_min_pair_cost_for_two_sided
        );
        apply!(pair_clip_multiplier, back_to_explore_pair_clip_multiplier);
        apply!(
            directional_strength_mult,
            back_to_explore_directional_strength_mult
        );
        apply!(
            residual_taper_start_frac,
            back_to_explore_residual_taper_start_frac
        );
        apply!(
            residual_min_clip_multiplier,
            back_to_explore_residual_min_clip_multiplier
        );
        apply!(range_soft_throttle, back_to_explore_range_soft_throttle);
        apply!(range_hard_throttle, back_to_explore_range_hard_throttle);
        apply!(
            range_min_clip_multiplier,
            back_to_explore_range_min_clip_multiplier
        );
        apply!(
            range_repair_min_clip_multiplier,
            back_to_explore_range_repair_min_clip_multiplier
        );
        apply!(range_chop_min_range, back_to_explore_range_chop_min_range);
        apply!(
            range_clean_path_efficiency,
            back_to_explore_range_clean_path_efficiency
        );
        apply!(
            range_chop_sign_flip_rate,
            back_to_explore_range_chop_sign_flip_rate
        );
        apply!(
            range_reversal_pressure,
            back_to_explore_range_reversal_pressure
        );
        apply!(
            clean_path_directional_clip_multiplier,
            back_to_explore_clean_path_directional_clip_multiplier
        );
        apply!(
            reversal_pressure_clip_multiplier,
            back_to_explore_reversal_pressure_clip_multiplier
        );
        apply!(
            reversal_pressure_directional_min_signal,
            back_to_explore_reversal_pressure_directional_min_signal
        );
        apply!(
            reversal_pressure_directional_min_edge,
            back_to_explore_reversal_pressure_directional_min_edge
        );
        apply!(
            reversal_pressure_directional_clip_multiplier,
            back_to_explore_reversal_pressure_directional_clip_multiplier
        );
        apply!(max_residual_shares, back_to_explore_max_residual_shares);
        apply!(
            min_clip_multiplier_to_emit,
            back_to_explore_min_clip_multiplier_to_emit
        );
        apply!(refresh_secs, back_to_explore_refresh_secs);
        apply!(
            stop_secs_before_close,
            back_to_explore_stop_secs_before_close
        );
        apply!(min_entry_price, back_to_explore_min_entry_price);
        apply!(max_entry_price, back_to_explore_max_entry_price);
        apply!(sweep_depth, back_to_explore_sweep_depth);
        if let Some(hours) = &self.high_activity_hours {
            cfg.back_to_explore_high_activity_hours = hours.clone();
        }
        apply!(
            base_participation_rate,
            back_to_explore_base_participation_rate
        );
        apply!(two_sided_preference, back_to_explore_two_sided_preference);
        apply!(
            target_risk_per_clip_frac,
            back_to_explore_target_risk_per_clip_frac
        );
        apply!(
            base_target_net_shares,
            back_to_explore_base_target_net_shares
        );
        apply!(
            good_hour_target_net_mult,
            back_to_explore_good_hour_target_net_mult
        );
        if let Some(path) = &self.policy_scales_jsonl {
            cfg.back_to_explore_policy_scales_jsonl = Some(path.clone());
        }
        apply!(debug_signals, back_to_explore_debug_signals);
    }
}

impl StrategyProfileFile {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read profile {}", path.display()))?;
        let mut file: StrategyProfileFile = toml::from_str(&content)
            .with_context(|| format!("failed to parse profile {} as TOML", path.display()))?;

        if file.bonereaper_v2.is_none() && file.back_to_explore.is_none() {
            let fallback: BonereaperV2Profile = toml::from_str(&content).with_context(|| {
                format!(
                    "failed to parse legacy profile format {} as TOML",
                    path.display()
                )
            })?;
            file.strategy = Some(StrategyProfileSelection {
                name: "bonereaper_v2".to_string(),
            });
            file.bonereaper_v2 = Some(fallback);
        }

        if file.strategy.is_none() {
            let has_bonereaper_v2 = file.bonereaper_v2.is_some();
            let has_back_to_explore = file.back_to_explore.is_some();
            match (has_bonereaper_v2, has_back_to_explore) {
                (true, false) => {
                    file.strategy = Some(StrategyProfileSelection {
                        name: "bonereaper_v2".to_string(),
                    });
                }
                (false, true) => {
                    file.strategy = Some(StrategyProfileSelection {
                        name: "back_to_explore".to_string(),
                    });
                }
                (true, true) => {
                    return Err(anyhow!(
                        "profile has both [bonereaper_v2] and [back_to_explore] sections but no [strategy].name; set one strategy"
                    ));
                }
                (false, false) => {}
            }
        }

        if file.strategy.is_none() {
            return Err(anyhow!(
                "profile missing strategy declaration and no strategy section found"
            ));
        }

        Ok(file)
    }

    pub fn strategy_name(&self) -> Option<&str> {
        self.strategy
            .as_ref()
            .map(|strategy| strategy.name.as_str())
    }

    pub fn selected_strategy_profile(
        &self,
        active: &[StratId],
    ) -> Result<Option<ResolvedStrategyProfile>> {
        let requested = self
            .strategy_name()
            .ok_or_else(|| anyhow!("profile missing [strategy].name"))?
            .to_ascii_lowercase();

        let resolved = match requested.as_str() {
            "bonereaper_v2" => {
                if !active.iter().any(|s| *s == StratId::BonereaperV2) {
                    return Ok(None);
                }
                self.bonereaper_v2
                    .clone()
                    .ok_or_else(|| anyhow!("bonereaper_v2 profile missing [bonereaper_v2] block"))?
                    .into()
            }
            "back_to_explore" => {
                if !active.iter().any(|s| *s == StratId::BackToExplore) {
                    return Ok(None);
                }
                self.back_to_explore
                    .clone()
                    .ok_or_else(|| {
                        anyhow!("back_to_explore profile missing [back_to_explore] block")
                    })?
                    .into()
            }
            other => {
                return Err(anyhow!(
                    "profile strategy '{}' does not match any known strategy (supported: bonereaper_v2, back_to_explore)",
                    other
                ));
            }
        };

        Ok(Some(resolved))
    }

    pub fn warn_if_inactive(&self, active: &[StratId]) -> bool {
        let Some(requested) = self
            .strategy_name()
            .map(|strategy| strategy.to_ascii_lowercase())
        else {
            return false;
        };
        if requested == "bonereaper_v2" {
            !active.iter().any(|s| *s == StratId::BonereaperV2)
        } else if requested == "back_to_explore" {
            !active.iter().any(|s| *s == StratId::BackToExplore)
        } else {
            false
        }
    }

    pub fn into_bonereaper_v2(self) -> Result<BonereaperV2Profile> {
        match self.bonereaper_v2 {
            Some(profile) => Ok(profile),
            None => Err(anyhow!(
                "requested strategy profile path is not a bonereaper_v2 profile"
            )),
        }
    }
}

pub enum ResolvedStrategyProfile {
    BonereaperV2(BonereaperV2Profile),
    BackToExplore(BackToExploreProfile),
}

impl ResolvedStrategyProfile {
    pub fn strategy_id(&self) -> StratId {
        match self {
            Self::BonereaperV2(_) => StratId::BonereaperV2,
            Self::BackToExplore(_) => StratId::BackToExplore,
        }
    }

    pub fn strategy_name(&self) -> &'static str {
        self.strategy_id().name()
    }

    pub fn apply_to_walkforward_config(&self, cfg: &mut WalkForwardConfig) {
        match self {
            Self::BonereaperV2(profile) => profile.apply_to_walkforward_config(cfg),
            Self::BackToExplore(profile) => profile.apply_to_walkforward_config(cfg),
        }
    }
}

impl From<BonereaperV2Profile> for ResolvedStrategyProfile {
    fn from(profile: BonereaperV2Profile) -> Self {
        Self::BonereaperV2(profile)
    }
}

impl From<BackToExploreProfile> for ResolvedStrategyProfile {
    fn from(profile: BackToExploreProfile) -> Self {
        Self::BackToExplore(profile)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum StratId {
    BuyYesAtOpen,
    ReactiveDirectional,
    PairedMm,
    SpotMomentumFollower,
    LateBigBet,
    BonereaperLite,
    BonereaperV2,
    DeltaNeutralMm,
    LateConfirmation,
    LateConvexTail,
    BackToExplore,
    UnlawfulRecycler,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum VolatilityBand {
    Low,
    High,
}

impl VolatilityBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "range_le_threshold",
            Self::High => "range_gt_threshold",
        }
    }
}

impl StratId {
    pub const ACTIVE: [Self; 3] = [Self::BackToExplore, Self::PairedMm, Self::BonereaperV2];

    pub const ARCHIVED: [Self; 9] = [
        Self::BuyYesAtOpen,
        Self::ReactiveDirectional,
        Self::SpotMomentumFollower,
        Self::LateBigBet,
        Self::BonereaperLite,
        Self::DeltaNeutralMm,
        Self::LateConfirmation,
        Self::LateConvexTail,
        Self::UnlawfulRecycler,
    ];

    pub const ALL: [Self; 12] = [
        Self::BuyYesAtOpen,
        Self::ReactiveDirectional,
        Self::SpotMomentumFollower,
        Self::LateBigBet,
        Self::BonereaperLite,
        Self::DeltaNeutralMm,
        Self::LateConfirmation,
        Self::LateConvexTail,
        Self::UnlawfulRecycler,
        Self::PairedMm,
        Self::BackToExplore,
        Self::BonereaperV2,
    ];

    pub fn from_name(value: &str) -> Option<Self> {
        match value {
            "buy_yes_at_open" => Some(Self::BuyYesAtOpen),
            "reactive_directional" => Some(Self::ReactiveDirectional),
            "paired_mm" => Some(Self::PairedMm),
            "spot_momentum_follower" => Some(Self::SpotMomentumFollower),
            "late_big_bet" => Some(Self::LateBigBet),
            "bonereaper_lite" => Some(Self::BonereaperLite),
            "bonereaper_v2" => Some(Self::BonereaperV2),
            "delta_neutral_mm" => Some(Self::DeltaNeutralMm),
            "late_confirmation" => Some(Self::LateConfirmation),
            "late_convex_tail" => Some(Self::LateConvexTail),
            "back_to_explore" => Some(Self::BackToExplore),
            "unlawful_recycler" => Some(Self::UnlawfulRecycler),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            StratId::BuyYesAtOpen => "buy_yes_at_open",
            StratId::ReactiveDirectional => "reactive_directional",
            StratId::PairedMm => "paired_mm",
            StratId::SpotMomentumFollower => "spot_momentum_follower",
            StratId::LateBigBet => "late_big_bet",
            StratId::BonereaperLite => "bonereaper_lite",
            StratId::BonereaperV2 => "bonereaper_v2",
            StratId::DeltaNeutralMm => "delta_neutral_mm",
            StratId::LateConfirmation => "late_confirmation",
            StratId::LateConvexTail => "late_convex_tail",
            StratId::BackToExplore => "back_to_explore",
            StratId::UnlawfulRecycler => "unlawful_recycler",
        }
    }

    pub fn active_names() -> Vec<&'static str> {
        Self::ACTIVE.iter().map(|strat| strat.name()).collect()
    }

    pub fn archived_names() -> Vec<&'static str> {
        Self::ARCHIVED.iter().map(|strat| strat.name()).collect()
    }

    pub fn all_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|strat| strat.name()).collect()
    }

    pub fn is_active(self) -> bool {
        Self::ACTIVE.iter().any(|id| *id == self)
    }
}

#[derive(Debug, Clone)]
pub struct WalkForwardConfig {
    pub starting_cash_usdc: f64,
    pub kelly_fraction: f64,
    pub max_clip_usdc: f64,
    pub max_order_clip_multiplier: f64,
    pub max_per_market_exposure_usdc: f64,
    pub max_per_market_exposure_frac: Option<f64>,
    pub spot_symbol: String,
    pub strategies: Vec<StratId>,
    pub max_concurrent_fetches: usize,
    /// Optional research-speed replay thinning. `0` keeps every raw event.
    /// Non-zero keeps first/last plus at most one event per interval.
    pub replay_sample_ms: u64,
    /// Directory to cache raw loaded ReplayEvents to disk (JSONL per asset).
    /// Extremely effective on AWS when re-running the same dates (avoids repeated
    /// S3 downloads after the first run). If None, no event cache is used.
    pub replay_event_cache_dir: Option<PathBuf>,
    pub load_pm_trades: bool,
    pub use_outcome_label: bool,
    pub maker_rebate_bps: f64,
    pub taker_fee_bps: f64,
    pub taker_latency_ms: u64,
    /// **Portfolio mode**: process markets in chronological order, compound
    /// equity from one market into the next. Disables parallelism (each
    /// market's starting cash depends on the previous market's end cash).
    /// When `false`, each market is independent and starts from
    /// `starting_cash_usdc`.
    pub portfolio_mode: bool,
    /// In portfolio mode, override `max_clip_usdc` per market to be
    /// `clip_fraction_of_equity × current_equity`. Set to `None` to use
    /// the static `max_clip_usdc` regardless of bankroll. Typical: 0.005
    /// (0.5% of equity per bet).
    pub clip_fraction_of_equity: Option<f64>,
    /// Portfolio-level drawdown where clips begin scaling down. Expressed as
    /// a fraction below peak equity, e.g. `0.12` for 12%. Disabled when this
    /// is greater than or equal to `clip_drawdown_hard_pct`.
    pub clip_drawdown_soft_pct: f64,
    /// Portfolio-level drawdown where clips scale to zero. Expressed as a
    /// fraction below peak equity, e.g. `0.25` for 25%.
    pub clip_drawdown_hard_pct: f64,
    /// Minimum multiplier after the hard drawdown threshold. Default `0.0`
    /// preserves hard-stop behavior; non-zero keeps a recovery-sized lane open.
    pub clip_drawdown_min_multiplier: f64,
    /// Session-level drawdown where clips begin scaling down. Sessions reset
    /// when the market date changes. Disabled when this is greater than or
    /// equal to `clip_session_drawdown_hard_pct`.
    pub clip_session_drawdown_soft_pct: f64,
    /// Session-level drawdown where clips scale to the session floor.
    pub clip_session_drawdown_hard_pct: f64,
    /// Minimum multiplier after the session hard drawdown threshold.
    pub clip_session_drawdown_min_multiplier: f64,
    /// Daily loss cap as % of the day's starting bankroll (resets on calendar date change).
    /// E.g. 0.05 caps losses at 5% of the equity at the start of the day. 1.0 disables.
    /// Simple hard stop for the rest of the day once breached.
    pub daily_loss_cap_pct: f64,
    /// Replay-safe portfolio risk control: after this many consecutive losing
    /// traded markets, set clip size to zero for the next configured markets.
    /// Disabled when `0`.
    pub loss_streak_cooldown_after: usize,
    /// Number of subsequent markets to skip after the loss streak trips.
    pub loss_streak_cooldown_markets: usize,
    /// PnL threshold used to classify a traded market as losing.
    pub loss_streak_loss_threshold_usdc: f64,
    pub br2_disable_internal_model_gates: bool,
    pub br2_participation_clip_frac: f32,
    pub br2_participation_max_pair_cost: f32,
    pub br2_participation_max_orders_per_leg: usize,
    pub br2_participation_max_inventory_delta_shares: f64,
    pub br2_participation_repair_inventory_delta_shares: f64,
    pub br2_participation_refresh_secs: f32,
    pub br2_participation_stop_secs_before_close: f32,
    pub br2_hedged_base_enabled: bool,
    pub br2_hedged_base_max_secs_in: f32,
    pub br2_hedged_base_max_pair_cost: f32,
    pub br2_hedged_base_min_minority_leg_frac: f32,
    pub br2_hedged_base_clip_usdc: f32,
    pub br2_hedged_base_max_notional_usdc: f32,
    pub br2_late_directional_overlay_frac: f32,
    pub br2_min_composite_direction: f32,
    pub br2_early_clip_frac: f32,
    pub br2_mid_clip_frac: f32,
    pub br2_late_clip_frac: f32,
    pub br2_late_max_fires: usize,
    pub br2_late_confirm_min_model_confidence: f32,
    pub br2_late_confirm_max_model_risk: f32,
    pub br2_late_confirm_min_model_side_p: f32,
    pub br2_late_confirm_min_model_edge: f32,
    pub br2_late_confirm_min_book_skew: f32,
    pub br2_late_confirm_max_whipsaw_score: f32,
    pub br2_late_confirm_min_realized_vol_180s_bps: f32,
    pub br2_late_confirm_max_observed_range: f32,
    pub br2_recent_regime_gate_enabled: bool,
    pub br2_recent_regime_gate_min_edge: f32,
    pub br2_recent_regime_gate_late_confirm: bool,
    pub br2_recent_regime_gate_high_skew: bool,
    pub br2_recent_regime_gate_late_favourite: bool,
    pub br2_high_skew_clip_frac: f32,
    pub br2_lane_size_late_favourite: f32,
    pub br2_lane_size_late_confirm: f32,
    pub br2_lane_size_high_skew: f32,
    pub br2_regime_gate_enabled: bool,
    pub br2_regime_gate_window: u8,
    pub br2_regime_gate_threshold: f32,
    pub br2_regime_gate_soft_band: f32,
    pub br2_regime_gate_lane_floor: f32,
    pub br2_regime_gate_whipsaw_weight: f32,
    pub br2_high_skew_max_clips: usize,
    pub br2_high_skew_max_whipsaw_score: f32,
    pub br2_high_skew_min_realized_vol_180s_bps: f32,
    pub br2_late_favourite_start_secs: f32,
    pub br2_late_favourite_threshold: f32,
    pub br2_late_favourite_min_ask: f32,
    pub br2_late_favourite_max_ask: f32,
    pub br2_late_favourite_clip_frac: f32,
    pub br2_late_favourite_high_cert_clip_frac: f32,
    pub br2_late_favourite_high_cert_full_clip_edge: f32,
    pub br2_late_favourite_fragile_high_cert_ask: f32,
    pub br2_late_favourite_fragile_high_cert_max_edge: f32,
    pub br2_late_favourite_fragile_high_cert_max_path_efficiency: f32,
    pub br2_late_favourite_fragile_high_cert_size_frac: f32,
    pub br2_late_favourite_max_clips: usize,
    pub br2_late_favourite_min_sustain_secs: f32,
    pub br2_late_favourite_sweep_depth: usize,
    pub br2_late_favourite_min_model_confidence: f32,
    pub br2_late_favourite_min_model_direction_abs: f32,
    pub br2_late_favourite_max_model_risk: f32,
    pub br2_late_favourite_min_model_side_p: f32,
    pub br2_late_favourite_min_model_edge: f32,
    pub br2_late_favourite_high_cert_min_model_edge: f32,
    pub br2_late_favourite_high_cert_bypass_model_edge: bool,
    pub br2_late_favourite_max_whipsaw_score: f32,
    pub br2_late_favourite_max_reversal_pressure: f32,
    pub br2_late_favourite_min_path_efficiency: f32,
    pub br2_late_favourite_min_realized_vol_180s_bps: f32,
    pub br2_late_favourite_max_observed_range: f32,
    pub br2_late_favourite_range_soft_throttle: f32,
    pub br2_late_favourite_range_hard_throttle: f32,
    pub br2_late_favourite_range_extra_edge: f32,
    pub br2_late_favourite_range_extra_confidence: f32,
    pub br2_late_favourite_max_adverse_fast_momentum: f32,
    pub br2_late_favourite_max_adverse_broad_momentum: f32,
    pub br2_late_favourite_max_entry_pullback: f32,
    pub br2_late_favourite_max_avg_entry_drawdown: f32,
    pub br2_tail_clip_frac: f32,
    pub br2_tail_max_clips: usize,
    pub br2_tail_sweep_depth: usize,
    pub br2_tail_min_ask: f32,
    pub br2_tail_max_ask: f32,
    pub br2_tail_min_seconds_to_close: f32,
    pub br2_tail_min_favourite_unrealized_edge: f32,
    pub br2_tail_min_observed_range: f32,
    pub br2_tail_target_favourite_loss_coverage_frac: f32,
    pub br2_tail_reversal_coverage_frac: f32,
    pub br2_tail_reversal_min_seconds_to_close: f32,
    pub br2_tail_reversal_max_seconds_to_close: f32,
    pub br2_tail_reversal_min_favourite_ask: f32,
    pub br2_tail_extreme_threshold: f32,
    pub br2_tail_min_skew_step: f32,
    pub br2_tail_budget_favourite_spend_frac: f32,
    pub br2_tail_budget_favourite_upside_frac: f32,
    pub br2_tail_regime_boost_coverage_frac: f32,
    pub br2_tail_regime_boost_budget_spend_frac: f32,
    pub br2_tail_regime_boost_budget_upside_frac: f32,
    pub br2_tail_regime_boost_min_whipsaw_score: f32,
    pub br2_tail_regime_boost_min_reversal_pressure: f32,
    pub br2_tail_regime_boost_min_realized_vol_180s_bps: f32,
    pub br2_tail_regime_boost_max_path_efficiency: f32,
    pub back_to_explore_base_clip_usdc: f64,
    pub back_to_explore_max_clip_usdc: f64,
    pub back_to_explore_min_pair_cost_for_two_sided: f64,
    pub back_to_explore_pair_clip_multiplier: f64,
    pub back_to_explore_directional_strength_mult: f64,
    pub back_to_explore_residual_taper_start_frac: f64,
    pub back_to_explore_residual_min_clip_multiplier: f64,
    pub back_to_explore_range_soft_throttle: f32,
    pub back_to_explore_range_hard_throttle: f32,
    pub back_to_explore_range_min_clip_multiplier: f64,
    pub back_to_explore_range_repair_min_clip_multiplier: f64,
    pub back_to_explore_range_chop_min_range: f32,
    pub back_to_explore_range_clean_path_efficiency: f32,
    pub back_to_explore_range_chop_sign_flip_rate: f32,
    pub back_to_explore_range_reversal_pressure: f32,
    pub back_to_explore_clean_path_directional_clip_multiplier: f64,
    pub back_to_explore_reversal_pressure_clip_multiplier: f64,
    pub back_to_explore_reversal_pressure_directional_min_signal: f64,
    pub back_to_explore_reversal_pressure_directional_min_edge: f64,
    pub back_to_explore_reversal_pressure_directional_clip_multiplier: f64,
    pub back_to_explore_max_residual_shares: f64,
    pub back_to_explore_min_clip_multiplier_to_emit: f64,
    pub back_to_explore_refresh_secs: f64,
    pub back_to_explore_stop_secs_before_close: f32,
    pub back_to_explore_min_entry_price: f32,
    pub back_to_explore_max_entry_price: f32,
    pub back_to_explore_sweep_depth: usize,
    pub back_to_explore_high_activity_hours: Vec<u8>,
    pub back_to_explore_base_participation_rate: f64,
    pub back_to_explore_two_sided_preference: f64,
    pub back_to_explore_target_risk_per_clip_frac: f64,
    pub back_to_explore_base_target_net_shares: f64,
    pub back_to_explore_good_hour_target_net_mult: f64,
    pub back_to_explore_policy_scales_jsonl: Option<PathBuf>,
    pub back_to_explore_debug_signals: bool,
    /// Phase-3 reversal-risk score modulators. OFF by default; inert defaults
    /// keep orders byte-identical to baseline.
    pub br2_reversal_score_enabled: bool,
    /// Path to the Phase-2 logistic coefficients JSON. None => score path off.
    pub br2_reversal_score_coeffs_path: Option<PathBuf>,
    pub br2_reversal_score_cov_min: f32,
    pub br2_reversal_score_cov_max: f32,
    pub br2_reversal_score_size_floor: f32,
    pub br2_reversal_score_size_ceiling: f32,
    pub enforce_model_gate: bool,
    pub model_gate_min_confidence: f32,
    pub model_gate_max_risk: f32,
    pub model_gate_min_edge: f32,
    pub model_btc_whipsaw_risk_weight: f32,
    pub model_btc_path_inefficiency_risk_weight: f32,
    pub model_btc_reversal_pressure_risk_weight: f32,
    /// Opt into explicit asset/timeframe features for mixed BTC/ETH or 5m/15m
    /// model experiments. Kept off by default to preserve BTC 5m baseline
    /// comparability.
    pub enable_market_context_features: bool,
    /// Split aggregate reporting by per-market price range in YES mid: high
    /// volatility if `range > threshold`.
    pub volatility_regime_threshold: f64,
    /// Enable walk-forward folds. Mutually exclusive with `fold_size`.
    /// If set, markets are split into this many chronological folds.
    pub walk_forward_folds: Option<usize>,
    /// Enable walk-forward folds with explicit fold-size (in markets).
    /// Mutually exclusive with `walk_forward_folds`.
    pub fold_size: Option<usize>,
    /// Purge this many markets around each train/test boundary.
    /// With forward-purged CV this excludes the immediately adjacent markets
    /// from training to reduce label leakage.
    pub purge_markets: usize,
    /// Do not evaluate a test fold until at least this many prior markets are
    /// available for walk-forward meta-calibrator training.
    pub min_train_markets: usize,
    /// Online meta-calibrator fit hyperparameters. These are intentionally
    /// runtime-tunable because validation often rejects overfit settings.
    pub meta_training_config: MetaTrainingConfig,
    /// Maximum market-balanced samples used for fitting the meta-calibrator.
    /// The raw extracted cache is still retained; this only bounds fit cost
    /// and prevents dense tick markets from dominating the objective.
    pub meta_max_fit_samples: usize,
    /// Maximum market-balanced samples used for validation selection.
    pub meta_max_validation_samples: usize,
    /// Maximum samples retained from a single market for fit/validation/OOS
    /// meta-calibrator diagnostics.
    pub meta_max_samples_per_market: usize,
    /// Maximum market-balanced OOS samples used in summary diagnostics.
    pub meta_max_oos_evaluation_samples: usize,
    /// Optional training/evaluation filter: keep only meta samples with base
    /// predicted-side probability at least this high.
    pub meta_train_min_base_p: f32,
    /// Optional training/evaluation filter: keep only samples past the early
    /// market penalty, e.g. `0.05` for late/candidate-regime calibration.
    pub meta_train_max_early_penalty: f32,
    /// Optional training/evaluation filter on `2 * abs(mid - 0.5)`.
    pub meta_train_min_mid_distance: f32,
    /// Optional JSON cache for extracted meta-calibrator training samples.
    /// Intended for AWS Batch/local sweeps where the train window is fixed.
    pub meta_training_samples_cache: Option<PathBuf>,
    /// Optional frozen meta-calibrator snapshot to load instead of training.
    pub meta_calibrator_snapshot_in: Option<PathBuf>,
    /// Optional path to write the trained meta-calibrator snapshot.
    pub meta_calibrator_snapshot_out: Option<PathBuf>,
    /// If true, error rather than fitting a meta-calibrator without a snapshot.
    pub forbid_meta_training: bool,
    /// Disable to run strategy logic against the hand-crafted model only.
    pub enable_meta_calibration: bool,
    /// In portfolio mode, write partial outputs every N evaluated markets.
    /// Set to zero to disable checkpointing.
    pub portfolio_checkpoint_every_markets: usize,
    /// Optional portfolio-mode per-decision JSONL path.
    pub decision_log_jsonl: Option<PathBuf>,
    /// Log every Nth replay event when `decision_log_jsonl` is set.
    pub decision_log_every_n: usize,
    /// Optional per-market JSONL path used for portfolio checkpoints.
    pub checkpoint_markets_out: Option<PathBuf>,
    /// Optional summary JSON path used for portfolio checkpoints.
    pub checkpoint_summary_out: Option<PathBuf>,
}

impl Default for WalkForwardConfig {
    fn default() -> Self {
        Self {
            starting_cash_usdc: 100.0,
            kelly_fraction: 0.25,
            max_clip_usdc: 20.0,
            max_order_clip_multiplier: 2.0,
            max_per_market_exposure_usdc: 50.0,
            max_per_market_exposure_frac: None,
            spot_symbol: "auto".to_string(),
            strategies: StratId::ACTIVE.to_vec(),
            max_concurrent_fetches: 64,
            replay_sample_ms: 0,
            replay_event_cache_dir: None,
            load_pm_trades: true,
            use_outcome_label: false,
            maker_rebate_bps: 0.0,
            taker_fee_bps: 0.0,
            taker_latency_ms: 0,
            portfolio_mode: false,
            clip_fraction_of_equity: None,
            clip_drawdown_soft_pct: 1.0,
            clip_drawdown_hard_pct: 1.0,
            clip_drawdown_min_multiplier: 0.0,
            clip_session_drawdown_soft_pct: 1.0,
            clip_session_drawdown_hard_pct: 1.0,
            clip_session_drawdown_min_multiplier: 0.0,
            daily_loss_cap_pct: 1.0,
            loss_streak_cooldown_after: 0,
            loss_streak_cooldown_markets: 0,
            loss_streak_loss_threshold_usdc: 0.0,
            br2_disable_internal_model_gates: false,
            br2_participation_clip_frac: 0.0,
            br2_participation_max_pair_cost: 0.99,
            br2_participation_max_orders_per_leg: 500,
            br2_participation_max_inventory_delta_shares: 25.0,
            br2_participation_repair_inventory_delta_shares: 5.0,
            br2_participation_refresh_secs: 0.50,
            br2_participation_stop_secs_before_close: 20.0,
            br2_hedged_base_enabled: false,
            br2_hedged_base_max_secs_in: 240.0,
            br2_hedged_base_max_pair_cost: 0.98,
            br2_hedged_base_min_minority_leg_frac: 0.20,
            br2_hedged_base_clip_usdc: 5.0,
            br2_hedged_base_max_notional_usdc: 0.0,
            br2_late_directional_overlay_frac: 1e9,
            br2_min_composite_direction: 0.10,
            br2_early_clip_frac: 0.00,
            br2_mid_clip_frac: 0.00,
            br2_late_clip_frac: 1.0,
            br2_late_max_fires: 3,
            br2_late_confirm_min_model_confidence: 0.58,
            br2_late_confirm_max_model_risk: 0.80,
            br2_late_confirm_min_model_side_p: 0.58,
            br2_late_confirm_min_model_edge: 0.02,
            br2_late_confirm_min_book_skew: 0.06,
            br2_late_confirm_max_whipsaw_score: 0.85,
            br2_late_confirm_min_realized_vol_180s_bps: 0.0,
            br2_late_confirm_max_observed_range: 1.0,
            br2_recent_regime_gate_enabled: false,
            br2_recent_regime_gate_min_edge: 0.08,
            br2_recent_regime_gate_late_confirm: true,
            br2_recent_regime_gate_high_skew: true,
            br2_recent_regime_gate_late_favourite: true,
            br2_high_skew_clip_frac: 0.60,
            br2_lane_size_late_favourite: 1.0,
            br2_lane_size_late_confirm: 1.0,
            br2_lane_size_high_skew: 1.0,
            br2_regime_gate_enabled: false,
            br2_regime_gate_window: 3,
            br2_regime_gate_threshold: 0.50,
            br2_regime_gate_soft_band: 0.0,
            br2_regime_gate_lane_floor: 0.0,
            br2_regime_gate_whipsaw_weight: 0.0,
            br2_high_skew_max_clips: 5,
            br2_high_skew_max_whipsaw_score: 0.75,
            br2_high_skew_min_realized_vol_180s_bps: 0.0,
            br2_late_favourite_start_secs: 180.0,
            br2_late_favourite_threshold: 0.22,
            br2_late_favourite_min_ask: 0.70,
            br2_late_favourite_max_ask: 0.97,
            br2_late_favourite_clip_frac: 1.00,
            br2_late_favourite_high_cert_clip_frac: 1.00,
            br2_late_favourite_high_cert_full_clip_edge: 0.04,
            br2_late_favourite_fragile_high_cert_ask: 0.923,
            br2_late_favourite_fragile_high_cert_max_edge: 0.005,
            br2_late_favourite_fragile_high_cert_max_path_efficiency: 0.50,
            br2_late_favourite_fragile_high_cert_size_frac: 0.50,
            br2_late_favourite_max_clips: 12,
            br2_late_favourite_min_sustain_secs: 0.0,
            br2_late_favourite_sweep_depth: 7,
            br2_late_favourite_min_model_confidence: 0.68,
            br2_late_favourite_min_model_direction_abs: 0.0,
            br2_late_favourite_max_model_risk: 0.72,
            br2_late_favourite_min_model_side_p: 0.62,
            br2_late_favourite_min_model_edge: 0.03,
            br2_late_favourite_high_cert_min_model_edge: 0.02,
            br2_late_favourite_high_cert_bypass_model_edge: false,
            br2_late_favourite_max_whipsaw_score: 0.75,
            br2_late_favourite_max_reversal_pressure: 1.0,
            br2_late_favourite_min_path_efficiency: 0.0,
            br2_late_favourite_min_realized_vol_180s_bps: 0.0,
            br2_late_favourite_max_observed_range: 1.0,
            br2_late_favourite_range_soft_throttle: 0.78,
            br2_late_favourite_range_hard_throttle: 0.98,
            br2_late_favourite_range_extra_edge: 0.03,
            br2_late_favourite_range_extra_confidence: 0.08,
            br2_late_favourite_max_adverse_fast_momentum: 1.0,
            br2_late_favourite_max_adverse_broad_momentum: 1.0,
            br2_late_favourite_max_entry_pullback: 1.0,
            br2_late_favourite_max_avg_entry_drawdown: 1.0,
            br2_tail_clip_frac: 0.10,
            br2_tail_max_clips: 3,
            br2_tail_sweep_depth: 3,
            br2_tail_min_ask: 0.01,
            br2_tail_max_ask: 0.10,
            br2_tail_min_seconds_to_close: 10.0,
            br2_tail_min_favourite_unrealized_edge: 0.0,
            br2_tail_min_observed_range: 0.0,
            br2_tail_target_favourite_loss_coverage_frac: 0.50,
            br2_tail_reversal_coverage_frac: 0.00,
            br2_tail_reversal_min_seconds_to_close: 10.0,
            br2_tail_reversal_max_seconds_to_close: 35.0,
            br2_tail_reversal_min_favourite_ask: 0.895,
            br2_tail_extreme_threshold: 0.30,
            br2_tail_min_skew_step: 0.02,
            br2_tail_budget_favourite_spend_frac: 0.20,
            br2_tail_budget_favourite_upside_frac: 0.25,
            br2_tail_regime_boost_coverage_frac: 0.0,
            br2_tail_regime_boost_budget_spend_frac: 0.0,
            br2_tail_regime_boost_budget_upside_frac: 0.0,
            br2_tail_regime_boost_min_whipsaw_score: 1.0,
            br2_tail_regime_boost_min_reversal_pressure: 1.0,
            br2_tail_regime_boost_min_realized_vol_180s_bps: 1.0e9,
            br2_tail_regime_boost_max_path_efficiency: -1.0,
            back_to_explore_base_clip_usdc: 7.24,
            back_to_explore_max_clip_usdc: 200.0,
            back_to_explore_min_pair_cost_for_two_sided: 0.96,
            back_to_explore_pair_clip_multiplier: 0.95,
            back_to_explore_directional_strength_mult: 1.6,
            back_to_explore_residual_taper_start_frac: 0.55,
            back_to_explore_residual_min_clip_multiplier: 0.35,
            back_to_explore_range_soft_throttle: 1.0,
            back_to_explore_range_hard_throttle: 1.0,
            back_to_explore_range_min_clip_multiplier: 1.0,
            back_to_explore_range_repair_min_clip_multiplier: 1.0,
            back_to_explore_range_chop_min_range: 1.0,
            back_to_explore_range_clean_path_efficiency: 1.0,
            back_to_explore_range_chop_sign_flip_rate: 1.0,
            back_to_explore_range_reversal_pressure: 1.0,
            back_to_explore_clean_path_directional_clip_multiplier: 1.0,
            back_to_explore_reversal_pressure_clip_multiplier: 1.0,
            back_to_explore_reversal_pressure_directional_min_signal: 1.05,
            back_to_explore_reversal_pressure_directional_min_edge: 0.01,
            back_to_explore_reversal_pressure_directional_clip_multiplier: 0.85,
            back_to_explore_max_residual_shares: 120.0,
            back_to_explore_min_clip_multiplier_to_emit: 0.18,
            back_to_explore_refresh_secs: 2.8,
            back_to_explore_stop_secs_before_close: 4.0,
            back_to_explore_min_entry_price: 0.02,
            back_to_explore_max_entry_price: 0.99,
            back_to_explore_sweep_depth: 6,
            back_to_explore_high_activity_hours: vec![14, 15, 16, 17, 18, 19],
            back_to_explore_base_participation_rate: 0.90,
            back_to_explore_two_sided_preference: 3.5,
            back_to_explore_target_risk_per_clip_frac: 0.0025,
            back_to_explore_base_target_net_shares: 6.0,
            back_to_explore_good_hour_target_net_mult: 3.0,
            back_to_explore_policy_scales_jsonl: None,
            back_to_explore_debug_signals: false,
            br2_reversal_score_enabled: false,
            br2_reversal_score_coeffs_path: None,
            br2_reversal_score_cov_min: f32::NAN,
            br2_reversal_score_cov_max: f32::NAN,
            br2_reversal_score_size_floor: 1.0,
            br2_reversal_score_size_ceiling: 1.0,
            enforce_model_gate: true,
            model_gate_min_confidence: 0.68,
            model_gate_max_risk: 0.72,
            model_gate_min_edge: 0.00,
            model_btc_whipsaw_risk_weight: 0.16,
            model_btc_path_inefficiency_risk_weight: 0.10,
            model_btc_reversal_pressure_risk_weight: 0.12,
            enable_market_context_features: false,
            volatility_regime_threshold: 0.08,
            walk_forward_folds: None,
            fold_size: None,
            purge_markets: 0,
            min_train_markets: 0,
            meta_training_config: MetaTrainingConfig::default(),
            meta_max_fit_samples: DEFAULT_META_MAX_FIT_SAMPLES,
            meta_max_validation_samples: DEFAULT_META_MAX_VALIDATION_SAMPLES,
            meta_max_samples_per_market: DEFAULT_META_MAX_SAMPLES_PER_MARKET,
            meta_max_oos_evaluation_samples: DEFAULT_META_MAX_OOS_EVALUATION_SAMPLES,
            meta_train_min_base_p: 0.0,
            meta_train_max_early_penalty: 1.0,
            meta_train_min_mid_distance: 0.0,
            meta_training_samples_cache: None,
            meta_calibrator_snapshot_in: None,
            meta_calibrator_snapshot_out: None,
            forbid_meta_training: false,
            enable_meta_calibration: true,
            portfolio_checkpoint_every_markets: 0,
            decision_log_jsonl: None,
            decision_log_every_n: 1,
            checkpoint_markets_out: None,
            checkpoint_summary_out: None,
        }
    }
}

impl WalkForwardConfig {
    fn build_back_to_explore_config(
        &self,
        clip: f64,
        market_window_ns: i64,
        external_risk_multiplier: f64,
    ) -> BackToExploreConfig {
        let base_clip_usdc = (clip.max(self.back_to_explore_base_clip_usdc).max(4.0))
            .min(self.back_to_explore_max_clip_usdc);
        BackToExploreConfig {
            base_clip_usdc,
            max_clip_usdc: self.back_to_explore_max_clip_usdc,
            min_pair_cost_for_two_sided: self.back_to_explore_min_pair_cost_for_two_sided,
            pair_clip_multiplier: self.back_to_explore_pair_clip_multiplier,
            directional_strength_mult: self.back_to_explore_directional_strength_mult,
            residual_taper_start_frac: self.back_to_explore_residual_taper_start_frac,
            residual_min_clip_multiplier: self.back_to_explore_residual_min_clip_multiplier,
            range_soft_throttle: self.back_to_explore_range_soft_throttle,
            range_hard_throttle: self.back_to_explore_range_hard_throttle,
            range_min_clip_multiplier: self.back_to_explore_range_min_clip_multiplier,
            range_repair_min_clip_multiplier: self.back_to_explore_range_repair_min_clip_multiplier,
            range_chop_min_range: self.back_to_explore_range_chop_min_range,
            range_clean_path_efficiency: self.back_to_explore_range_clean_path_efficiency,
            range_chop_sign_flip_rate: self.back_to_explore_range_chop_sign_flip_rate,
            range_reversal_pressure: self.back_to_explore_range_reversal_pressure,
            clean_path_directional_clip_multiplier: self
                .back_to_explore_clean_path_directional_clip_multiplier,
            reversal_pressure_clip_multiplier: self
                .back_to_explore_reversal_pressure_clip_multiplier,
            reversal_pressure_directional_min_signal: self
                .back_to_explore_reversal_pressure_directional_min_signal,
            reversal_pressure_directional_min_edge: self
                .back_to_explore_reversal_pressure_directional_min_edge,
            reversal_pressure_directional_clip_multiplier: self
                .back_to_explore_reversal_pressure_directional_clip_multiplier,
            max_residual_shares: self.back_to_explore_max_residual_shares,
            min_clip_multiplier_to_emit: self.back_to_explore_min_clip_multiplier_to_emit,
            refresh_secs: self.back_to_explore_refresh_secs,
            stop_secs_before_close: self.back_to_explore_stop_secs_before_close,
            min_entry_price: self.back_to_explore_min_entry_price,
            max_entry_price: self.back_to_explore_max_entry_price,
            sweep_depth: self.back_to_explore_sweep_depth,
            market_window_ns,
            high_activity_hours: self.back_to_explore_high_activity_hours.clone(),
            base_participation_rate: self.back_to_explore_base_participation_rate,
            two_sided_preference: self.back_to_explore_two_sided_preference,
            target_risk_per_clip_frac: self.back_to_explore_target_risk_per_clip_frac,
            base_target_net_shares: self.back_to_explore_base_target_net_shares,
            good_hour_target_net_mult: self.back_to_explore_good_hour_target_net_mult,
            external_risk_multiplier,
            debug_signals: self.back_to_explore_debug_signals,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MarketResult {
    pub asset_id: String,
    pub slug: String,
    pub close_ts: i64,
    pub outcome_label: String,
    pub volatility_range: f64,
    pub volatility_band: VolatilityBand,
    pub per_strategy: HashMap<&'static str, StrategyMarketResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyMarketResult {
    pub orders_submitted: usize,
    pub orders_filled: usize,
    pub orders_filled_taker: usize,
    pub orders_filled_maker: usize,
    pub orders_rejected_model_gate: usize,
    pub orders_rejected_model_gate_confidence: usize,
    pub orders_rejected_model_gate_risk: usize,
    pub orders_rejected_model_gate_edge: usize,
    pub pnl_usdc: f64,
    pub start_equity_usdc: f64,
    pub end_equity_usdc: f64,
    pub max_drawdown_pct: f64,
    pub fills: usize,
    pub maker_rebates_usdc: f64,
    pub requested_shares: f64,
    pub filled_shares: f64,
    pub fill_shares_ratio: f64,
    pub requested_notional_usdc: f64,
    pub filled_notional_usdc: f64,
    pub fill_notional_ratio: f64,
    pub avg_slippage_bps: f64,
    pub clip_used_usdc: f64,
    pub yes_resolved: bool,
    /// Per-fill detail: ts, side, shares, price, notional, tag, maker, rebate.
    /// Empty if `fills_count == 0`. Use sparingly for large runs (per-market
    /// rows can grow large).
    pub fills_detail: Vec<crate::runner::Fill>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bonereaper_v2_gate_stats: Option<BonereaperV2GateStats>,
    #[serde(skip_serializing)]
    pub model_training_samples: Vec<MetaTrainingSample>,
}

fn market_duration_secs_from_slug(slug: &str) -> i64 {
    let slug = slug.to_ascii_lowercase();
    if slug.contains("-updown-15m-") {
        900
    } else if slug.contains("-updown-4h-") {
        14_400
    } else {
        300
    }
}

fn market_open_ts(m: &MarketHandle) -> i64 {
    parse_close_ts(&m.slug).unwrap_or_else(|| {
        m.close_ts
            .saturating_sub(market_duration_secs_from_slug(&m.slug))
    })
}

fn market_close_ts(m: &MarketHandle) -> i64 {
    let open_ts = market_open_ts(m);
    let duration_secs = market_duration_secs_from_slug(&m.slug);
    if m.close_ts <= open_ts {
        open_ts.saturating_add(duration_secs)
    } else {
        m.close_ts
    }
}

fn market_open_ns(m: &MarketHandle) -> i64 {
    market_open_ts(m).saturating_mul(1_000_000_000)
}

fn market_close_ns(m: &MarketHandle) -> i64 {
    market_close_ts(m).saturating_mul(1_000_000_000)
}

type BtePolicyScaleMap = HashMap<(String, i64), f64>;

#[derive(Debug, Deserialize)]
struct BtePolicyScaleRow {
    slug: String,
    close_ts: i64,
    scale: f64,
}

fn load_bte_policy_scales(path: &Path) -> Result<BtePolicyScaleMap> {
    let file = File::open(path)
        .with_context(|| format!("failed to open BTE policy scale file {}", path.display()))?;
    let mut out = HashMap::new();
    for (line_idx, line) in BufReader::new(file).lines().enumerate() {
        let line = line.with_context(|| {
            format!(
                "failed to read line {} from BTE policy scale file {}",
                line_idx + 1,
                path.display()
            )
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let row: BtePolicyScaleRow = serde_json::from_str(&line).with_context(|| {
            format!(
                "failed to parse line {} from BTE policy scale file {}",
                line_idx + 1,
                path.display()
            )
        })?;
        out.insert((row.slug, row.close_ts), row.scale.clamp(0.0, 3.0));
    }
    Ok(out)
}

fn bte_policy_scale_for(scales: Option<&BtePolicyScaleMap>, market: &MarketHandle) -> f64 {
    scales
        .and_then(|map| {
            map.get(&(market.slug.clone(), market_close_ts(market)))
                .copied()
        })
        .unwrap_or(1.0)
}

fn outcome_label_resolved_yes(outcome: &str) -> Option<bool> {
    if outcome.eq_ignore_ascii_case("up") || outcome.eq_ignore_ascii_case("yes") {
        Some(true)
    } else if outcome.eq_ignore_ascii_case("down") || outcome.eq_ignore_ascii_case("no") {
        Some(false)
    } else {
        None
    }
}

pub(crate) fn validate_outcome_labels(markets: &[MarketHandle]) -> Result<()> {
    let invalid = markets
        .iter()
        .find(|market| outcome_label_resolved_yes(&market.outcome).is_none());
    if let Some(market) = invalid {
        return Err(anyhow!(
            "--use-outcome-label requires explicit Up/Down outcome labels; market {} has outcome={:?}",
            market.slug,
            market.outcome
        ));
    }
    Ok(())
}

fn prior_market_range_mean(results: &[MarketResult], window: usize) -> f32 {
    if window == 0 || results.is_empty() {
        return 0.0;
    }
    let start = results.len().saturating_sub(window);
    let slice = &results[start..];
    if slice.is_empty() {
        return 0.0;
    }
    (slice
        .iter()
        .map(|result| result.volatility_range)
        .sum::<f64>()
        / slice.len() as f64) as f32
}

#[derive(Debug, Clone, Serialize)]
pub struct WalkForwardSummary {
    pub markets_attempted: usize,
    pub markets_succeeded: usize,
    /// Key runtime controls used to produce this summary.
    pub run_config: Option<SummaryRunConfig>,
    /// Overall aggregate for all markets.
    pub per_strategy: HashMap<&'static str, StrategyAggregate>,
    /// Aggregate split by volatility regime (Low/High).
    pub by_volatility_band: HashMap<VolatilityBand, HashMap<&'static str, StrategyAggregate>>,
    /// Per-fold summaries when walk-forward mode is enabled.
    pub fold_summaries: Vec<WalkForwardFoldSummary>,
    /// Meta-calibrator training/evaluation evidence for train-once portfolio
    /// runs. Empty for legacy independent-market runs without ML training.
    pub meta_calibration: Option<MetaCalibrationReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryRunConfig {
    /// Controls shared by every strategy at runtime.
    pub shared: SharedRunConfig,
    /// Strategy-specific runtime knobs included only for strategies included in
    /// the requested strategy set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub strategies: Vec<StrategyRunConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedRunConfig {
    pub starting_cash_usdc: f64,
    pub kelly_fraction: f64,
    pub max_clip_usdc: f64,
    pub max_order_clip_multiplier: f64,
    pub max_per_market_exposure_usdc: f64,
    pub max_per_market_exposure_frac: Option<f64>,
    pub spot_symbol_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spot_symbol_override: Option<String>,
    pub portfolio_mode: bool,
    pub max_concurrent_fetches: usize,
    pub replay_sample_ms: u64,
    pub taker_latency_ms: u64,
    pub replay_event_cache_dir: Option<String>,
    pub load_pm_trades: bool,
    pub clip_fraction_of_equity: Option<f64>,
    pub clip_drawdown_soft_pct: f64,
    pub clip_drawdown_hard_pct: f64,
    pub clip_drawdown_min_multiplier: f64,
    pub clip_session_drawdown_soft_pct: f64,
    pub clip_session_drawdown_hard_pct: f64,
    pub clip_session_drawdown_min_multiplier: f64,
    pub daily_loss_cap_pct: f64,
    pub loss_streak_cooldown_after: usize,
    pub loss_streak_cooldown_markets: usize,
    pub loss_streak_loss_threshold_usdc: f64,
    pub enforce_model_gate: bool,
    pub model_gate_min_confidence: f32,
    pub model_gate_max_risk: f32,
    pub model_gate_min_edge: f32,
    #[serde(rename = "model_spot_whipsaw_risk_weight")]
    pub model_btc_whipsaw_risk_weight: f32,
    #[serde(rename = "model_spot_path_inefficiency_risk_weight")]
    pub model_btc_path_inefficiency_risk_weight: f32,
    #[serde(rename = "model_spot_reversal_pressure_risk_weight")]
    pub model_btc_reversal_pressure_risk_weight: f32,
    pub enable_market_context_features: bool,
    pub volatility_regime_threshold: f64,
    pub walk_forward_folds: Option<usize>,
    pub fold_size: Option<usize>,
    pub purge_markets: usize,
    pub use_outcome_label: bool,
    pub maker_rebate_bps: f64,
    pub taker_fee_bps: f64,
    pub maker_rebates: Option<f64>,
    pub decision_log_every_n: usize,
    pub portfolio_checkpoint_every_markets: usize,
    pub decision_log_jsonl: Option<String>,
    pub checkpoint_markets_out: Option<String>,
    pub checkpoint_summary_out: Option<String>,
    pub meta_max_fit_samples: usize,
    pub meta_max_validation_samples: usize,
    pub meta_max_samples_per_market: usize,
    pub meta_max_oos_evaluation_samples: usize,
    pub meta_train_min_base_p: f32,
    pub meta_train_max_early_penalty: f32,
    pub meta_train_min_mid_distance: f32,
    pub min_train_markets: usize,
    pub meta_training_config: MetaTrainingConfig,
    pub meta_training_samples_cache: Option<String>,
    pub meta_calibrator_snapshot_in: Option<String>,
    pub meta_calibrator_snapshot_out: Option<String>,
    pub enable_meta_calibration: bool,
    pub forbid_meta_training: bool,
}

impl From<&WalkForwardConfig> for SharedRunConfig {
    fn from(cfg: &WalkForwardConfig) -> Self {
        Self {
            starting_cash_usdc: cfg.starting_cash_usdc,
            kelly_fraction: cfg.kelly_fraction,
            max_clip_usdc: cfg.max_clip_usdc,
            max_order_clip_multiplier: cfg.max_order_clip_multiplier,
            max_per_market_exposure_usdc: cfg.max_per_market_exposure_usdc,
            max_per_market_exposure_frac: cfg.max_per_market_exposure_frac,
            spot_symbol_mode: spot_symbol_mode(&cfg.spot_symbol).to_string(),
            spot_symbol_override: spot_symbol_override(&cfg.spot_symbol),
            portfolio_mode: cfg.portfolio_mode,
            max_concurrent_fetches: cfg.max_concurrent_fetches,
            replay_sample_ms: cfg.replay_sample_ms,
            taker_latency_ms: cfg.taker_latency_ms,
            replay_event_cache_dir: cfg
                .replay_event_cache_dir
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            load_pm_trades: cfg.load_pm_trades,
            clip_fraction_of_equity: cfg.clip_fraction_of_equity,
            clip_drawdown_soft_pct: cfg.clip_drawdown_soft_pct,
            clip_drawdown_hard_pct: cfg.clip_drawdown_hard_pct,
            clip_drawdown_min_multiplier: cfg.clip_drawdown_min_multiplier,
            clip_session_drawdown_soft_pct: cfg.clip_session_drawdown_soft_pct,
            clip_session_drawdown_hard_pct: cfg.clip_session_drawdown_hard_pct,
            clip_session_drawdown_min_multiplier: cfg.clip_session_drawdown_min_multiplier,
            daily_loss_cap_pct: cfg.daily_loss_cap_pct,
            loss_streak_cooldown_after: cfg.loss_streak_cooldown_after,
            loss_streak_cooldown_markets: cfg.loss_streak_cooldown_markets,
            loss_streak_loss_threshold_usdc: cfg.loss_streak_loss_threshold_usdc,
            enforce_model_gate: cfg.enforce_model_gate,
            model_gate_min_confidence: cfg.model_gate_min_confidence,
            model_gate_max_risk: cfg.model_gate_max_risk,
            model_gate_min_edge: cfg.model_gate_min_edge,
            model_btc_whipsaw_risk_weight: cfg.model_btc_whipsaw_risk_weight,
            model_btc_path_inefficiency_risk_weight: cfg.model_btc_path_inefficiency_risk_weight,
            model_btc_reversal_pressure_risk_weight: cfg.model_btc_reversal_pressure_risk_weight,
            enable_market_context_features: cfg.enable_market_context_features,
            volatility_regime_threshold: cfg.volatility_regime_threshold,
            walk_forward_folds: cfg.walk_forward_folds,
            fold_size: cfg.fold_size,
            purge_markets: cfg.purge_markets,
            meta_max_fit_samples: cfg.meta_max_fit_samples,
            meta_max_validation_samples: cfg.meta_max_validation_samples,
            meta_max_samples_per_market: cfg.meta_max_samples_per_market,
            meta_max_oos_evaluation_samples: cfg.meta_max_oos_evaluation_samples,
            meta_train_min_base_p: cfg.meta_train_min_base_p,
            meta_train_max_early_penalty: cfg.meta_train_max_early_penalty,
            meta_train_min_mid_distance: cfg.meta_train_min_mid_distance,
            min_train_markets: cfg.min_train_markets,
            meta_training_config: cfg.meta_training_config,
            meta_training_samples_cache: cfg
                .meta_training_samples_cache
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            meta_calibrator_snapshot_in: cfg
                .meta_calibrator_snapshot_in
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            meta_calibrator_snapshot_out: cfg
                .meta_calibrator_snapshot_out
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            enable_meta_calibration: cfg.enable_meta_calibration,
            forbid_meta_training: cfg.forbid_meta_training,
            decision_log_every_n: cfg.decision_log_every_n,
            portfolio_checkpoint_every_markets: cfg.portfolio_checkpoint_every_markets,
            decision_log_jsonl: cfg
                .decision_log_jsonl
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            checkpoint_markets_out: cfg
                .checkpoint_markets_out
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            checkpoint_summary_out: cfg
                .checkpoint_summary_out
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            use_outcome_label: cfg.use_outcome_label,
            maker_rebate_bps: cfg.maker_rebate_bps,
            taker_fee_bps: cfg.taker_fee_bps,
            maker_rebates: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BonereaperV2SummaryConfig {
    pub disable_internal_model_gates: bool,
    pub participation_clip_frac: f32,
    pub participation_max_pair_cost: f32,
    pub participation_max_orders_per_leg: usize,
    pub participation_max_inventory_delta_shares: f64,
    pub participation_repair_inventory_delta_shares: f64,
    pub participation_refresh_secs: f32,
    pub participation_stop_secs_before_close: f32,
    pub hedged_base_enabled: bool,
    pub hedged_base_max_secs_in: f32,
    pub hedged_base_max_pair_cost: f32,
    pub hedged_base_min_minority_leg_frac: f32,
    pub hedged_base_clip_usdc: f32,
    pub hedged_base_max_notional_usdc: f32,
    pub late_directional_overlay_frac: f32,
    pub min_composite_direction: f32,
    pub early_clip_frac: f32,
    pub mid_clip_frac: f32,
    pub late_clip_frac: f32,
    pub late_max_fires: usize,
    pub late_confirm_min_model_confidence: f32,
    pub late_confirm_max_model_risk: f32,
    pub late_confirm_min_model_side_p: f32,
    pub late_confirm_min_model_edge: f32,
    pub late_confirm_min_book_skew: f32,
    pub late_confirm_max_whipsaw_score: f32,
    pub late_confirm_min_realized_vol_180s_bps: f32,
    pub late_confirm_max_observed_range: f32,
    pub recent_regime_gate_enabled: bool,
    pub recent_regime_gate_min_edge: f32,
    pub recent_regime_gate_late_confirm: bool,
    pub recent_regime_gate_high_skew: bool,
    pub recent_regime_gate_late_favourite: bool,
    pub high_skew_clip_frac: f32,
    pub lane_size_late_favourite: f32,
    pub lane_size_late_confirm: f32,
    pub lane_size_high_skew: f32,
    pub regime_gate_enabled: bool,
    pub regime_gate_window: u8,
    pub regime_gate_threshold: f32,
    pub regime_gate_soft_band: f32,
    pub regime_gate_lane_floor: f32,
    pub regime_gate_whipsaw_weight: f32,
    pub high_skew_max_clips: usize,
    pub high_skew_max_whipsaw_score: f32,
    pub high_skew_min_realized_vol_180s_bps: f32,
    pub late_favourite_start_secs: f32,
    pub late_favourite_threshold: f32,
    pub late_favourite_min_ask: f32,
    pub late_favourite_max_ask: f32,
    pub late_favourite_clip_frac: f32,
    pub late_favourite_high_cert_clip_frac: f32,
    pub late_favourite_high_cert_full_clip_edge: f32,
    pub late_favourite_fragile_high_cert_ask: f32,
    pub late_favourite_fragile_high_cert_max_edge: f32,
    pub late_favourite_fragile_high_cert_max_path_efficiency: f32,
    pub late_favourite_fragile_high_cert_size_frac: f32,
    pub late_favourite_max_clips: usize,
    pub late_favourite_min_sustain_secs: f32,
    pub late_favourite_sweep_depth: usize,
    pub late_favourite_min_model_confidence: f32,
    pub late_favourite_min_model_direction_abs: f32,
    pub late_favourite_max_model_risk: f32,
    pub late_favourite_min_model_side_p: f32,
    pub late_favourite_min_model_edge: f32,
    pub late_favourite_high_cert_min_model_edge: f32,
    pub late_favourite_high_cert_bypass_model_edge: bool,
    pub late_favourite_max_whipsaw_score: f32,
    pub late_favourite_max_reversal_pressure: f32,
    pub late_favourite_min_path_efficiency: f32,
    pub late_favourite_min_realized_vol_180s_bps: f32,
    pub late_favourite_max_observed_range: f32,
    pub late_favourite_range_soft_throttle: f32,
    pub late_favourite_range_hard_throttle: f32,
    pub late_favourite_range_extra_edge: f32,
    pub late_favourite_range_extra_confidence: f32,
    pub late_favourite_max_adverse_fast_momentum: f32,
    pub late_favourite_max_adverse_broad_momentum: f32,
    pub late_favourite_max_entry_pullback: f32,
    pub late_favourite_max_avg_entry_drawdown: f32,
    pub tail_clip_frac: f32,
    pub tail_max_clips: usize,
    pub tail_sweep_depth: usize,
    pub tail_min_ask: f32,
    pub tail_max_ask: f32,
    pub tail_min_seconds_to_close: f32,
    pub tail_min_favourite_unrealized_edge: f32,
    pub tail_min_observed_range: f32,
    pub tail_target_favourite_loss_coverage_frac: f32,
    pub tail_reversal_coverage_frac: f32,
    pub tail_reversal_min_seconds_to_close: f32,
    pub tail_reversal_max_seconds_to_close: f32,
    pub tail_reversal_min_favourite_ask: f32,
    pub tail_extreme_threshold: f32,
    pub tail_min_skew_step: f32,
    pub tail_budget_favourite_spend_frac: f32,
    pub tail_budget_favourite_upside_frac: f32,
    pub tail_regime_boost_coverage_frac: f32,
    pub tail_regime_boost_budget_spend_frac: f32,
    pub tail_regime_boost_budget_upside_frac: f32,
    pub tail_regime_boost_min_whipsaw_score: f32,
    pub tail_regime_boost_min_reversal_pressure: f32,
    pub tail_regime_boost_min_realized_vol_180s_bps: f32,
    pub tail_regime_boost_max_path_efficiency: f32,
    pub reversal_score_enabled: bool,
    pub reversal_score_cov_min: f32,
    pub reversal_score_cov_max: f32,
    pub reversal_score_size_floor: f32,
    pub reversal_score_size_ceiling: f32,
    pub reversal_score_coeffs_path: Option<String>,
}

impl From<&WalkForwardConfig> for BonereaperV2SummaryConfig {
    fn from(cfg: &WalkForwardConfig) -> Self {
        Self {
            disable_internal_model_gates: cfg.br2_disable_internal_model_gates,
            participation_clip_frac: cfg.br2_participation_clip_frac,
            participation_max_pair_cost: cfg.br2_participation_max_pair_cost,
            participation_max_orders_per_leg: cfg.br2_participation_max_orders_per_leg,
            participation_max_inventory_delta_shares: cfg
                .br2_participation_max_inventory_delta_shares,
            participation_repair_inventory_delta_shares: cfg
                .br2_participation_repair_inventory_delta_shares,
            participation_refresh_secs: cfg.br2_participation_refresh_secs,
            participation_stop_secs_before_close: cfg.br2_participation_stop_secs_before_close,
            hedged_base_enabled: cfg.br2_hedged_base_enabled,
            hedged_base_max_secs_in: cfg.br2_hedged_base_max_secs_in,
            hedged_base_max_pair_cost: cfg.br2_hedged_base_max_pair_cost,
            hedged_base_min_minority_leg_frac: cfg.br2_hedged_base_min_minority_leg_frac,
            hedged_base_clip_usdc: cfg.br2_hedged_base_clip_usdc,
            hedged_base_max_notional_usdc: cfg.br2_hedged_base_max_notional_usdc,
            late_directional_overlay_frac: cfg.br2_late_directional_overlay_frac,
            min_composite_direction: cfg.br2_min_composite_direction,
            early_clip_frac: cfg.br2_early_clip_frac,
            mid_clip_frac: cfg.br2_mid_clip_frac,
            late_clip_frac: cfg.br2_late_clip_frac,
            late_max_fires: cfg.br2_late_max_fires,
            late_confirm_min_model_confidence: cfg.br2_late_confirm_min_model_confidence,
            late_confirm_max_model_risk: cfg.br2_late_confirm_max_model_risk,
            late_confirm_min_model_side_p: cfg.br2_late_confirm_min_model_side_p,
            late_confirm_min_model_edge: cfg.br2_late_confirm_min_model_edge,
            late_confirm_min_book_skew: cfg.br2_late_confirm_min_book_skew,
            late_confirm_max_whipsaw_score: cfg.br2_late_confirm_max_whipsaw_score,
            late_confirm_min_realized_vol_180s_bps: cfg.br2_late_confirm_min_realized_vol_180s_bps,
            late_confirm_max_observed_range: cfg.br2_late_confirm_max_observed_range,
            recent_regime_gate_enabled: cfg.br2_recent_regime_gate_enabled,
            recent_regime_gate_min_edge: cfg.br2_recent_regime_gate_min_edge,
            recent_regime_gate_late_confirm: cfg.br2_recent_regime_gate_late_confirm,
            recent_regime_gate_high_skew: cfg.br2_recent_regime_gate_high_skew,
            recent_regime_gate_late_favourite: cfg.br2_recent_regime_gate_late_favourite,
            high_skew_clip_frac: cfg.br2_high_skew_clip_frac,
            lane_size_late_favourite: cfg.br2_lane_size_late_favourite,
            lane_size_late_confirm: cfg.br2_lane_size_late_confirm,
            lane_size_high_skew: cfg.br2_lane_size_high_skew,
            regime_gate_enabled: cfg.br2_regime_gate_enabled,
            regime_gate_window: cfg.br2_regime_gate_window,
            regime_gate_threshold: cfg.br2_regime_gate_threshold,
            regime_gate_soft_band: cfg.br2_regime_gate_soft_band,
            regime_gate_lane_floor: cfg.br2_regime_gate_lane_floor,
            regime_gate_whipsaw_weight: cfg.br2_regime_gate_whipsaw_weight,
            high_skew_max_clips: cfg.br2_high_skew_max_clips,
            high_skew_max_whipsaw_score: cfg.br2_high_skew_max_whipsaw_score,
            high_skew_min_realized_vol_180s_bps: cfg.br2_high_skew_min_realized_vol_180s_bps,
            late_favourite_start_secs: cfg.br2_late_favourite_start_secs,
            late_favourite_threshold: cfg.br2_late_favourite_threshold,
            late_favourite_min_ask: cfg.br2_late_favourite_min_ask,
            late_favourite_max_ask: cfg.br2_late_favourite_max_ask,
            late_favourite_clip_frac: cfg.br2_late_favourite_clip_frac,
            late_favourite_high_cert_clip_frac: cfg.br2_late_favourite_high_cert_clip_frac,
            late_favourite_high_cert_full_clip_edge: cfg
                .br2_late_favourite_high_cert_full_clip_edge,
            late_favourite_fragile_high_cert_ask: cfg.br2_late_favourite_fragile_high_cert_ask,
            late_favourite_fragile_high_cert_max_edge: cfg
                .br2_late_favourite_fragile_high_cert_max_edge,
            late_favourite_fragile_high_cert_max_path_efficiency: cfg
                .br2_late_favourite_fragile_high_cert_max_path_efficiency,
            late_favourite_fragile_high_cert_size_frac: cfg
                .br2_late_favourite_fragile_high_cert_size_frac,
            late_favourite_max_clips: cfg.br2_late_favourite_max_clips,
            late_favourite_min_sustain_secs: cfg.br2_late_favourite_min_sustain_secs,
            late_favourite_sweep_depth: cfg.br2_late_favourite_sweep_depth,
            late_favourite_min_model_confidence: cfg.br2_late_favourite_min_model_confidence,
            late_favourite_min_model_direction_abs: cfg.br2_late_favourite_min_model_direction_abs,
            late_favourite_max_model_risk: cfg.br2_late_favourite_max_model_risk,
            late_favourite_min_model_side_p: cfg.br2_late_favourite_min_model_side_p,
            late_favourite_min_model_edge: cfg.br2_late_favourite_min_model_edge,
            late_favourite_high_cert_min_model_edge: cfg
                .br2_late_favourite_high_cert_min_model_edge,
            late_favourite_high_cert_bypass_model_edge: cfg
                .br2_late_favourite_high_cert_bypass_model_edge,
            late_favourite_max_whipsaw_score: cfg.br2_late_favourite_max_whipsaw_score,
            late_favourite_max_reversal_pressure: cfg.br2_late_favourite_max_reversal_pressure,
            late_favourite_min_path_efficiency: cfg.br2_late_favourite_min_path_efficiency,
            late_favourite_min_realized_vol_180s_bps: cfg
                .br2_late_favourite_min_realized_vol_180s_bps,
            late_favourite_max_observed_range: cfg.br2_late_favourite_max_observed_range,
            late_favourite_range_soft_throttle: cfg.br2_late_favourite_range_soft_throttle,
            late_favourite_range_hard_throttle: cfg.br2_late_favourite_range_hard_throttle,
            late_favourite_range_extra_edge: cfg.br2_late_favourite_range_extra_edge,
            late_favourite_range_extra_confidence: cfg.br2_late_favourite_range_extra_confidence,
            late_favourite_max_adverse_fast_momentum: cfg
                .br2_late_favourite_max_adverse_fast_momentum,
            late_favourite_max_adverse_broad_momentum: cfg
                .br2_late_favourite_max_adverse_broad_momentum,
            late_favourite_max_entry_pullback: cfg.br2_late_favourite_max_entry_pullback,
            late_favourite_max_avg_entry_drawdown: cfg.br2_late_favourite_max_avg_entry_drawdown,
            tail_clip_frac: cfg.br2_tail_clip_frac,
            tail_max_clips: cfg.br2_tail_max_clips,
            tail_sweep_depth: cfg.br2_tail_sweep_depth,
            tail_min_ask: cfg.br2_tail_min_ask,
            tail_max_ask: cfg.br2_tail_max_ask,
            tail_min_seconds_to_close: cfg.br2_tail_min_seconds_to_close,
            tail_min_favourite_unrealized_edge: cfg.br2_tail_min_favourite_unrealized_edge,
            tail_min_observed_range: cfg.br2_tail_min_observed_range,
            tail_target_favourite_loss_coverage_frac: cfg
                .br2_tail_target_favourite_loss_coverage_frac,
            tail_reversal_coverage_frac: cfg.br2_tail_reversal_coverage_frac,
            tail_reversal_min_seconds_to_close: cfg.br2_tail_reversal_min_seconds_to_close,
            tail_reversal_max_seconds_to_close: cfg.br2_tail_reversal_max_seconds_to_close,
            tail_reversal_min_favourite_ask: cfg.br2_tail_reversal_min_favourite_ask,
            tail_extreme_threshold: cfg.br2_tail_extreme_threshold,
            tail_min_skew_step: cfg.br2_tail_min_skew_step,
            tail_budget_favourite_spend_frac: cfg.br2_tail_budget_favourite_spend_frac,
            tail_budget_favourite_upside_frac: cfg.br2_tail_budget_favourite_upside_frac,
            tail_regime_boost_coverage_frac: cfg.br2_tail_regime_boost_coverage_frac,
            tail_regime_boost_budget_spend_frac: cfg.br2_tail_regime_boost_budget_spend_frac,
            tail_regime_boost_budget_upside_frac: cfg.br2_tail_regime_boost_budget_upside_frac,
            tail_regime_boost_min_whipsaw_score: cfg.br2_tail_regime_boost_min_whipsaw_score,
            tail_regime_boost_min_reversal_pressure: cfg
                .br2_tail_regime_boost_min_reversal_pressure,
            tail_regime_boost_min_realized_vol_180s_bps: cfg
                .br2_tail_regime_boost_min_realized_vol_180s_bps,
            tail_regime_boost_max_path_efficiency: cfg.br2_tail_regime_boost_max_path_efficiency,
            reversal_score_enabled: cfg.br2_reversal_score_enabled,
            reversal_score_cov_min: cfg.br2_reversal_score_cov_min,
            reversal_score_cov_max: cfg.br2_reversal_score_cov_max,
            reversal_score_size_floor: cfg.br2_reversal_score_size_floor,
            reversal_score_size_ceiling: cfg.br2_reversal_score_size_ceiling,
            reversal_score_coeffs_path: cfg
                .br2_reversal_score_coeffs_path
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
        }
    }
}

fn spot_symbol_mode(configured: &str) -> &'static str {
    if configured.is_empty() {
        "disabled"
    } else if configured.eq_ignore_ascii_case("auto") {
        "auto"
    } else {
        "override"
    }
}

fn spot_symbol_override(configured: &str) -> Option<String> {
    if spot_symbol_mode(configured) == "override" {
        Some(configured.to_string())
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StrategyRunConfig {
    pub strategy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackToExploreSummaryConfig {
    pub base_clip_usdc: f64,
    pub max_clip_usdc: f64,
    pub min_pair_cost_for_two_sided: f64,
    pub pair_clip_multiplier: f64,
    pub directional_strength_mult: f64,
    pub residual_taper_start_frac: f64,
    pub residual_min_clip_multiplier: f64,
    pub range_soft_throttle: f32,
    pub range_hard_throttle: f32,
    pub range_min_clip_multiplier: f64,
    pub range_repair_min_clip_multiplier: f64,
    pub range_chop_min_range: f32,
    pub range_clean_path_efficiency: f32,
    pub range_chop_sign_flip_rate: f32,
    pub range_reversal_pressure: f32,
    pub clean_path_directional_clip_multiplier: f64,
    pub reversal_pressure_clip_multiplier: f64,
    pub reversal_pressure_directional_min_signal: f64,
    pub reversal_pressure_directional_min_edge: f64,
    pub reversal_pressure_directional_clip_multiplier: f64,
    pub max_residual_shares: f64,
    pub min_clip_multiplier_to_emit: f64,
    pub refresh_secs: f64,
    pub stop_secs_before_close: f32,
    pub min_entry_price: f32,
    pub max_entry_price: f32,
    pub sweep_depth: usize,
    pub high_activity_hours: Vec<u8>,
    pub base_participation_rate: f64,
    pub two_sided_preference: f64,
    pub target_risk_per_clip_frac: f64,
    pub base_target_net_shares: f64,
    pub good_hour_target_net_mult: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policy_scales_jsonl: Option<String>,
    pub debug_signals: bool,
}

impl From<&WalkForwardConfig> for BackToExploreSummaryConfig {
    fn from(cfg: &WalkForwardConfig) -> Self {
        Self {
            base_clip_usdc: cfg.back_to_explore_base_clip_usdc,
            max_clip_usdc: cfg.back_to_explore_max_clip_usdc,
            min_pair_cost_for_two_sided: cfg.back_to_explore_min_pair_cost_for_two_sided,
            pair_clip_multiplier: cfg.back_to_explore_pair_clip_multiplier,
            directional_strength_mult: cfg.back_to_explore_directional_strength_mult,
            residual_taper_start_frac: cfg.back_to_explore_residual_taper_start_frac,
            residual_min_clip_multiplier: cfg.back_to_explore_residual_min_clip_multiplier,
            range_soft_throttle: cfg.back_to_explore_range_soft_throttle,
            range_hard_throttle: cfg.back_to_explore_range_hard_throttle,
            range_min_clip_multiplier: cfg.back_to_explore_range_min_clip_multiplier,
            range_repair_min_clip_multiplier: cfg.back_to_explore_range_repair_min_clip_multiplier,
            range_chop_min_range: cfg.back_to_explore_range_chop_min_range,
            range_clean_path_efficiency: cfg.back_to_explore_range_clean_path_efficiency,
            range_chop_sign_flip_rate: cfg.back_to_explore_range_chop_sign_flip_rate,
            range_reversal_pressure: cfg.back_to_explore_range_reversal_pressure,
            clean_path_directional_clip_multiplier: cfg
                .back_to_explore_clean_path_directional_clip_multiplier,
            reversal_pressure_clip_multiplier: cfg
                .back_to_explore_reversal_pressure_clip_multiplier,
            reversal_pressure_directional_min_signal: cfg
                .back_to_explore_reversal_pressure_directional_min_signal,
            reversal_pressure_directional_min_edge: cfg
                .back_to_explore_reversal_pressure_directional_min_edge,
            reversal_pressure_directional_clip_multiplier: cfg
                .back_to_explore_reversal_pressure_directional_clip_multiplier,
            max_residual_shares: cfg.back_to_explore_max_residual_shares,
            min_clip_multiplier_to_emit: cfg.back_to_explore_min_clip_multiplier_to_emit,
            refresh_secs: cfg.back_to_explore_refresh_secs,
            stop_secs_before_close: cfg.back_to_explore_stop_secs_before_close,
            min_entry_price: cfg.back_to_explore_min_entry_price,
            max_entry_price: cfg.back_to_explore_max_entry_price,
            sweep_depth: cfg.back_to_explore_sweep_depth,
            high_activity_hours: cfg.back_to_explore_high_activity_hours.clone(),
            base_participation_rate: cfg.back_to_explore_base_participation_rate,
            two_sided_preference: cfg.back_to_explore_two_sided_preference,
            target_risk_per_clip_frac: cfg.back_to_explore_target_risk_per_clip_frac,
            base_target_net_shares: cfg.back_to_explore_base_target_net_shares,
            good_hour_target_net_mult: cfg.back_to_explore_good_hour_target_net_mult,
            policy_scales_jsonl: cfg
                .back_to_explore_policy_scales_jsonl
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            debug_signals: cfg.back_to_explore_debug_signals,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MetaCalibrationReport {
    pub train_markets: usize,
    pub raw_train_samples: usize,
    pub train_samples: usize,
    pub train_updates: u32,
    pub train_log_loss: Option<f32>,
    pub selected_training_config: Option<MetaTrainingConfig>,
    pub candidate_evaluations: Vec<MetaCandidateEvaluation>,
    pub raw_validation_samples: usize,
    pub validation_samples: usize,
    pub validation: Option<MetaEvaluationSummary>,
    pub selected: bool,
    pub rejected_reason: Option<String>,
    pub oos_samples: usize,
    pub oos_evaluation_samples: usize,
    pub oos: Option<MetaEvaluationSummary>,
    pub beta_enabled: bool,
    pub beta_coefficients: (f32, f32, f32),
    pub top_feature_weights: Vec<MetaFeatureWeight>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetaCandidateEvaluation {
    pub training_config: MetaTrainingConfig,
    pub train_log_loss: f32,
    pub updates: u32,
    pub beta_enabled: bool,
    pub beta_coefficients: (f32, f32, f32),
    pub isotonic_bins: usize,
    pub tree_count: usize,
    pub tree_split_count: usize,
    pub top_feature_weights: Vec<MetaFeatureWeight>,
    pub validation: MetaEvaluationSummary,
    pub selected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalkForwardFoldSummary {
    pub fold_idx: usize,
    pub train_end_exclusive: usize,
    pub purge_markets: usize,
    pub test_start: usize,
    pub test_end: usize,
    pub meta_train_samples: usize,
    pub meta_train_log_loss: Option<f32>,
    pub meta_oos: Option<MetaEvaluationSummary>,
    pub fold_results: WalkForwardSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetaEvaluationSummary {
    pub samples: usize,
    pub market_count: usize,
    pub positive_rate: f32,
    pub market_equal_weighted_positive_rate: f32,
    pub base_distribution: PredictionDistribution,
    pub calibrated_distribution: PredictionDistribution,
    pub prior_log_loss: f32,
    pub base_log_loss: f32,
    pub calibrated_log_loss: f32,
    pub log_loss_delta: f32,
    pub prior_log_loss_delta: f32,
    pub prior_brier: f32,
    pub base_brier: f32,
    pub calibrated_brier: f32,
    pub brier_delta: f32,
    pub prior_brier_delta: f32,
    pub market_equal_weighted_prior_log_loss: f32,
    pub market_equal_weighted_base_log_loss: f32,
    pub market_equal_weighted_calibrated_log_loss: f32,
    pub market_equal_weighted_prior_brier: f32,
    pub market_equal_weighted_base_brier: f32,
    pub market_equal_weighted_calibrated_brier: f32,
    pub base_accuracy: f32,
    pub calibrated_accuracy: f32,
    pub calibrated_ece: f32,
    pub calibration_bins: Vec<CalibrationBin>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PredictionDistribution {
    pub mean: f32,
    pub p10: f32,
    pub p50: f32,
    pub p90: f32,
    pub share_ge_55: f32,
    pub share_ge_60: f32,
    pub share_ge_65: f32,
    pub share_ge_70: f32,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CalibrationBin {
    pub lower: f32,
    pub upper: f32,
    pub samples: usize,
    pub avg_predicted: f32,
    pub observed_rate: f32,
}

fn market_volatility_range(events: &[pm_types::ReplayEvent]) -> f64 {
    if events.is_empty() {
        return 0.0;
    }
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for e in events {
        if !e.yes_mid.is_finite() {
            continue;
        }
        let v = e.yes_mid as f64;
        if v < low {
            low = v;
        }
        if v > high {
            high = v;
        }
    }
    if !low.is_finite() {
        return 0.0;
    }
    high - low
}

fn volatility_band(range: f64, threshold: f64) -> VolatilityBand {
    if range.is_nan() {
        return VolatilityBand::Low;
    }
    if range > threshold {
        VolatilityBand::High
    } else {
        VolatilityBand::Low
    }
}

fn sample_replay_events(
    events: &[pm_types::ReplayEvent],
    sample_ms: u64,
) -> Vec<pm_types::ReplayEvent> {
    if sample_ms == 0 || events.len() <= 2 {
        return events.to_vec();
    }
    let sample_ns = (sample_ms as i64).saturating_mul(1_000_000).max(1);
    let mut sampled = Vec::with_capacity(events.len().min(320));
    sampled.push(events[0]);

    let first_ns = events[0].ts_ns;
    let mut current_bucket = None::<i64>;
    let mut pending = None::<pm_types::ReplayEvent>;
    for event in events.iter().skip(1).take(events.len().saturating_sub(2)) {
        let bucket = event.ts_ns.saturating_sub(first_ns) / sample_ns;
        if current_bucket != Some(bucket) {
            if let Some(previous) = pending.take() {
                sampled.push(previous);
            }
            current_bucket = Some(bucket);
        }
        pending = Some(*event);
    }
    if let Some(previous) = pending {
        if sampled
            .last()
            .is_none_or(|last| last.ts_ns != previous.ts_ns)
        {
            sampled.push(previous);
        }
    }

    let last = *events.last().expect("events length checked");
    if sampled.last().is_none_or(|event| event.ts_ns != last.ts_ns) {
        sampled.push(last);
    }
    sampled
}

fn read_replay_event_cache(path: &Path) -> Result<Vec<ReplayEvent>> {
    let file =
        File::open(path).with_context(|| format!("open replay event cache {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line =
            line.with_context(|| format!("read line {} from {}", idx + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let event: ReplayEvent = serde_json::from_str(&line)
            .with_context(|| format!("decode line {} from {}", idx + 1, path.display()))?;
        events.push(event);
    }
    Ok(events)
}

fn write_replay_event_cache(path: &Path, events: &[ReplayEvent]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create replay event cache dir {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let file = File::create(&tmp)
        .with_context(|| format!("create replay event cache {}", tmp.display()))?;
    let mut writer = BufWriter::new(file);
    for event in events {
        writeln!(writer, "{}", serde_json::to_string(event)?)
            .with_context(|| format!("write replay event cache {}", tmp.display()))?;
    }
    writer
        .flush()
        .with_context(|| format!("flush replay event cache {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

fn rebind_replay_event_market_ids(events: &mut [ReplayEvent], market_id: MarketId) {
    for event in events {
        event.market_id = market_id;
    }
}

fn replay_event_cache_path(cache_dir: Option<&Path>, market: &MarketHandle) -> Option<PathBuf> {
    cache_dir.map(|dir| {
        dir.join(&market.date)
            .join(format!("{}.jsonl", market.asset_id))
    })
}

async fn load_replay_events_for_market(
    store: &TelonexStore,
    store_inner: Arc<dyn object_store::ObjectStore>,
    market: &MarketHandle,
    market_id: MarketId,
    cache_dir: Option<&Path>,
) -> Result<Vec<ReplayEvent>> {
    let cache_path = replay_event_cache_path(cache_dir, market);
    if let Some(ref path) = cache_path {
        match read_replay_event_cache(path) {
            Ok(mut events) if !events.is_empty() => {
                rebind_replay_event_market_ids(&mut events, market_id);
                tracing::info!(
                    market = %market.slug,
                    n = events.len(),
                    "replay events from disk cache"
                );
                return Ok(events);
            }
            Ok(_) => {}
            Err(err) if path.exists() => {
                tracing::warn!(
                    market = %market.slug,
                    cache = %path.display(),
                    error = %err,
                    "replay event cache read failed; falling back to source tape"
                );
            }
            Err(_) => {}
        }
    }

    let source_path = store
        .resolve_asset_day(
            "polymarket",
            Channel::BookSnapshot25,
            &market.date,
            &market.asset_id,
        )
        .await
        .with_context(|| format!("resolve tape path for {}", market.slug))?;
    let (events, _stats) = load_book_snapshot_async(store_inner, source_path, market_id)
        .await
        .with_context(|| format!("load tape for {}", market.slug))?;
    if let Some(ref path) = cache_path {
        if let Err(err) = write_replay_event_cache(path, &events) {
            tracing::warn!(
                market = %market.slug,
                cache = %path.display(),
                error = %err,
                "replay event cache write failed"
            );
        }
    }
    Ok(events)
}

fn compounded_clip(bankroll: f64, frac: f64) -> f64 {
    if !bankroll.is_finite() || !frac.is_finite() || bankroll <= 0.0 || frac <= 0.0 {
        return 0.0;
    }
    let raw = bankroll * frac;
    let cap = (bankroll * 0.10).max(0.0);
    if cap <= 0.50 {
        raw.min(cap).max(0.0)
    } else {
        raw.clamp(0.50, cap)
    }
}

fn per_market_exposure_cap(cfg: &WalkForwardConfig, bankroll: f64) -> f64 {
    match cfg.max_per_market_exposure_frac {
        Some(frac) if frac.is_finite() && frac >= 0.0 => {
            cfg.max_per_market_exposure_usdc.min(bankroll * frac)
        }
        _ => cfg.max_per_market_exposure_usdc,
    }
}

fn drawdown_clip_multiplier(
    drawdown_pct: f64,
    soft_pct: f64,
    hard_pct: f64,
    min_multiplier: f64,
) -> f64 {
    if !drawdown_pct.is_finite()
        || !soft_pct.is_finite()
        || !hard_pct.is_finite()
        || soft_pct >= hard_pct
    {
        return 1.0;
    }
    let floor = if min_multiplier.is_finite() {
        min_multiplier.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if drawdown_pct <= soft_pct {
        1.0
    } else if drawdown_pct >= hard_pct {
        floor
    } else {
        let progress = (drawdown_pct - soft_pct) / (hard_pct - soft_pct);
        1.0 - progress * (1.0 - floor)
    }
}

fn daily_remaining_loss_budget_usdc(
    daily_start_equity: f64,
    current_equity: f64,
    daily_loss_cap_pct: f64,
) -> Option<f64> {
    if daily_loss_cap_pct >= 1.0 || daily_start_equity <= 0.0 {
        return None;
    }
    let max_loss = daily_start_equity * daily_loss_cap_pct.max(0.0);
    let realized_loss = (daily_start_equity - current_equity).max(0.0);
    Some((max_loss - realized_loss).max(0.0))
}

#[derive(Debug, Clone, Default)]
struct LossStreakCooldownState {
    consecutive_losses: usize,
    cooldown_remaining_markets: usize,
}

impl LossStreakCooldownState {
    fn is_active(&self) -> bool {
        self.cooldown_remaining_markets > 0
    }

    fn consume_cooldown_market(&mut self) {
        self.cooldown_remaining_markets = self.cooldown_remaining_markets.saturating_sub(1);
    }

    fn record_completed_market(
        &mut self,
        traded: bool,
        pnl_usdc: f64,
        loss_threshold_usdc: f64,
        cooldown_after: usize,
        cooldown_markets: usize,
    ) {
        if cooldown_after == 0 || cooldown_markets == 0 || !traded {
            return;
        }
        if pnl_usdc <= loss_threshold_usdc {
            self.consecutive_losses += 1;
            if self.consecutive_losses >= cooldown_after {
                self.consecutive_losses = 0;
                self.cooldown_remaining_markets = cooldown_markets;
            }
        } else {
            self.consecutive_losses = 0;
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct StrategyAggregate {
    pub total_pnl_usdc: f64,
    pub first_start_equity_usdc: f64,
    pub last_end_equity_usdc: f64,
    pub min_end_equity_usdc: f64,
    pub max_end_equity_usdc: f64,
    pub compounded_return_pct: f64,
    pub path_max_drawdown_pct: f64,
    pub mean_pnl_usdc: f64,
    pub median_pnl_usdc: f64,
    pub stdev_pnl_usdc: f64,
    pub hit_rate: f64,
    pub markets_with_orders: usize,
    pub total_orders_submitted: usize,
    pub total_orders_filled: usize,
    pub total_orders_filled_taker: usize,
    pub total_orders_filled_maker: usize,
    pub maker_fill_rate: f64,
    pub total_requested_notional_usdc: f64,
    pub total_filled_notional_usdc: f64,
    pub fill_notional_ratio: f64,
    pub total_requested_shares: f64,
    pub total_filled_shares: f64,
    pub fill_shares_ratio: f64,
    pub avg_slippage_bps: f64,
    pub total_orders_rejected_model_gate: usize,
    pub total_orders_rejected_model_gate_confidence: usize,
    pub total_orders_rejected_model_gate_risk: usize,
    pub total_orders_rejected_model_gate_edge: usize,
    pub worst_market_pnl: f64,
    pub best_market_pnl: f64,
    pub sharpe_ratio: f64,
    pub by_fill_tag: HashMap<String, FillTagAggregate>,
    pub model_fill_quality: ModelFillQualitySummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bonereaper_v2_gate_stats: Option<BonereaperV2GateStats>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelFillQualitySummary {
    pub all: ModelFillQuality,
    pub range_ge_050: ModelFillQuality,
    pub range_lt_050: ModelFillQuality,
    pub whipsaw_ge_035: ModelFillQuality,
    pub whipsaw_lt_035: ModelFillQuality,
    pub market_samples: usize,
    pub market_majority_side_accuracy: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ModelFillQuality {
    pub fills: usize,
    pub hit_rate: f64,
    pub avg_predicted_p: f64,
    pub brier: f64,
    pub log_loss: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FillTagAggregate {
    pub fills: usize,
    pub maker_fills: usize,
    pub taker_fills: usize,
    pub maker_fill_rate: f64,
    pub total_notional_usdc: f64,
    pub total_pnl_usdc: f64,
    pub mean_pnl_usdc: f64,
    pub avg_fill_price: f64,
    pub avg_slippage_bps: f64,
    pub hit_rate: f64,
    pub avg_side_edge_vs_fill: f64,
    pub avg_market_yes_range_so_far: f64,
    pub avg_regime_whipsaw_score: f64,
    pub avg_regime_path_efficiency: f64,
    pub avg_regime_reversal_pressure: f64,
    pub avg_regime_sign_flip_rate: f64,
    pub avg_regime_realized_vol_180s_bps: f64,
    pub avg_post_fill_adverse_excursion: f64,
    pub avg_post_fill_favourable_excursion: f64,
    pub post_fill_cross_mid_rate: f64,
}

#[derive(Debug, Default)]
struct FillTagAccumulator {
    fills: usize,
    maker_fills: usize,
    taker_fills: usize,
    wins: usize,
    total_pnl_usdc: f64,
    total_notional_usdc: f64,
    sum_fill_price: f64,
    slippage_notional: f64,
    sum_side_edge_vs_fill: f64,
    side_edge_samples: usize,
    sum_market_yes_range_so_far: f64,
    market_range_samples: usize,
    sum_regime_whipsaw_score: f64,
    sum_regime_path_efficiency: f64,
    sum_regime_reversal_pressure: f64,
    sum_regime_sign_flip_rate: f64,
    sum_regime_realized_vol_180s_bps: f64,
    regime_samples: usize,
    sum_post_fill_adverse_excursion: f64,
    sum_post_fill_favourable_excursion: f64,
    post_fill_cross_mid_count: usize,
    post_fill_path_samples: usize,
}

#[derive(Debug, Default)]
struct ModelFillQualityAccumulator {
    all: ModelFillQualityBucket,
    range_ge_050: ModelFillQualityBucket,
    range_lt_050: ModelFillQualityBucket,
    whipsaw_ge_035: ModelFillQualityBucket,
    whipsaw_lt_035: ModelFillQualityBucket,
    market_samples: usize,
    market_majority_side_correct: usize,
}

#[derive(Debug, Default)]
struct ModelFillQualityBucket {
    fills: usize,
    wins: usize,
    sum_predicted_p: f64,
    sum_brier: f64,
    sum_log_loss: f64,
}

impl ModelFillQualityAccumulator {
    fn push_fill(&mut self, fill: &crate::runner::Fill, yes_resolved: bool) {
        let Some(predicted_p) = fill.side_model_p else {
            return;
        };
        let Some(win) = buy_fill_won(fill, yes_resolved) else {
            return;
        };
        let p = (predicted_p as f64).clamp(1e-6, 1.0 - 1e-6);
        self.all.push(p, win);
        if let Some(range) = fill.market_yes_range_so_far {
            if range >= 0.50 {
                self.range_ge_050.push(p, win);
            } else {
                self.range_lt_050.push(p, win);
            }
        }
        if let Some(whipsaw) = fill.regime_whipsaw_score {
            if whipsaw >= 0.35 {
                self.whipsaw_ge_035.push(p, win);
            } else {
                self.whipsaw_lt_035.push(p, win);
            }
        }
    }

    fn push_market_majority(&mut self, record: &StrategyMarketResult) {
        let mut yes_notional = 0.0;
        let mut no_notional = 0.0;
        for fill in &record.fills_detail {
            if fill.side_model_p.is_none() {
                continue;
            }
            match fill.side.as_str() {
                "BuyYes" => yes_notional += fill.notional,
                "BuyNo" => no_notional += fill.notional,
                _ => {}
            }
        }
        if yes_notional == 0.0 && no_notional == 0.0 {
            return;
        }
        let predicted_yes = yes_notional >= no_notional;
        self.market_samples += 1;
        if predicted_yes == record.yes_resolved {
            self.market_majority_side_correct += 1;
        }
    }

    fn into_summary(self) -> ModelFillQualitySummary {
        ModelFillQualitySummary {
            all: self.all.into_quality(),
            range_ge_050: self.range_ge_050.into_quality(),
            range_lt_050: self.range_lt_050.into_quality(),
            whipsaw_ge_035: self.whipsaw_ge_035.into_quality(),
            whipsaw_lt_035: self.whipsaw_lt_035.into_quality(),
            market_samples: self.market_samples,
            market_majority_side_accuracy: if self.market_samples > 0 {
                self.market_majority_side_correct as f64 / self.market_samples as f64
            } else {
                0.0
            },
        }
    }
}

impl ModelFillQualityBucket {
    fn push(&mut self, predicted_p: f64, win: bool) {
        self.fills += 1;
        if win {
            self.wins += 1;
            self.sum_log_loss -= predicted_p.ln();
        } else {
            self.sum_log_loss -= (1.0 - predicted_p).ln();
        }
        let outcome = if win { 1.0 } else { 0.0 };
        self.sum_predicted_p += predicted_p;
        self.sum_brier += (predicted_p - outcome).powi(2);
    }

    fn into_quality(self) -> ModelFillQuality {
        ModelFillQuality {
            fills: self.fills,
            hit_rate: if self.fills > 0 {
                self.wins as f64 / self.fills as f64
            } else {
                0.0
            },
            avg_predicted_p: if self.fills > 0 {
                self.sum_predicted_p / self.fills as f64
            } else {
                0.0
            },
            brier: if self.fills > 0 {
                self.sum_brier / self.fills as f64
            } else {
                0.0
            },
            log_loss: if self.fills > 0 {
                self.sum_log_loss / self.fills as f64
            } else {
                0.0
            },
        }
    }
}

impl FillTagAccumulator {
    fn push(&mut self, fill: &crate::runner::Fill, pnl: f64) {
        self.fills += 1;
        if fill.maker {
            self.maker_fills += 1;
        } else {
            self.taker_fills += 1;
        }
        self.total_pnl_usdc += pnl;
        self.total_notional_usdc += fill.notional;
        self.sum_fill_price += fill.price as f64;
        self.slippage_notional += fill.slippage_bps as f64 * fill.notional;
        if pnl > 0.0 {
            self.wins += 1;
        }
        if let Some(edge) = fill.side_edge_vs_fill {
            self.sum_side_edge_vs_fill += edge as f64;
            self.side_edge_samples += 1;
        }
        if let Some(range) = fill.market_yes_range_so_far {
            self.sum_market_yes_range_so_far += range as f64;
            self.market_range_samples += 1;
        }
        if let (
            Some(whipsaw),
            Some(path_efficiency),
            Some(reversal_pressure),
            Some(sign_flip_rate),
            Some(realized_vol),
        ) = (
            fill.regime_whipsaw_score,
            fill.regime_path_efficiency,
            fill.regime_reversal_pressure,
            fill.regime_sign_flip_rate,
            fill.regime_realized_vol_180s_bps,
        ) {
            self.sum_regime_whipsaw_score += whipsaw as f64;
            self.sum_regime_path_efficiency += path_efficiency as f64;
            self.sum_regime_reversal_pressure += reversal_pressure as f64;
            self.sum_regime_sign_flip_rate += sign_flip_rate as f64;
            self.sum_regime_realized_vol_180s_bps += realized_vol as f64;
            self.regime_samples += 1;
        }
        if let Some(path) = fill.post_fill_path {
            self.sum_post_fill_adverse_excursion += path.adverse_excursion as f64;
            self.sum_post_fill_favourable_excursion += path.favourable_excursion as f64;
            self.post_fill_cross_mid_count += usize::from(path.crossed_mid_after_fill);
            self.post_fill_path_samples += 1;
        }
    }

    fn into_aggregate(self) -> FillTagAggregate {
        FillTagAggregate {
            fills: self.fills,
            maker_fills: self.maker_fills,
            taker_fills: self.taker_fills,
            maker_fill_rate: if self.fills > 0 {
                self.maker_fills as f64 / self.fills as f64
            } else {
                0.0
            },
            total_notional_usdc: self.total_notional_usdc,
            total_pnl_usdc: self.total_pnl_usdc,
            mean_pnl_usdc: if self.fills > 0 {
                self.total_pnl_usdc / self.fills as f64
            } else {
                0.0
            },
            avg_fill_price: if self.fills > 0 {
                self.sum_fill_price / self.fills as f64
            } else {
                0.0
            },
            avg_slippage_bps: if self.total_notional_usdc > 0.0 {
                self.slippage_notional / self.total_notional_usdc
            } else {
                0.0
            },
            hit_rate: if self.fills > 0 {
                self.wins as f64 / self.fills as f64
            } else {
                0.0
            },
            avg_side_edge_vs_fill: if self.side_edge_samples > 0 {
                self.sum_side_edge_vs_fill / self.side_edge_samples as f64
            } else {
                0.0
            },
            avg_market_yes_range_so_far: if self.market_range_samples > 0 {
                self.sum_market_yes_range_so_far / self.market_range_samples as f64
            } else {
                0.0
            },
            avg_regime_whipsaw_score: if self.regime_samples > 0 {
                self.sum_regime_whipsaw_score / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_path_efficiency: if self.regime_samples > 0 {
                self.sum_regime_path_efficiency / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_reversal_pressure: if self.regime_samples > 0 {
                self.sum_regime_reversal_pressure / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_sign_flip_rate: if self.regime_samples > 0 {
                self.sum_regime_sign_flip_rate / self.regime_samples as f64
            } else {
                0.0
            },
            avg_regime_realized_vol_180s_bps: if self.regime_samples > 0 {
                self.sum_regime_realized_vol_180s_bps / self.regime_samples as f64
            } else {
                0.0
            },
            avg_post_fill_adverse_excursion: if self.post_fill_path_samples > 0 {
                self.sum_post_fill_adverse_excursion / self.post_fill_path_samples as f64
            } else {
                0.0
            },
            avg_post_fill_favourable_excursion: if self.post_fill_path_samples > 0 {
                self.sum_post_fill_favourable_excursion / self.post_fill_path_samples as f64
            } else {
                0.0
            },
            post_fill_cross_mid_rate: if self.post_fill_path_samples > 0 {
                self.post_fill_cross_mid_count as f64 / self.post_fill_path_samples as f64
            } else {
                0.0
            },
        }
    }
}

/// Per-market spot-history cache so we don't re-download the same Binance day.
#[derive(Default)]
struct SpotCache {
    pub inner: HashMap<String, Arc<SpotHistory>>,
    raw_days: HashMap<String, Arc<Vec<SpotTick>>>,
}

impl SpotCache {
    async fn load_raw_day(
        &mut self,
        store: &TelonexStore,
        symbol: &str,
        date: &str,
        required: bool,
    ) -> Result<Option<Arc<Vec<SpotTick>>>> {
        let key = spot_cache_key(symbol, date);
        if let Some(ticks) = self.raw_days.get(&key) {
            return Ok(Some(ticks.clone()));
        }

        let path = match resolve_binance_day(store, "agg_trades", symbol, date).await {
            Ok(path) => path,
            Err(err) if required => {
                return Err(err).with_context(|| format!("resolve spot {symbol} {date}"));
            }
            Err(err) => {
                tracing::warn!(
                    symbol,
                    date,
                    error = %err,
                    "optional prior spot day unavailable"
                );
                return Ok(None);
            }
        };
        let (ticks, stats) = load_binance_agg_trades_async(store.store(), path).await?;
        tracing::info!(symbol, date, ticks = stats.rows_emitted, "spot day loaded");
        let ticks = Arc::new(ticks);
        self.raw_days.insert(key, ticks.clone());
        Ok(Some(ticks))
    }

    async fn get_or_load(
        &mut self,
        store: &TelonexStore,
        symbol: &str,
        date: &str,
    ) -> Result<Arc<SpotHistory>> {
        let key = spot_cache_key(symbol, date);
        if let Some(s) = self.inner.get(&key) {
            return Ok(s.clone());
        }
        let current = self
            .load_raw_day(store, symbol, date, true)
            .await?
            .ok_or_else(|| anyhow!("missing required spot day {symbol} {date}"))?;

        let mut ticks = Vec::new();
        if let Some(prev_date) = previous_date(date)? {
            if let Some(prev) = self
                .load_raw_day(store, symbol, &prev_date, false)
                .await
                .with_context(|| format!("load optional prior spot {prev_date}"))?
            {
                ticks.reserve(prev.len() + current.len());
                ticks.extend_from_slice(&prev);
            }
        }
        ticks.extend_from_slice(&current);
        let h = Arc::new(SpotHistory::new(ticks));
        self.inner.insert(key, h.clone());
        Ok(h)
    }
}

fn previous_date(date: &str) -> Result<Option<String>> {
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .with_context(|| format!("parse market date {date}"))?;
    Ok(parsed
        .checked_sub_signed(Duration::days(1))
        .map(|d| d.format("%Y-%m-%d").to_string()))
}

fn spot_history_for_market(
    spot_map: &HashMap<String, Arc<SpotHistory>>,
    empty_spot: &Arc<SpotHistory>,
    cfg: &WalkForwardConfig,
    market: &MarketHandle,
) -> Arc<SpotHistory> {
    let Some(symbol) = spot_symbol_for_market(&cfg.spot_symbol, &market.slug)
        .ok()
        .flatten()
    else {
        return empty_spot.clone();
    };
    spot_map
        .get(&spot_cache_key(&symbol, &market.date))
        .cloned()
        .unwrap_or_else(|| empty_spot.clone())
}

fn model_market_context_for_slug(slug: &str) -> ModelMarketContext {
    let slug = slug.to_ascii_lowercase();
    let asset = if slug.starts_with("btc-updown-") {
        MarketAsset::Btc
    } else if slug.starts_with("eth-updown-") {
        MarketAsset::Eth
    } else {
        MarketAsset::Unknown
    };
    let window_seconds = if slug.contains("-5m-") {
        300
    } else if slug.contains("-15m-") {
        900
    } else {
        0
    };
    ModelMarketContext {
        asset,
        window_seconds,
    }
}

fn model_market_context_for_cfg(
    cfg: &WalkForwardConfig,
    market: &MarketHandle,
) -> ModelMarketContext {
    if cfg.enable_market_context_features {
        model_market_context_for_slug(&market.slug)
    } else {
        ModelMarketContext::default()
    }
}

pub async fn run_walkforward(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
) -> Result<(Vec<MarketResult>, WalkForwardSummary)> {
    // Always sort by normalized close so portfolio mode is well-defined and
    // parallel mode logs read sensibly. Some older local market lists stored
    // the slug/open timestamp in `close_ts`; normalize those to open + 300s.
    let mut markets_sorted: Vec<MarketHandle> = markets.to_vec();
    markets_sorted.sort_by_key(market_close_ts);
    let markets = &markets_sorted[..];

    let mut spot_cache = SpotCache::default();
    // Preload all distinct spot symbol/date pairs up front to amortize downloads.
    // Use `--spot-symbol auto` for mixed BTC/ETH market lists.
    let unique_spot_days: Vec<(String, String)> = markets
        .iter()
        .map(|m| {
            spot_symbol_for_market(&cfg.spot_symbol, &m.slug)
                .map(|symbol| symbol.map(|symbol| (symbol, m.date.clone())))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    for (symbol, date) in &unique_spot_days {
        spot_cache
            .get_or_load(store, symbol, date)
            .await
            .with_context(|| format!("preload spot {symbol} {date}"))?;
    }
    let spot_map_top: HashMap<String, Arc<SpotHistory>> = spot_cache.inner.clone();
    if cfg.portfolio_mode && (cfg.walk_forward_folds.is_some() || cfg.fold_size.is_some()) {
        return Err(anyhow!(
            "walk-forward fold configuration is not supported in portfolio mode"
        ));
    }

    if cfg.portfolio_mode {
        let preloaded_snapshot = if cfg.enable_meta_calibration {
            match cfg.meta_calibrator_snapshot_in.as_deref() {
                Some(path) => Some(read_meta_snapshot(path)?),
                None => None,
            }
        } else {
            None
        };
        if cfg.min_train_markets > 0 {
            if cfg.min_train_markets >= markets.len() {
                return Err(anyhow!(
                    "min_train_markets={} leaves no markets for portfolio evaluation",
                    cfg.min_train_markets
                ));
            }
            let mut meta_report = None;
            let meta_snapshot = if !cfg.enable_meta_calibration {
                tracing::info!(
                    min_train_markets = cfg.min_train_markets,
                    "meta-calibration disabled; preserving train/eval split without training snapshot"
                );
                None
            } else if let Some(snapshot) = preloaded_snapshot.clone() {
                meta_report = Some(MetaCalibrationReport {
                    train_markets: 0,
                    raw_train_samples: 0,
                    train_samples: 0,
                    train_updates: snapshot.updates,
                    train_log_loss: None,
                    selected_training_config: None,
                    candidate_evaluations: Vec::new(),
                    raw_validation_samples: 0,
                    validation_samples: 0,
                    validation: None,
                    selected: true,
                    rejected_reason: None,
                    oos_samples: 0,
                    oos_evaluation_samples: 0,
                    oos: None,
                    beta_enabled: snapshot.beta_enabled(),
                    beta_coefficients: snapshot.beta_coefficients(),
                    top_feature_weights: snapshot.top_feature_weights(12),
                });
                Some(snapshot)
            } else if cfg.forbid_meta_training {
                return Err(anyhow!(
                    "meta training is forbidden but no meta-calibrator snapshot was loaded"
                ));
            } else {
                let training_samples = load_or_collect_training_samples(
                    store,
                    &markets[..cfg.min_train_markets],
                    cfg,
                    &spot_map_top,
                )
                .await?;
                if training_samples.is_empty() {
                    None
                } else {
                    let selected = train_validated_meta_calibrator(
                        cfg.min_train_markets,
                        &training_samples,
                        cfg.meta_training_config,
                        MetaSampleLimits::from_config(cfg),
                        cfg.meta_calibrator_snapshot_out.as_deref(),
                    )?;
                    meta_report = Some(selected.report);
                    Some(selected.snapshot)
                }
            };
            return run_portfolio(
                store,
                &markets[cfg.min_train_markets..],
                cfg,
                &spot_map_top,
                meta_snapshot,
                meta_report,
            )
            .await;
        }
        let meta_report = preloaded_snapshot
            .as_ref()
            .map(|snapshot| MetaCalibrationReport {
                train_markets: 0,
                raw_train_samples: 0,
                train_samples: 0,
                train_updates: snapshot.updates,
                train_log_loss: None,
                selected_training_config: None,
                candidate_evaluations: Vec::new(),
                raw_validation_samples: 0,
                validation_samples: 0,
                validation: None,
                selected: true,
                rejected_reason: None,
                oos_samples: 0,
                oos_evaluation_samples: 0,
                oos: None,
                beta_enabled: snapshot.beta_enabled(),
                beta_coefficients: snapshot.beta_coefficients(),
                top_feature_weights: snapshot.top_feature_weights(12),
            });
        return run_portfolio(
            store,
            markets,
            cfg,
            &spot_map_top,
            preloaded_snapshot,
            meta_report,
        )
        .await;
    }

    let mut fold_summaries = Vec::new();
    let fold_plan = build_fold_plan(markets.len(), cfg)?;
    let use_folds = cfg.walk_forward_folds.is_some() || cfg.fold_size.is_some();

    let mut results = Vec::new();
    let mut training_samples = Vec::new();
    let mut training_loaded_until = 0usize;
    for (fold_idx, (train_end_exclusive, test_start, test_end)) in fold_plan.iter().enumerate() {
        let mut meta_train_samples = 0usize;
        let mut meta_train_log_loss = None;
        let meta_snapshot = if cfg.enable_meta_calibration && use_folds && *train_end_exclusive > 0
        {
            if *train_end_exclusive > training_loaded_until {
                let new_samples = collect_training_samples(
                    store,
                    &markets[training_loaded_until..*train_end_exclusive],
                    cfg,
                    &spot_map_top,
                )
                .await?;
                training_samples.extend(new_samples);
                training_loaded_until = *train_end_exclusive;
            }
            if training_samples.is_empty() {
                None
            } else {
                let mut state = ModelState::new();
                let stats = state.fit_meta_calibrator(&training_samples, cfg.meta_training_config);
                meta_train_samples = stats.samples;
                meta_train_log_loss = Some(stats.log_loss);
                tracing::info!(
                    fold = fold_idx,
                    samples = stats.samples,
                    updates = stats.updates,
                    train_markets = training_loaded_until,
                    log_loss = stats.log_loss,
                    "trained fold meta-calibrator"
                );
                Some(state.meta_calibrator_snapshot())
            }
        } else {
            None
        };
        let meta_oos = if cfg.enable_meta_calibration && use_folds {
            if let Some(snapshot) = meta_snapshot.as_ref() {
                let test_samples = collect_training_samples(
                    store,
                    &markets[*test_start..*test_end],
                    cfg,
                    &spot_map_top,
                )
                .await?;
                Some(evaluate_meta_calibration(snapshot, &test_samples))
            } else {
                None
            }
        } else {
            None
        };
        let fold_markets = &markets[*test_start..*test_end];
        let fold_markets = run_markets(
            store,
            fold_markets,
            cfg,
            &spot_map_top,
            *test_start,
            cfg.max_concurrent_fetches,
            meta_snapshot,
        )
        .await?;
        if use_folds {
            let mut fold_summary = aggregate(&fold_markets, &cfg.strategies);
            fold_summary.run_config = Some(summary_run_config(cfg));
            fold_summaries.push(WalkForwardFoldSummary {
                fold_idx,
                train_end_exclusive: *train_end_exclusive,
                purge_markets: cfg.purge_markets,
                test_start: *test_start,
                test_end: *test_end,
                meta_train_samples,
                meta_train_log_loss,
                meta_oos,
                fold_results: fold_summary,
            });
        }
        results.extend(fold_markets);
    }

    let mut summary = aggregate(&results, &cfg.strategies);
    summary.run_config = Some(summary_run_config(cfg));
    if use_folds {
        summary.fold_summaries = fold_summaries;
    }
    Ok((results, summary))
}

fn build_fold_plan(total: usize, cfg: &WalkForwardConfig) -> Result<Vec<(usize, usize, usize)>> {
    if cfg.walk_forward_folds.is_some() && cfg.fold_size.is_some() {
        return Err(anyhow!(
            "cannot set both --walk-forward-folds and --fold-size"
        ));
    }
    if total == 0 {
        return Ok(vec![(0, 0, 0)]);
    }
    if let Some(folds) = cfg.walk_forward_folds {
        if folds == 0 {
            return Err(anyhow!("walk-forward-folds must be >= 1"));
        }
        if folds > total {
            return Err(anyhow!(
                "walk-forward-folds ({folds}) cannot exceed number of markets ({total})"
            ));
        }
        let base = total / folds;
        let remainder = total % folds;
        let mut test_start = 0usize;
        let mut out = Vec::with_capacity(folds);
        for fold_idx in 0..folds {
            let size = base + usize::from(fold_idx < remainder);
            let test_end = (test_start + size).min(total);
            if size == 0 || test_start >= total || test_end <= test_start {
                break;
            }
            let train_end_exclusive = test_start.saturating_sub(cfg.purge_markets);
            if train_end_exclusive >= cfg.min_train_markets {
                out.push((train_end_exclusive, test_start, test_end));
            }
            test_start = test_end;
        }
        if out.is_empty() {
            return Err(anyhow!(
                "no walk-forward folds satisfy min_train_markets={} with total markets={total}",
                cfg.min_train_markets
            ));
        }
        Ok(out)
    } else if let Some(fold_size) = cfg.fold_size {
        if fold_size == 0 {
            return Err(anyhow!("fold-size must be >= 1"));
        }
        let mut out = Vec::new();
        let mut test_start = 0usize;
        while test_start < total {
            let test_end = (test_start + fold_size).min(total);
            let train_end_exclusive = test_start.saturating_sub(cfg.purge_markets);
            if train_end_exclusive >= cfg.min_train_markets {
                out.push((train_end_exclusive, test_start, test_end));
            }
            test_start = test_end;
        }
        if out.is_empty() {
            return Err(anyhow!(
                "no walk-forward folds satisfy min_train_markets={} with total markets={total}",
                cfg.min_train_markets
            ));
        }
        Ok(out)
    } else {
        Ok(vec![(0, 0, total)])
    }
}

async fn load_or_collect_training_samples(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
) -> Result<Vec<MetaTrainingSample>> {
    if let Some(path) = cfg.meta_training_samples_cache.as_deref() {
        if path.exists() {
            let file = File::open(path)
                .with_context(|| format!("open meta training samples cache {}", path.display()))?;
            match serde_json::from_reader::<_, Vec<MetaTrainingSample>>(BufReader::new(file)) {
                Ok(samples) => {
                    tracing::info!(
                        samples = samples.len(),
                        path = %path.display(),
                        "loaded meta training samples cache"
                    );
                    return Ok(samples);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "ignoring incompatible meta training samples cache"
                    );
                }
            }
        }

        let samples = collect_training_samples(store, markets, cfg, spot_map).await?;
        write_meta_training_samples(path, &samples)?;
        return Ok(samples);
    }

    collect_training_samples(store, markets, cfg, spot_map).await
}

fn write_meta_training_samples(
    path: &std::path::Path,
    samples: &[MetaTrainingSample],
) -> Result<()> {
    ensure_parent_dir(path)?;
    let file = File::create(path)
        .with_context(|| format!("create meta training samples cache {}", path.display()))?;
    serde_json::to_writer(BufWriter::new(file), samples)
        .with_context(|| format!("write meta training samples cache {}", path.display()))?;
    tracing::info!(
        samples = samples.len(),
        path = %path.display(),
        "wrote meta training samples cache"
    );
    Ok(())
}

fn read_meta_snapshot(path: &std::path::Path) -> Result<OnlineMetaCalibratorSnapshot> {
    let file = File::open(path)
        .with_context(|| format!("open meta-calibrator snapshot {}", path.display()))?;
    let snapshot = serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("read meta-calibrator snapshot {}", path.display()))?;
    tracing::info!(
        path = %path.display(),
        "loaded meta-calibrator snapshot"
    );
    Ok(snapshot)
}

fn write_meta_snapshot(
    path: &std::path::Path,
    snapshot: &OnlineMetaCalibratorSnapshot,
) -> Result<()> {
    ensure_parent_dir(path)?;
    let file = File::create(path)
        .with_context(|| format!("create meta-calibrator snapshot {}", path.display()))?;
    serde_json::to_writer_pretty(BufWriter::new(file), snapshot)
        .with_context(|| format!("write meta-calibrator snapshot {}", path.display()))?;
    tracing::info!(
        updates = snapshot.updates,
        path = %path.display(),
        "wrote meta-calibrator snapshot"
    );
    Ok(())
}

fn ensure_parent_dir(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }
    Ok(())
}

struct SelectedMetaCalibrator {
    snapshot: OnlineMetaCalibratorSnapshot,
    report: MetaCalibrationReport,
}

const META_FEATURE_EARLY_MARKET_PENALTY: usize = 20;
const META_FEATURE_MID_DISTANCE_FROM_HALF: usize = 38;

#[derive(Debug, Clone, Copy)]
struct MetaSampleLimits {
    max_fit_samples: usize,
    max_validation_samples: usize,
    max_samples_per_market: usize,
    min_base_p: f32,
    max_early_penalty: f32,
    min_mid_distance: f32,
}

impl MetaSampleLimits {
    fn from_config(cfg: &WalkForwardConfig) -> Self {
        Self {
            max_fit_samples: cfg.meta_max_fit_samples,
            max_validation_samples: cfg.meta_max_validation_samples,
            max_samples_per_market: cfg.meta_max_samples_per_market,
            min_base_p: cfg.meta_train_min_base_p,
            max_early_penalty: cfg.meta_train_max_early_penalty,
            min_mid_distance: cfg.meta_train_min_mid_distance,
        }
    }
}

fn filter_meta_samples_for_training(
    samples: &[MetaTrainingSample],
    limits: MetaSampleLimits,
) -> Vec<MetaTrainingSample> {
    let min_base_p = limits.min_base_p.clamp(0.0, 1.0);
    let max_early_penalty = limits.max_early_penalty.clamp(0.0, 1.0);
    let min_mid_distance = limits.min_mid_distance.clamp(0.0, 1.0);
    if min_base_p <= 0.0 && max_early_penalty >= 1.0 && min_mid_distance <= 0.0 {
        return samples.to_vec();
    }
    samples
        .iter()
        .copied()
        .filter(|sample| {
            sample.base_side_probability >= min_base_p
                && sample.features.values[META_FEATURE_EARLY_MARKET_PENALTY] <= max_early_penalty
                && sample.features.values[META_FEATURE_MID_DISTANCE_FROM_HALF] >= min_mid_distance
        })
        .collect()
}

fn train_validated_meta_calibrator(
    train_markets: usize,
    training_samples: &[MetaTrainingSample],
    training_config: MetaTrainingConfig,
    limits: MetaSampleLimits,
    snapshot_out: Option<&std::path::Path>,
) -> Result<SelectedMetaCalibrator> {
    let filtered_training_samples = filter_meta_samples_for_training(training_samples, limits);
    if filtered_training_samples.len() != training_samples.len() {
        tracing::info!(
            raw_samples = training_samples.len(),
            filtered_samples = filtered_training_samples.len(),
            min_base_p = limits.min_base_p,
            max_early_penalty = limits.max_early_penalty,
            min_mid_distance = limits.min_mid_distance,
            "filtered meta training samples"
        );
    }
    let training_samples = filtered_training_samples.as_slice();
    if training_samples.len() < 2 {
        let snapshot = OnlineMetaCalibrator::default().snapshot();
        if let Some(path) = snapshot_out {
            write_meta_snapshot(path, &snapshot)?;
        }
        let stats = MetaTrainingStats {
            samples: 0,
            epochs: 0,
            updates: 0,
            log_loss: 0.0,
        };
        return Ok(SelectedMetaCalibrator {
            report: meta_calibration_report(
                train_markets,
                &[],
                &stats,
                &snapshot,
                None,
                Vec::new(),
                0,
                0,
                None,
                0,
                false,
                Some("not enough training samples".to_string()),
            ),
            snapshot,
        });
    }
    let (raw_fit_samples, raw_validation_samples) =
        split_meta_samples_by_market(training_samples, 0.80);
    let fit_samples = market_balanced_meta_samples(
        &raw_fit_samples,
        limits.max_fit_samples,
        limits.max_samples_per_market,
    );
    let validation_samples = market_balanced_meta_samples(
        &raw_validation_samples,
        limits.max_validation_samples,
        limits.max_samples_per_market,
    );
    tracing::info!(
        raw_fit_samples = raw_fit_samples.len(),
        fit_samples = fit_samples.len(),
        raw_validation_samples = raw_validation_samples.len(),
        validation_samples = validation_samples.len(),
        max_samples_per_market = limits.max_samples_per_market,
        "selected market-balanced meta-calibrator samples"
    );

    let candidates = meta_training_candidates(training_config);
    let mut candidate_evaluations = Vec::with_capacity(candidates.len());
    let mut best: Option<(
        MetaTrainingConfig,
        MetaTrainingStats,
        OnlineMetaCalibratorSnapshot,
        MetaEvaluationSummary,
    )> = None;
    for candidate_cfg in candidates {
        let mut state = ModelState::new();
        let stats = state.fit_meta_calibrator(&fit_samples, candidate_cfg);
        let snapshot = state.meta_calibrator_snapshot();
        let validation = evaluate_meta_calibration(&snapshot, &validation_samples);
        let validation_passed = validation.calibrated_log_loss < validation.base_log_loss
            && validation.calibrated_log_loss < validation.prior_log_loss
            && validation.calibrated_brier <= validation.base_brier
            && validation.calibrated_brier <= validation.prior_brier
            && validation.market_equal_weighted_calibrated_log_loss
                < validation.market_equal_weighted_base_log_loss
            && validation.market_equal_weighted_calibrated_log_loss
                < validation.market_equal_weighted_prior_log_loss
            && validation.market_equal_weighted_calibrated_brier
                <= validation.market_equal_weighted_base_brier
            && validation.market_equal_weighted_calibrated_brier
                <= validation.market_equal_weighted_prior_brier;
        if validation_passed {
            let replace = best
                .as_ref()
                .map(|(_, _, _, best_validation)| {
                    validation.calibrated_log_loss < best_validation.calibrated_log_loss
                })
                .unwrap_or(true);
            if replace {
                best = Some((candidate_cfg, stats, snapshot.clone(), validation.clone()));
            }
        }
        candidate_evaluations.push(MetaCandidateEvaluation {
            training_config: candidate_cfg,
            train_log_loss: stats.log_loss,
            updates: stats.updates,
            beta_enabled: snapshot.beta_enabled(),
            beta_coefficients: snapshot.beta_coefficients(),
            isotonic_bins: snapshot.isotonic_bins(),
            tree_count: snapshot.tree_count(),
            tree_split_count: snapshot.tree_split_count(),
            top_feature_weights: snapshot.top_feature_weights(8),
            validation,
            selected: false,
        });
    }
    let (selected_training_config, stats, selected_snapshot, validation, validation_passed) =
        if let Some((selected_cfg, stats, snapshot, validation)) = best {
            if let Some(candidate) = candidate_evaluations
                .iter_mut()
                .find(|candidate| candidate.training_config == selected_cfg)
            {
                candidate.selected = true;
            }
            (Some(selected_cfg), stats, snapshot, validation, true)
        } else {
            let stats = MetaTrainingStats {
                samples: fit_samples.len(),
                epochs: training_config.epochs,
                updates: 0,
                log_loss: 0.0,
            };
            let snapshot = OnlineMetaCalibrator::default().snapshot();
            let validation = evaluate_meta_calibration(&snapshot, &validation_samples);
            (None, stats, snapshot, validation, false)
        };
    if let Some(path) = snapshot_out {
        write_meta_snapshot(path, &selected_snapshot)?;
    }
    tracing::info!(
        fit_samples = stats.samples,
        validation_samples = validation.samples,
        updates = stats.updates,
        train_markets,
        train_log_loss = stats.log_loss,
        validation_market_count = validation.market_count,
        validation_positive_rate = validation.positive_rate,
        validation_market_equal_weighted_positive_rate =
            validation.market_equal_weighted_positive_rate,
        validation_prior_log_loss = validation.prior_log_loss,
        validation_base_log_loss = validation.base_log_loss,
        validation_calibrated_log_loss = validation.calibrated_log_loss,
        validation_market_equal_weighted_prior_log_loss =
            validation.market_equal_weighted_prior_log_loss,
        validation_market_equal_weighted_base_log_loss =
            validation.market_equal_weighted_base_log_loss,
        validation_market_equal_weighted_calibrated_log_loss =
            validation.market_equal_weighted_calibrated_log_loss,
        validation_calibrated_mean = validation.calibrated_distribution.mean,
        validation_calibrated_p50 = validation.calibrated_distribution.p50,
        validation_calibrated_p90 = validation.calibrated_distribution.p90,
        validation_calibrated_share_ge_60 = validation.calibrated_distribution.share_ge_60,
        validation_calibrated_share_ge_65 = validation.calibrated_distribution.share_ge_65,
        validation_prior_brier = validation.prior_brier,
        validation_base_brier = validation.base_brier,
        validation_calibrated_brier = validation.calibrated_brier,
        selected = validation_passed,
        ?selected_training_config,
        "validated portfolio meta-calibrator"
    );
    let rejected_reason = if validation_passed {
        None
    } else {
        Some(
            "validation log loss or brier did not improve over sample and market-equal base/prior baselines"
                .to_string(),
        )
    };
    let report = meta_calibration_report(
        train_markets,
        &fit_samples,
        &stats,
        &selected_snapshot,
        selected_training_config,
        candidate_evaluations,
        raw_fit_samples.len(),
        validation_samples.len(),
        Some(validation),
        raw_validation_samples.len(),
        validation_passed,
        rejected_reason,
    );
    Ok(SelectedMetaCalibrator {
        snapshot: selected_snapshot,
        report,
    })
}

fn split_meta_samples_by_market(
    samples: &[MetaTrainingSample],
    fit_fraction: f64,
) -> (Vec<MetaTrainingSample>, Vec<MetaTrainingSample>) {
    let groups = group_meta_samples_by_market(samples);
    if groups.len() < 2 {
        return (samples.to_vec(), Vec::new());
    }

    let split_at = ((groups.len() as f64) * fit_fraction).round() as usize;
    let split_at = split_at.clamp(1, groups.len().saturating_sub(1));
    let mut fit = Vec::new();
    let mut validation = Vec::new();
    for (idx, (_market_idx, market_samples)) in groups.into_iter().enumerate() {
        if idx < split_at {
            fit.extend(market_samples);
        } else {
            validation.extend(market_samples);
        }
    }
    (fit, validation)
}

fn market_balanced_meta_samples(
    samples: &[MetaTrainingSample],
    max_samples: usize,
    max_samples_per_market: usize,
) -> Vec<MetaTrainingSample> {
    if samples.is_empty() || max_samples == 0 {
        return Vec::new();
    }
    if samples.len() <= max_samples && max_samples_per_market == 0 {
        return samples.to_vec();
    }

    let groups = group_meta_samples_by_market(samples);
    if groups.is_empty() {
        return Vec::new();
    }

    let configured_per_market = if max_samples_per_market == 0 {
        usize::MAX
    } else {
        max_samples_per_market
    };
    let per_market_cap = if max_samples >= groups.len() {
        configured_per_market.min((max_samples / groups.len()).max(1))
    } else {
        1
    };
    let mut selected = Vec::with_capacity(samples.len().min(max_samples));
    for (_market_idx, market_samples) in groups {
        let take = market_samples.len().min(per_market_cap);
        extend_evenly_sampled(&market_samples, take, &mut selected);
    }

    if selected.len() <= max_samples {
        selected
    } else {
        let mut bounded = Vec::with_capacity(max_samples);
        extend_evenly_sampled(&selected, max_samples, &mut bounded);
        bounded
    }
}

fn group_meta_samples_by_market(
    samples: &[MetaTrainingSample],
) -> Vec<(u32, Vec<MetaTrainingSample>)> {
    let mut groups: BTreeMap<u32, Vec<MetaTrainingSample>> = BTreeMap::new();
    for sample in samples {
        groups.entry(sample.market_idx).or_default().push(*sample);
    }
    groups.into_iter().collect()
}

fn extend_evenly_sampled<T: Copy>(items: &[T], take: usize, out: &mut Vec<T>) {
    if take == 0 || items.is_empty() {
        return;
    }
    if take >= items.len() {
        out.extend_from_slice(items);
        return;
    }
    if take == 1 {
        out.push(items[items.len() / 2]);
        return;
    }
    let last = items.len() - 1;
    let denom = take - 1;
    for i in 0..take {
        let idx = (i * last + denom / 2) / denom;
        out.push(items[idx]);
    }
}

fn meta_training_candidates(primary: MetaTrainingConfig) -> Vec<MetaTrainingConfig> {
    let candidates = [
        primary,
        MetaTrainingConfig {
            epochs: 4,
            learning_rate: 0.005,
            l2: 0.01,
            weight_clip: 0.25,
            reset_before_fit: true,
        },
        MetaTrainingConfig {
            epochs: 8,
            learning_rate: 0.01,
            l2: 0.01,
            weight_clip: 0.50,
            reset_before_fit: true,
        },
        MetaTrainingConfig {
            epochs: 12,
            learning_rate: 0.02,
            l2: 0.005,
            weight_clip: 0.75,
            reset_before_fit: true,
        },
        MetaTrainingConfig {
            epochs: 16,
            learning_rate: 0.02,
            l2: 0.01,
            weight_clip: 1.00,
            reset_before_fit: true,
        },
    ];

    let mut deduped = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !deduped.contains(&candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}

fn meta_calibration_report(
    train_markets: usize,
    training_samples: &[MetaTrainingSample],
    stats: &MetaTrainingStats,
    snapshot: &OnlineMetaCalibratorSnapshot,
    selected_training_config: Option<MetaTrainingConfig>,
    candidate_evaluations: Vec<MetaCandidateEvaluation>,
    raw_train_samples: usize,
    validation_samples: usize,
    validation: Option<MetaEvaluationSummary>,
    raw_validation_samples: usize,
    selected: bool,
    rejected_reason: Option<String>,
) -> MetaCalibrationReport {
    MetaCalibrationReport {
        train_markets,
        raw_train_samples,
        train_samples: training_samples.len(),
        train_updates: stats.updates,
        train_log_loss: Some(stats.log_loss),
        selected_training_config,
        candidate_evaluations,
        raw_validation_samples,
        validation_samples,
        validation,
        selected,
        rejected_reason,
        oos_samples: 0,
        oos_evaluation_samples: 0,
        oos: None,
        beta_enabled: snapshot.beta_enabled(),
        beta_coefficients: snapshot.beta_coefficients(),
        top_feature_weights: snapshot.top_feature_weights(12),
    }
}

fn evaluate_meta_calibration(
    snapshot: &OnlineMetaCalibratorSnapshot,
    samples: &[MetaTrainingSample],
) -> MetaEvaluationSummary {
    let calibrator = OnlineMetaCalibrator::from_snapshot(snapshot.clone());
    if samples.is_empty() {
        return MetaEvaluationSummary {
            samples: 0,
            market_count: 0,
            positive_rate: 0.0,
            market_equal_weighted_positive_rate: 0.0,
            base_distribution: PredictionDistribution::default(),
            calibrated_distribution: PredictionDistribution::default(),
            prior_log_loss: 0.0,
            base_log_loss: 0.0,
            calibrated_log_loss: 0.0,
            log_loss_delta: 0.0,
            prior_log_loss_delta: 0.0,
            prior_brier: 0.0,
            base_brier: 0.0,
            calibrated_brier: 0.0,
            brier_delta: 0.0,
            prior_brier_delta: 0.0,
            market_equal_weighted_prior_log_loss: 0.0,
            market_equal_weighted_base_log_loss: 0.0,
            market_equal_weighted_calibrated_log_loss: 0.0,
            market_equal_weighted_prior_brier: 0.0,
            market_equal_weighted_base_brier: 0.0,
            market_equal_weighted_calibrated_brier: 0.0,
            base_accuracy: 0.0,
            calibrated_accuracy: 0.0,
            calibrated_ece: 0.0,
            calibration_bins: Vec::new(),
        };
    }

    let mut base_log_loss = 0.0f32;
    let mut calibrated_log_loss = 0.0f32;
    let mut base_brier = 0.0f32;
    let mut calibrated_brier = 0.0f32;
    let mut base_correct = 0usize;
    let mut calibrated_correct = 0usize;
    let mut observed_count = 0usize;
    let mut base_predictions = Vec::with_capacity(samples.len());
    let mut calibrated_predictions = Vec::with_capacity(samples.len());
    let mut bins = [CalibrationBinAccumulator::default(); 10];
    let mut market_accumulators: std::collections::BTreeMap<u32, MarketCalibrationAccumulator> =
        std::collections::BTreeMap::new();

    for sample in samples {
        let observed = sample.side_observed;
        let target = if observed { 1.0 } else { 0.0 };
        observed_count += usize::from(observed);
        let base = sample.base_side_probability.clamp(1.0e-6, 1.0 - 1.0e-6);
        let calibrated = calibrator
            .predict_side_win_probability(base, &sample.features)
            .clamp(1.0e-6, 1.0 - 1.0e-6);

        base_log_loss += binary_log_loss(base, observed);
        calibrated_log_loss += binary_log_loss(calibrated, observed);
        base_brier += (base - target) * (base - target);
        calibrated_brier += (calibrated - target) * (calibrated - target);
        let market_acc = market_accumulators.entry(sample.market_idx).or_default();
        market_acc.samples += 1;
        market_acc.observed += usize::from(observed);
        market_acc.base_log_loss += binary_log_loss(base, observed);
        market_acc.calibrated_log_loss += binary_log_loss(calibrated, observed);
        market_acc.base_brier += (base - target) * (base - target);
        market_acc.calibrated_brier += (calibrated - target) * (calibrated - target);
        base_predictions.push(base);
        calibrated_predictions.push(calibrated);
        base_correct += usize::from((base >= 0.5) == observed);
        calibrated_correct += usize::from((calibrated >= 0.5) == observed);
        let bin = ((calibrated * bins.len() as f32).floor() as usize).min(bins.len() - 1);
        bins[bin].samples += 1;
        bins[bin].sum_predicted += calibrated;
        bins[bin].observed += usize::from(observed);
    }

    let n = samples.len() as f32;
    let positive_rate = observed_count as f32 / n;
    let prior = positive_rate.clamp(1.0e-6, 1.0 - 1.0e-6);
    let prior_log_loss = -(positive_rate * prior.ln() + (1.0 - positive_rate) * (1.0 - prior).ln());
    let prior_brier = samples
        .iter()
        .map(|sample| {
            let target = if sample.side_observed { 1.0 } else { 0.0 };
            (prior - target) * (prior - target)
        })
        .sum::<f32>()
        / n;
    let base_log_loss = base_log_loss / n;
    let calibrated_log_loss = calibrated_log_loss / n;
    let base_brier = base_brier / n;
    let calibrated_brier = calibrated_brier / n;
    let market_count = market_accumulators.len();
    let mut market_positive_rate = 0.0f32;
    let mut market_prior_log_loss = 0.0f32;
    let mut market_base_log_loss = 0.0f32;
    let mut market_calibrated_log_loss = 0.0f32;
    let mut market_prior_brier = 0.0f32;
    let mut market_base_brier = 0.0f32;
    let mut market_calibrated_brier = 0.0f32;
    for market in market_accumulators.values() {
        let market_n = market.samples.max(1) as f32;
        let observed_rate = market.observed as f32 / market_n;
        market_positive_rate += observed_rate;
        market_prior_log_loss +=
            -(observed_rate * prior.ln() + (1.0 - observed_rate) * (1.0 - prior).ln());
        market_base_log_loss += market.base_log_loss / market_n;
        market_calibrated_log_loss += market.calibrated_log_loss / market_n;
        market_prior_brier +=
            observed_rate * (1.0 - prior) * (1.0 - prior) + (1.0 - observed_rate) * prior * prior;
        market_base_brier += market.base_brier / market_n;
        market_calibrated_brier += market.calibrated_brier / market_n;
    }
    let market_denom = market_count.max(1) as f32;
    let market_positive_rate = market_positive_rate / market_denom;
    let market_prior_log_loss = market_prior_log_loss / market_denom;
    let market_base_log_loss = market_base_log_loss / market_denom;
    let market_calibrated_log_loss = market_calibrated_log_loss / market_denom;
    let market_prior_brier = market_prior_brier / market_denom;
    let market_base_brier = market_base_brier / market_denom;
    let market_calibrated_brier = market_calibrated_brier / market_denom;
    let mut calibrated_ece = 0.0f32;
    let calibration_bins: Vec<CalibrationBin> = bins
        .iter()
        .enumerate()
        .filter_map(|(idx, bin)| {
            if bin.samples == 0 {
                return None;
            }
            let samples_f = bin.samples as f32;
            let avg_predicted = bin.sum_predicted / samples_f;
            let observed_rate = bin.observed as f32 / samples_f;
            calibrated_ece += (samples_f / n) * (avg_predicted - observed_rate).abs();
            Some(CalibrationBin {
                lower: idx as f32 / 10.0,
                upper: (idx + 1) as f32 / 10.0,
                samples: bin.samples,
                avg_predicted,
                observed_rate,
            })
        })
        .collect();
    MetaEvaluationSummary {
        samples: samples.len(),
        market_count,
        positive_rate,
        market_equal_weighted_positive_rate: market_positive_rate,
        base_distribution: prediction_distribution(&mut base_predictions),
        calibrated_distribution: prediction_distribution(&mut calibrated_predictions),
        prior_log_loss,
        base_log_loss,
        calibrated_log_loss,
        log_loss_delta: calibrated_log_loss - base_log_loss,
        prior_log_loss_delta: calibrated_log_loss - prior_log_loss,
        prior_brier,
        base_brier,
        calibrated_brier,
        brier_delta: calibrated_brier - base_brier,
        prior_brier_delta: calibrated_brier - prior_brier,
        market_equal_weighted_prior_log_loss: market_prior_log_loss,
        market_equal_weighted_base_log_loss: market_base_log_loss,
        market_equal_weighted_calibrated_log_loss: market_calibrated_log_loss,
        market_equal_weighted_prior_brier: market_prior_brier,
        market_equal_weighted_base_brier: market_base_brier,
        market_equal_weighted_calibrated_brier: market_calibrated_brier,
        base_accuracy: base_correct as f32 / n,
        calibrated_accuracy: calibrated_correct as f32 / n,
        calibrated_ece,
        calibration_bins,
    }
}

fn prediction_distribution(predictions: &mut [f32]) -> PredictionDistribution {
    if predictions.is_empty() {
        return PredictionDistribution::default();
    }
    predictions.sort_by(|a, b| a.total_cmp(b));
    let n = predictions.len();
    let mean = predictions.iter().sum::<f32>() / n as f32;
    let percentile = |q: f32| -> f32 {
        let idx = ((n - 1) as f32 * q).round() as usize;
        predictions[idx.min(n - 1)]
    };
    let share_ge = |threshold: f32| -> f32 {
        let count = predictions.iter().filter(|p| **p >= threshold).count();
        count as f32 / n as f32
    };
    PredictionDistribution {
        mean,
        p10: percentile(0.10),
        p50: percentile(0.50),
        p90: percentile(0.90),
        share_ge_55: share_ge(0.55),
        share_ge_60: share_ge(0.60),
        share_ge_65: share_ge(0.65),
        share_ge_70: share_ge(0.70),
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CalibrationBinAccumulator {
    samples: usize,
    sum_predicted: f32,
    observed: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct MarketCalibrationAccumulator {
    samples: usize,
    observed: usize,
    base_log_loss: f32,
    calibrated_log_loss: f32,
    base_brier: f32,
    calibrated_brier: f32,
}

fn binary_log_loss(p: f32, observed: bool) -> f32 {
    let p = p.clamp(1.0e-6, 1.0 - 1.0e-6);
    if observed { -p.ln() } else { -(1.0 - p).ln() }
}

async fn collect_training_samples(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
) -> Result<Vec<MetaTrainingSample>> {
    if markets.is_empty() {
        return Ok(Vec::new());
    }

    let store_inner = store.store();
    let empty_spot = Arc::new(SpotHistory::default());
    let total = markets.len();
    let completed = Arc::new(AtomicUsize::new(0));
    let concurrency = cfg.max_concurrent_fetches.max(1);
    tracing::info!(markets = total, concurrency, "collecting training samples");

    let mut indexed_samples =
        futures::stream::iter(markets.iter().cloned().enumerate().map(|(idx, m)| {
            let store = store.clone();
            let store_inner = store_inner.clone();
            let spot = spot_history_for_market(spot_map, &empty_spot, cfg, &m);
            let use_outcome_label = cfg.use_outcome_label;
            let starting_cash_usdc = cfg.starting_cash_usdc;
            let replay_sample_ms = cfg.replay_sample_ms;
            let replay_event_cache_dir = cfg.replay_event_cache_dir.clone();
            let enable_market_context_features = cfg.enable_market_context_features;
            let completed = completed.clone();
            async move {
                let samples = collect_training_samples_for_market(
                    &store,
                    store_inner,
                    idx,
                    &m,
                    spot,
                    use_outcome_label,
                    starting_cash_usdc,
                    replay_sample_ms,
                    replay_event_cache_dir.as_deref(),
                    enable_market_context_features,
                )
                .await;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                if done % 100 == 0 || done == total {
                    tracing::info!(done, total, "training sample progress");
                }
                (idx, samples)
            }
        }))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;

    indexed_samples.sort_by_key(|(idx, _)| *idx);
    let mut samples = Vec::with_capacity(markets.len());
    for (_, market_samples) in indexed_samples {
        samples.extend(market_samples);
    }
    Ok(samples)
}

async fn collect_training_samples_for_market(
    store: &TelonexStore,
    store_inner: Arc<dyn object_store::ObjectStore>,
    idx: usize,
    m: &MarketHandle,
    spot: Arc<SpotHistory>,
    use_outcome_label: bool,
    starting_cash_usdc: f64,
    replay_sample_ms: u64,
    replay_event_cache_dir: Option<&Path>,
    enable_market_context_features: bool,
) -> Vec<MetaTrainingSample> {
    let events = match load_replay_events_for_market(
        store,
        store_inner,
        m,
        MarketId(idx as u32 + 1),
        replay_event_cache_dir,
    )
    .await
    {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(market = %m.slug, error = %e, "training tape load failed");
            return Vec::new();
        }
    };
    let sampled_events;
    let events_for_run = if replay_sample_ms > 0 {
        sampled_events = sample_replay_events(&events, replay_sample_ms);
        sampled_events.as_slice()
    } else {
        events.as_slice()
    };
    let resolved_yes = if use_outcome_label {
        Some(
            outcome_label_resolved_yes(&m.outcome)
                .expect("outcome labels are validated before replay"),
        )
    } else {
        None
    };
    let runner_cfg = RunnerConfig {
        starting_cash_usdc,
        market_open_ns: market_open_ns(m),
        market_close_ns: market_close_ns(m),
        resolved_yes,
        portfolio_limits: PortfolioLimits::default(),
        equity_curve_jsonl: None,
        snapshot_every_n: 1_000_000,
        maker_rebate_bps: 0.0,
        taker_fee_bps: 0.0,
        decision_log_jsonl: None,
        decision_log_parquet: None,
        strategy_name: "meta_training".to_string(),
        shared_model_state: None,
        update_model_state_on_resolution: true,
        meta_calibrator_snapshot: None,
        enable_meta_calibration: true,
        current_btc_net_shares: 0.0,
        current_eth_net_shares: 0.0,
        daily_start_cash_usdc: 0.0,
        daily_loss_cap_pct: 1.0,
        model_market_context: if enable_market_context_features {
            model_market_context_for_slug(&m.slug)
        } else {
            ModelMarketContext::default()
        },
        prior_market_range_1d: 0.0,
        prior_market_range_3d: 0.0,
        prior_market_range_7d: 0.0,
        model_btc_whipsaw_risk_weight: 0.16,
        model_btc_path_inefficiency_risk_weight: 0.10,
        model_btc_reversal_pressure_risk_weight: 0.12,
        decision_log_every_n: 1_000_000,
        max_inventory_imbalance_shares: 1.5,
        taker_slippage_bps: 0.0,
        taker_latency_ms: 0,
        enforce_model_gate: false,
        model_gate_min_confidence: 0.68,
        model_gate_max_risk: 0.72,
        model_gate_min_edge: 0.05,
        current_daily_loss_pct: 0.0,
    };
    let mut strat = NoopStrategy;
    match run_backtest(
        events_for_run,
        &spot,
        &TradeHistory::default(),
        &mut strat,
        &runner_cfg,
    ) {
        Ok(report) => {
            let mut samples = report.model_training_samples;
            for sample in &mut samples {
                sample.market_idx = idx as u32;
            }
            samples
        }
        Err(e) => {
            tracing::warn!(market = %m.slug, error = %e, "training sample extraction failed");
            Vec::new()
        }
    }
}

async fn run_markets(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
    market_id_offset: usize,
    max_concurrent_fetches: usize,
    meta_calibrator_snapshot: Option<OnlineMetaCalibratorSnapshot>,
) -> Result<Vec<MarketResult>> {
    if markets.is_empty() {
        return Ok(Vec::new());
    }

    let spot_empty = Arc::new(SpotHistory::default());
    let store_inner = store.store();
    let cfg_arc = Arc::new(cfg.clone());
    let bte_policy_scales = cfg
        .back_to_explore_policy_scales_jsonl
        .as_deref()
        .map(load_bte_policy_scales)
        .transpose()?
        .map(Arc::new);

    // Phase 1: bounded async I/O — only load raw data + build runner config.
    // No strategy execution here.
    let load_stream = futures::stream::iter(markets.iter().enumerate().map(|(idx, m)| {
        let store_inner = store_inner.clone();
        let spot_map = spot_map.clone();
        let spot_empty = spot_empty.clone();
        let cfg_arc = cfg_arc.clone();
        let meta_calibrator_snapshot = meta_calibrator_snapshot.clone();
        let store_for_resolve = store.clone();

        async move {
            let market_id = MarketId((idx + market_id_offset) as u32 + 1);
            let events = match load_replay_events_for_market(
                &store_for_resolve,
                store_inner.clone(),
                &m,
                market_id,
                cfg_arc.replay_event_cache_dir.as_deref(),
            )
            .await
            {
                Ok(events) => events,
                Err(e) => {
                    tracing::warn!(market = %m.slug, error = %e, "tape load failed");
                    return None;
                }
            };

            let sampled_events;
            let events_for_run = if cfg_arc.replay_sample_ms > 0 {
                sampled_events = sample_replay_events(&events, cfg_arc.replay_sample_ms);
                sampled_events.as_slice().to_vec()
            } else {
                events.clone()
            };

            let trades = if cfg_arc.load_pm_trades {
                match resolve_pm_trades_day(&store_for_resolve, &m.date, &m.asset_id).await {
                    Ok(tp) => match load_pm_trades_async(store_inner.clone(), tp).await {
                        Ok((ticks, _)) => Arc::new(TradeHistory::new(ticks)),
                        Err(e) => {
                            tracing::debug!(market = %m.slug, error = %e, "trades load failed");
                            Arc::new(TradeHistory::default())
                        }
                    },
                    Err(_) => Arc::new(TradeHistory::default()),
                }
            } else {
                Arc::new(TradeHistory::default())
            };

            let spot = spot_history_for_market(&spot_map, &spot_empty, &cfg_arc, &m);

            let resolved_yes = if cfg_arc.use_outcome_label {
                Some(
                    outcome_label_resolved_yes(&m.outcome)
                        .expect("outcome labels are validated before replay"),
                )
            } else {
                None
            };

            let runner_cfg = RunnerConfig {
                starting_cash_usdc: cfg_arc.starting_cash_usdc,
                market_open_ns: market_open_ns(&m),
                market_close_ns: market_close_ns(&m),
                resolved_yes,
                portfolio_limits: PortfolioLimits {
                    max_clip_usdc: cfg_arc.max_clip_usdc * cfg_arc.max_order_clip_multiplier,
                    max_per_market_exposure_usdc: per_market_exposure_cap(
                        &cfg_arc,
                        cfg_arc.starting_cash_usdc,
                    ),
                    ..PortfolioLimits::default()
                },
                equity_curve_jsonl: None,
                snapshot_every_n: 1_000_000,
                maker_rebate_bps: cfg_arc.maker_rebate_bps,
                taker_fee_bps: cfg_arc.taker_fee_bps,
                decision_log_jsonl: None,
                decision_log_parquet: None,
                strategy_name: "parallel_walkforward".to_string(),
                shared_model_state: None,
                update_model_state_on_resolution: meta_calibrator_snapshot.is_none(),
                meta_calibrator_snapshot,
                enable_meta_calibration: cfg_arc.enable_meta_calibration,
                model_market_context: model_market_context_for_cfg(&cfg_arc, &m),
                prior_market_range_1d: 0.0,
                prior_market_range_3d: 0.0,
                prior_market_range_7d: 0.0,
                model_btc_whipsaw_risk_weight: cfg_arc.model_btc_whipsaw_risk_weight,
                model_btc_path_inefficiency_risk_weight: cfg_arc
                    .model_btc_path_inefficiency_risk_weight,
                model_btc_reversal_pressure_risk_weight: cfg_arc
                    .model_btc_reversal_pressure_risk_weight,
                decision_log_every_n: 1_000_000,
                max_inventory_imbalance_shares: 1.5,
                taker_slippage_bps: 15.0,
                taker_latency_ms: cfg_arc.taker_latency_ms,
                enforce_model_gate: cfg_arc.enforce_model_gate,
                model_gate_min_confidence: cfg_arc.model_gate_min_confidence,
                model_gate_max_risk: cfg_arc.model_gate_max_risk,
                model_gate_min_edge: cfg_arc.model_gate_min_edge,
                current_btc_net_shares: 0.0,
                current_eth_net_shares: 0.0,
                daily_start_cash_usdc: 0.0,
                daily_loss_cap_pct: 1.0,
                current_daily_loss_pct: 0.0,
            };

            Some((m.clone(), events_for_run, spot, trades, runner_cfg, idx))
        }
    }))
    .buffer_unordered(max_concurrent_fetches);

    let mut loaded: Vec<_> = Vec::new();
    let mut load_stream = std::pin::pin!(load_stream);
    while let Some(item) = load_stream.next().await {
        if let Some(x) = item {
            loaded.push(x);
        }
    }

    let mut results = Vec::with_capacity(loaded.len());

    if cfg.portfolio_mode {
        loaded.sort_by_key(|(_, _, _, _, _, idx)| *idx);

        // Cross-market ladder exposure tracking (for BackToExplore etc.)
        // We maintain committed net per asset from markets that have already resolved.
        // When starting a new market we pass the current ladder state so the strategy
        // can see exposure across other simultaneous windows.
        let mut asset_net: std::collections::HashMap<String, f64> =
            std::collections::HashMap::new();

        // Must stay strictly serial — each market's starting equity depends on previous.
        for (m, events_for_run, spot, trades, runner_cfg, idx) in loaded {
            let asset = if m.slug.starts_with("btc-updown-") || m.slug.starts_with("bitcoin-") {
                "btc"
            } else if m.slug.starts_with("eth-updown-") || m.slug.starts_with("ethereum-") {
                "eth"
            } else {
                "other"
            };

            let btc_net = *asset_net.get("btc").unwrap_or(&0.0);
            let eth_net = *asset_net.get("eth").unwrap_or(&0.0);

            // Create a per-market runner config with the current ladder state injected.
            let mut market_runner_cfg = runner_cfg.clone();
            market_runner_cfg.current_btc_net_shares = btc_net;
            market_runner_cfg.current_eth_net_shares = eth_net;
            let bte_policy_scale = bte_policy_scale_for(bte_policy_scales.as_deref(), &m);

            let mut per_strategy = HashMap::new();
            for &strat in &cfg.strategies {
                match run_one_strategy(
                    strat,
                    cfg,
                    &events_for_run,
                    &spot,
                    &trades,
                    &market_runner_cfg,
                    cfg.starting_cash_usdc,
                    cfg.max_clip_usdc,
                    None,
                    None,
                    if strat == StratId::BackToExplore {
                        bte_policy_scale
                    } else {
                        1.0
                    },
                ) {
                    Ok(mut r) => {
                        for sample in &mut r.model_training_samples {
                            sample.market_idx = (idx + market_id_offset) as u32;
                        }
                        per_strategy.insert(strat.name(), r);
                    }
                    Err(e) => {
                        tracing::warn!(market = %m.slug, strategy = strat.name(), error = %e, "strategy run failed");
                    }
                }
            }
            let volatility_range = market_volatility_range(&events_for_run);
            let volatility_band =
                volatility_band(volatility_range, cfg.volatility_regime_threshold);
            let close_ts = market_close_ts(&m);

            // Update committed ladder exposure from this market's fills (for future markets).
            if let Some(r) = per_strategy.get("back_to_explore") {
                let mut delta = 0.0;
                for fill in &r.fills_detail {
                    let signed = match fill.side.as_str() {
                        "BuyYes" | "SellNo" => fill.shares,
                        "BuyNo" | "SellYes" => -fill.shares,
                        _ => 0.0,
                    };
                    delta += signed;
                }
                *asset_net.entry(asset.to_string()).or_insert(0.0) += delta;
            }

            results.push(MarketResult {
                asset_id: m.asset_id,
                slug: m.slug,
                close_ts,
                outcome_label: m.outcome,
                volatility_range,
                volatility_band,
                per_strategy,
            });
        }
    } else {
        // Phase 2: full parallel execution across markets/days using rayon.
        // This is the main grid-scale win for independent (non-portfolio) runs.
        use rayon::prelude::*;

        let parallel_results: Vec<_> = loaded
            .into_par_iter()
            .map(|(m, events_for_run, spot, trades, runner_cfg, idx)| {
                let mut per_strategy = HashMap::new();
                let bte_policy_scale = bte_policy_scale_for(bte_policy_scales.as_deref(), &m);
                for &strat in &cfg.strategies {
                    match run_one_strategy(
                        strat,
                        cfg,
                        &events_for_run,
                        &spot,
                        &trades,
                        &runner_cfg,
                        cfg.starting_cash_usdc,
                        cfg.max_clip_usdc,
                        None,
                        None,
                        if strat == StratId::BackToExplore {
                            bte_policy_scale
                        } else {
                            1.0
                        },
                    ) {
                        Ok(mut r) => {
                            for sample in &mut r.model_training_samples {
                                sample.market_idx = (idx + market_id_offset) as u32;
                            }
                            per_strategy.insert(strat.name(), r);
                        }
                        Err(e) => {
                            tracing::warn!(market = %m.slug, strategy = strat.name(), error = %e, "strategy run failed");
                        }
                    }
                }
                let volatility_range = market_volatility_range(&events_for_run);
                let volatility_band =
                    volatility_band(volatility_range, cfg.volatility_regime_threshold);
                let close_ts = market_close_ts(&m);

                MarketResult {
                    asset_id: m.asset_id,
                    slug: m.slug,
                    close_ts,
                    outcome_label: m.outcome,
                    volatility_range,
                    volatility_band,
                    per_strategy,
                }
            })
            .collect();

        results.extend(parallel_results);
    }

    results.sort_by_key(|r| r.close_ts);
    Ok(results)
}

fn run_one_strategy(
    strat: StratId,
    cfg: &WalkForwardConfig,
    events: &[pm_types::ReplayEvent],
    spot: &SpotHistory,
    trades: &TradeHistory,
    runner_cfg: &RunnerConfig,
    bankroll: f64,
    clip: f64,
    shared_skew_table: Option<Arc<Mutex<SkewWinRateTable>>>,
    shared_model_state: Option<Arc<Mutex<ModelState>>>,
    bte_external_risk_multiplier: f64,
) -> Result<StrategyMarketResult> {
    let (report, bonereaper_v2_gate_stats) = match strat {
        StratId::BuyYesAtOpen => {
            let mut s = BuyYesAtOpen::new(10.0);
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::ReactiveDirectional => {
            let mut s = ReactiveDirectional::new(ReactiveDirectionalConfig {
                bankroll_usdc: bankroll,
                kelly_fraction: cfg.kelly_fraction,
                max_clip_usdc: clip,
                early_pair_clip_usdc: 0.5,
                conviction_threshold_yes: 0.68,
                conviction_threshold_no: 0.68,
                book_weight: 0.3,
                spot_weight: 0.7,
                shared_model_state,
                shared_skew_table,
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::PairedMm => {
            let mut s = PairedMmDense::new(PairedMmDenseConfig {
                clip_shares: (clip * 0.3 / 5.0).max(0.05), // scale with clip
                max_rungs_per_leg: 3,
                max_entry_pair_cost: 1.05,
                max_leg_imbalance_shares: 0.6,
                min_refresh_ns: 2_000_000_000,
                ..PairedMmDenseConfig::default()
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::SpotMomentumFollower => {
            let mut s = SpotMomentumFollower::new(SpotMomentumFollowerConfig {
                clip_usdc: clip,
                ..SpotMomentumFollowerConfig::default()
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::LateBigBet => {
            let mut s = LateBigBet::new(LateBigBetConfig {
                bankroll_usdc: bankroll,
                kelly_fraction: 0.5,
                max_clip_usdc: clip,
                late_seconds: 60.0,
                min_conviction: 0.15,
                max_ask_yes: 0.94,
                min_bid_yes: 0.06,
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::BonereaperLite => {
            let mut s = BonereaperLite::new(BonereaperLiteConfig {
                bankroll_usdc: bankroll,
                max_clip_usdc: clip,
                ..BonereaperLiteConfig::default()
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::DeltaNeutralMm => {
            let mut s = DeltaNeutralMm::new(DeltaNeutralMmConfig {
                clip_shares: (clip * 0.3).max(0.1),
                max_pair_cost: 1.02,
                max_inventory_delta_shares: 1.0,
                ..DeltaNeutralMmConfig::default()
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::UnlawfulRecycler => {
            let mut s = UnlawfulRecycler::new(UnlawfulRecyclerConfig {
                clip_shares: (clip * 0.30).clamp(1.0, 25.0),
                max_pair_cost: 0.99,
                max_inventory_delta_shares: (clip * 0.80).clamp(10.0, 60.0),
                repair_inventory_delta_shares: (clip * 0.20).clamp(3.0, 15.0),
                min_refresh_ns: 250_000_000,
                max_orders_per_leg: 500,
                stop_secs_before_close: 20.0,
                ..UnlawfulRecyclerConfig::default()
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::BonereaperV2 => {
            let reversal_score_coeffs = match &cfg.br2_reversal_score_coeffs_path {
                Some(path) => {
                    let content = std::fs::read_to_string(path).with_context(|| {
                        format!("failed to read reversal-score coeffs {}", path.display())
                    })?;
                    let coeffs: ReversalScoreCoeffs =
                        serde_json::from_str(&content).with_context(|| {
                            format!(
                                "failed to parse reversal-score coeffs {} as JSON",
                                path.display()
                            )
                        })?;
                    Some(coeffs)
                }
                None => None,
            };
            let mut s = BonereaperV2::new(BonereaperV2Config {
                bankroll_usdc: bankroll,
                max_clip_usdc: clip,
                disable_internal_model_gates: cfg.br2_disable_internal_model_gates,
                participation_clip_frac: cfg.br2_participation_clip_frac,
                participation_max_pair_cost: cfg.br2_participation_max_pair_cost,
                participation_max_orders_per_leg: cfg.br2_participation_max_orders_per_leg,
                participation_max_inventory_delta_shares: cfg
                    .br2_participation_max_inventory_delta_shares,
                participation_repair_inventory_delta_shares: cfg
                    .br2_participation_repair_inventory_delta_shares,
                participation_refresh_secs: cfg.br2_participation_refresh_secs,
                participation_stop_secs_before_close: cfg.br2_participation_stop_secs_before_close,
                hedged_base_enabled: cfg.br2_hedged_base_enabled,
                hedged_base_max_secs_in: cfg.br2_hedged_base_max_secs_in,
                hedged_base_max_pair_cost: cfg.br2_hedged_base_max_pair_cost,
                hedged_base_min_minority_leg_frac: cfg.br2_hedged_base_min_minority_leg_frac,
                hedged_base_clip_usdc: cfg.br2_hedged_base_clip_usdc,
                hedged_base_max_notional_usdc: cfg.br2_hedged_base_max_notional_usdc,
                late_directional_overlay_frac: cfg.br2_late_directional_overlay_frac,
                min_composite_direction: cfg.br2_min_composite_direction,
                early_clip_frac: cfg.br2_early_clip_frac,
                mid_clip_frac: cfg.br2_mid_clip_frac,
                late_clip_frac: cfg.br2_late_clip_frac,
                late_max_fires: cfg.br2_late_max_fires,
                late_confirm_min_model_confidence: cfg.br2_late_confirm_min_model_confidence,
                late_confirm_max_model_risk: cfg.br2_late_confirm_max_model_risk,
                late_confirm_min_model_side_p: cfg.br2_late_confirm_min_model_side_p,
                late_confirm_min_model_edge: cfg.br2_late_confirm_min_model_edge,
                late_confirm_min_book_skew: cfg.br2_late_confirm_min_book_skew,
                late_confirm_max_whipsaw_score: cfg.br2_late_confirm_max_whipsaw_score,
                late_confirm_min_realized_vol_180s_bps: cfg
                    .br2_late_confirm_min_realized_vol_180s_bps,
                late_confirm_max_observed_range: cfg.br2_late_confirm_max_observed_range,
                recent_regime_gate_enabled: cfg.br2_recent_regime_gate_enabled,
                recent_regime_gate_min_edge: cfg.br2_recent_regime_gate_min_edge,
                recent_regime_gate_late_confirm: cfg.br2_recent_regime_gate_late_confirm,
                recent_regime_gate_high_skew: cfg.br2_recent_regime_gate_high_skew,
                recent_regime_gate_late_favourite: cfg.br2_recent_regime_gate_late_favourite,
                high_skew_clip_frac: cfg.br2_high_skew_clip_frac,
                lane_size_late_favourite: cfg.br2_lane_size_late_favourite,
                lane_size_late_confirm: cfg.br2_lane_size_late_confirm,
                lane_size_high_skew: cfg.br2_lane_size_high_skew,
                regime_gate_enabled: cfg.br2_regime_gate_enabled,
                regime_gate_window: cfg.br2_regime_gate_window,
                regime_gate_threshold: cfg.br2_regime_gate_threshold,
                regime_gate_soft_band: cfg.br2_regime_gate_soft_band,
                regime_gate_lane_floor: cfg.br2_regime_gate_lane_floor,
                regime_gate_whipsaw_weight: cfg.br2_regime_gate_whipsaw_weight,
                high_skew_max_clips: cfg.br2_high_skew_max_clips,
                high_skew_max_whipsaw_score: cfg.br2_high_skew_max_whipsaw_score,
                high_skew_min_realized_vol_180s_bps: cfg.br2_high_skew_min_realized_vol_180s_bps,
                late_favourite_start_secs: cfg.br2_late_favourite_start_secs,
                late_favourite_threshold: cfg.br2_late_favourite_threshold,
                late_favourite_min_ask: cfg.br2_late_favourite_min_ask,
                late_favourite_max_ask: cfg.br2_late_favourite_max_ask,
                late_favourite_clip_frac: cfg.br2_late_favourite_clip_frac,
                late_favourite_high_cert_clip_frac: cfg.br2_late_favourite_high_cert_clip_frac,
                late_favourite_high_cert_full_clip_edge: cfg
                    .br2_late_favourite_high_cert_full_clip_edge,
                late_favourite_fragile_high_cert_ask: cfg.br2_late_favourite_fragile_high_cert_ask,
                late_favourite_fragile_high_cert_max_edge: cfg
                    .br2_late_favourite_fragile_high_cert_max_edge,
                late_favourite_fragile_high_cert_max_path_efficiency: cfg
                    .br2_late_favourite_fragile_high_cert_max_path_efficiency,
                late_favourite_fragile_high_cert_size_frac: cfg
                    .br2_late_favourite_fragile_high_cert_size_frac,
                late_favourite_max_clips: cfg.br2_late_favourite_max_clips,
                late_favourite_min_sustain_secs: cfg.br2_late_favourite_min_sustain_secs,
                late_favourite_sweep_depth: cfg.br2_late_favourite_sweep_depth,
                late_favourite_min_model_confidence: cfg.br2_late_favourite_min_model_confidence,
                late_favourite_min_model_direction_abs: cfg
                    .br2_late_favourite_min_model_direction_abs,
                late_favourite_max_model_risk: cfg.br2_late_favourite_max_model_risk,
                late_favourite_min_model_side_p: cfg.br2_late_favourite_min_model_side_p,
                late_favourite_min_model_edge: cfg.br2_late_favourite_min_model_edge,
                late_favourite_high_cert_min_model_edge: cfg
                    .br2_late_favourite_high_cert_min_model_edge,
                late_favourite_high_cert_bypass_model_edge: cfg
                    .br2_late_favourite_high_cert_bypass_model_edge,
                late_favourite_max_whipsaw_score: cfg.br2_late_favourite_max_whipsaw_score,
                late_favourite_max_reversal_pressure: cfg.br2_late_favourite_max_reversal_pressure,
                late_favourite_min_path_efficiency: cfg.br2_late_favourite_min_path_efficiency,
                late_favourite_min_realized_vol_180s_bps: cfg
                    .br2_late_favourite_min_realized_vol_180s_bps,
                late_favourite_max_observed_range: cfg.br2_late_favourite_max_observed_range,
                late_favourite_range_soft_throttle: cfg.br2_late_favourite_range_soft_throttle,
                late_favourite_range_hard_throttle: cfg.br2_late_favourite_range_hard_throttle,
                late_favourite_range_extra_edge: cfg.br2_late_favourite_range_extra_edge,
                late_favourite_range_extra_confidence: cfg
                    .br2_late_favourite_range_extra_confidence,
                late_favourite_max_adverse_fast_momentum: cfg
                    .br2_late_favourite_max_adverse_fast_momentum,
                late_favourite_max_adverse_broad_momentum: cfg
                    .br2_late_favourite_max_adverse_broad_momentum,
                late_favourite_max_entry_pullback: cfg.br2_late_favourite_max_entry_pullback,
                late_favourite_max_avg_entry_drawdown: cfg
                    .br2_late_favourite_max_avg_entry_drawdown,
                tail_clip_frac: cfg.br2_tail_clip_frac,
                tail_max_clips: cfg.br2_tail_max_clips,
                tail_sweep_depth: cfg.br2_tail_sweep_depth,
                tail_min_ask: cfg.br2_tail_min_ask,
                tail_max_ask: cfg.br2_tail_max_ask,
                tail_min_seconds_to_close: cfg.br2_tail_min_seconds_to_close,
                tail_min_favourite_unrealized_edge: cfg.br2_tail_min_favourite_unrealized_edge,
                tail_min_observed_range: cfg.br2_tail_min_observed_range,
                tail_target_favourite_loss_coverage_frac: cfg
                    .br2_tail_target_favourite_loss_coverage_frac,
                tail_reversal_coverage_frac: cfg.br2_tail_reversal_coverage_frac,
                tail_reversal_min_seconds_to_close: cfg.br2_tail_reversal_min_seconds_to_close,
                tail_reversal_max_seconds_to_close: cfg.br2_tail_reversal_max_seconds_to_close,
                tail_reversal_min_favourite_ask: cfg.br2_tail_reversal_min_favourite_ask,
                tail_extreme_threshold: cfg.br2_tail_extreme_threshold,
                tail_min_skew_step: cfg.br2_tail_min_skew_step,
                tail_budget_favourite_spend_frac: cfg.br2_tail_budget_favourite_spend_frac,
                tail_budget_favourite_upside_frac: cfg.br2_tail_budget_favourite_upside_frac,
                tail_regime_boost_coverage_frac: cfg.br2_tail_regime_boost_coverage_frac,
                tail_regime_boost_budget_spend_frac: cfg.br2_tail_regime_boost_budget_spend_frac,
                tail_regime_boost_budget_upside_frac: cfg.br2_tail_regime_boost_budget_upside_frac,
                tail_regime_boost_min_whipsaw_score: cfg.br2_tail_regime_boost_min_whipsaw_score,
                tail_regime_boost_min_reversal_pressure: cfg
                    .br2_tail_regime_boost_min_reversal_pressure,
                tail_regime_boost_min_realized_vol_180s_bps: cfg
                    .br2_tail_regime_boost_min_realized_vol_180s_bps,
                tail_regime_boost_max_path_efficiency: cfg
                    .br2_tail_regime_boost_max_path_efficiency,
                reversal_score_enabled: cfg.br2_reversal_score_enabled,
                reversal_score_coeffs,
                reversal_score_cov_min: cfg.br2_reversal_score_cov_min,
                reversal_score_cov_max: cfg.br2_reversal_score_cov_max,
                reversal_score_size_floor: cfg.br2_reversal_score_size_floor,
                reversal_score_size_ceiling: cfg.br2_reversal_score_size_ceiling,
                ..BonereaperV2Config::default()
            });
            let report = run_backtest(events, spot, trades, &mut s, runner_cfg)?;
            (report, Some(s.gate_stats()))
        }
        StratId::LateConfirmation => {
            let mut s = LateConfirmation::new(LateConfirmationConfig {
                bankroll_usdc: bankroll,
                max_clip_usdc: clip,
                ..LateConfirmationConfig::default()
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::LateConvexTail => {
            let mut s = LateConvexTail::new(LateConvexTailConfig {
                bankroll_usdc: bankroll,
                max_clip_usdc: clip * 0.2,
                ..LateConvexTailConfig::default()
            });
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
        StratId::BackToExplore => {
            let market_window_ns = runner_cfg
                .market_close_ns
                .saturating_sub(runner_cfg.market_open_ns)
                .max(1);
            let bte_cfg = cfg.build_back_to_explore_config(
                clip,
                market_window_ns,
                bte_external_risk_multiplier,
            );
            let mut s = BackToExploreTaker::new(bte_cfg);
            (
                run_backtest(events, spot, trades, &mut s, runner_cfg)?,
                None,
            )
        }
    };
    let filled_notional_usdc = report.filled_notional_usdc;
    let avg_slippage_bps = if filled_notional_usdc > 0.0 {
        report
            .fills
            .iter()
            .map(|fill| fill.slippage_bps as f64 * fill.notional)
            .sum::<f64>()
            / filled_notional_usdc
    } else {
        0.0
    };
    Ok(StrategyMarketResult {
        orders_submitted: report.counters.orders_submitted,
        orders_filled: report.counters.orders_filled_taker + report.counters.orders_filled_maker,
        orders_filled_taker: report.counters.orders_filled_taker,
        orders_filled_maker: report.counters.orders_filled_maker,
        orders_rejected_model_gate: report.counters.orders_rejected_model_gate,
        orders_rejected_model_gate_confidence: report
            .counters
            .orders_rejected_model_gate_confidence,
        orders_rejected_model_gate_risk: report.counters.orders_rejected_model_gate_risk,
        orders_rejected_model_gate_edge: report.counters.orders_rejected_model_gate_edge,
        pnl_usdc: report.pnl_usdc,
        start_equity_usdc: bankroll,
        end_equity_usdc: report.end_equity_usdc,
        max_drawdown_pct: report.max_drawdown_pct,
        fills: report.fills.len(),
        maker_rebates_usdc: report.maker_rebates_usdc,
        requested_shares: report.requested_shares,
        filled_shares: report.filled_shares,
        fill_shares_ratio: if report.requested_shares > 0.0 {
            report.filled_shares / report.requested_shares
        } else {
            0.0
        },
        requested_notional_usdc: report.requested_notional_usdc,
        filled_notional_usdc,
        fill_notional_ratio: if report.requested_notional_usdc > 0.0 {
            filled_notional_usdc / report.requested_notional_usdc
        } else {
            0.0
        },
        avg_slippage_bps,
        clip_used_usdc: clip,
        yes_resolved: report.yes_resolved,
        fills_detail: report.fills,
        bonereaper_v2_gate_stats,
        model_training_samples: report.model_training_samples,
    })
}

/// Portfolio-mode walk-forward: sequential, chronological, compounding equity.
/// Each strategy maintains its own running bankroll; per-market max_clip can
/// scale with equity via `cfg.clip_fraction_of_equity`.
async fn run_portfolio(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
    meta_calibrator_snapshot: Option<OnlineMetaCalibratorSnapshot>,
    mut meta_report: Option<MetaCalibrationReport>,
) -> Result<(Vec<MarketResult>, WalkForwardSummary)> {
    let store_inner = store.store();
    let empty_spot = Arc::new(SpotHistory::default());
    let bte_policy_scales = cfg
        .back_to_explore_policy_scales_jsonl
        .as_deref()
        .map(load_bte_policy_scales)
        .transpose()?;
    let mut equity_by_strategy: HashMap<&'static str, f64> = cfg
        .strategies
        .iter()
        .map(|s| (s.name(), cfg.starting_cash_usdc))
        .collect();
    let mut peak_equity_by_strategy = equity_by_strategy.clone();
    let mut session_peak_equity_by_strategy = equity_by_strategy.clone();
    let mut session_date_by_strategy: HashMap<&'static str, String> = cfg
        .strategies
        .iter()
        .map(|s| (s.name(), String::new()))
        .collect();
    let mut daily_start_equity_by_strategy = equity_by_strategy.clone();
    let mut loss_streak_cooldown_by_strategy: HashMap<&'static str, LossStreakCooldownState> = cfg
        .strategies
        .iter()
        .map(|s| (s.name(), LossStreakCooldownState::default()))
        .collect();
    let mut shared_skew_tables: HashMap<&'static str, Arc<Mutex<SkewWinRateTable>>> =
        HashMap::new();
    let mut shared_model_states: HashMap<&'static str, Arc<Mutex<ModelState>>> = HashMap::new();
    for strat in &cfg.strategies {
        if *strat == StratId::ReactiveDirectional {
            shared_skew_tables.insert(strat.name(), Arc::new(Mutex::new(SkewWinRateTable::new())));
            shared_model_states.insert(
                strat.name(),
                Arc::new(Mutex::new(model_state_with_snapshot(
                    meta_calibrator_snapshot.as_ref(),
                ))),
            );
        } else {
            shared_model_states.insert(
                strat.name(),
                Arc::new(Mutex::new(model_state_with_snapshot(
                    meta_calibrator_snapshot.as_ref(),
                ))),
            );
        }
    }
    let mut results: Vec<MarketResult> = Vec::with_capacity(markets.len());
    let mut oos_meta_samples: Vec<MetaTrainingSample> = Vec::with_capacity(markets.len());
    if let Some(path) = cfg.decision_log_jsonl.as_deref() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }

    for (idx, m) in markets.iter().enumerate() {
        let events = match load_replay_events_for_market(
            store,
            store_inner.clone(),
            m,
            MarketId(idx as u32 + 1),
            cfg.replay_event_cache_dir.as_deref(),
        )
        .await
        {
            Ok(events) => events,
            Err(e) => {
                tracing::warn!(market = %m.slug, error = %e, "tape load failed");
                continue;
            }
        };
        let sampled_events;
        let events_for_run = if cfg.replay_sample_ms > 0 {
            sampled_events = sample_replay_events(&events, cfg.replay_sample_ms);
            sampled_events.as_slice()
        } else {
            events.as_slice()
        };
        let trades = if cfg.load_pm_trades {
            match resolve_pm_trades_day(store, &m.date, &m.asset_id).await {
                Ok(tp) => match load_pm_trades_async(store_inner.clone(), tp).await {
                    Ok((ticks, _)) => Arc::new(TradeHistory::new(ticks)),
                    Err(_) => Arc::new(TradeHistory::default()),
                },
                Err(_) => Arc::new(TradeHistory::default()),
            }
        } else {
            Arc::new(TradeHistory::default())
        };
        let spot = spot_history_for_market(spot_map, &empty_spot, cfg, m);
        let resolved_yes = if cfg.use_outcome_label {
            Some(
                outcome_label_resolved_yes(&m.outcome)
                    .expect("outcome labels are validated before replay"),
            )
        } else {
            None
        };

        let mut per_strategy = HashMap::new();
        let mut captured_meta_sample_for_market = false;
        let bte_policy_scale = bte_policy_scale_for(bte_policy_scales.as_ref(), m);
        for &strat in &cfg.strategies {
            let bankroll = *equity_by_strategy
                .get(strat.name())
                .unwrap_or(&cfg.starting_cash_usdc);
            let peak_equity = *peak_equity_by_strategy
                .get(strat.name())
                .unwrap_or(&cfg.starting_cash_usdc);
            let drawdown_pct = if peak_equity > 0.0 {
                1.0 - bankroll / peak_equity
            } else {
                0.0
            };
            let global_clip_multiplier = drawdown_clip_multiplier(
                drawdown_pct,
                cfg.clip_drawdown_soft_pct,
                cfg.clip_drawdown_hard_pct,
                cfg.clip_drawdown_min_multiplier,
            );
            let session_date = session_date_by_strategy.entry(strat.name()).or_default();
            if session_date != &m.date {
                *session_date = m.date.clone();
                session_peak_equity_by_strategy.insert(strat.name(), bankroll);
                daily_start_equity_by_strategy.insert(strat.name(), bankroll);
            }
            let session_peak_equity = *session_peak_equity_by_strategy
                .get(strat.name())
                .unwrap_or(&bankroll);
            let session_drawdown_pct = if session_peak_equity > 0.0 {
                1.0 - bankroll / session_peak_equity
            } else {
                0.0
            };
            let session_clip_multiplier = drawdown_clip_multiplier(
                session_drawdown_pct,
                cfg.clip_session_drawdown_soft_pct,
                cfg.clip_session_drawdown_hard_pct,
                cfg.clip_session_drawdown_min_multiplier,
            );
            let daily_start_equity = *daily_start_equity_by_strategy
                .get(strat.name())
                .unwrap_or(&bankroll);
            let daily_loss_pct = if daily_start_equity > 0.0 {
                (daily_start_equity - bankroll) / daily_start_equity
            } else {
                0.0
            };
            let remaining_daily_loss_budget = daily_remaining_loss_budget_usdc(
                daily_start_equity,
                bankroll,
                cfg.daily_loss_cap_pct,
            );
            let daily_clip_multiplier = if remaining_daily_loss_budget == Some(0.0)
                || (cfg.daily_loss_cap_pct < 1.0 && daily_loss_pct >= cfg.daily_loss_cap_pct)
            {
                0.0
            } else {
                1.0
            };
            let loss_streak_cooldown_active = loss_streak_cooldown_by_strategy
                .get(strat.name())
                .is_some_and(LossStreakCooldownState::is_active);
            let loss_streak_clip_multiplier = if loss_streak_cooldown_active {
                0.0
            } else {
                1.0
            };
            let max_per_market_exposure_usdc = remaining_daily_loss_budget
                .map(|remaining| per_market_exposure_cap(cfg, bankroll).min(remaining))
                .unwrap_or_else(|| per_market_exposure_cap(cfg, bankroll));
            let clip_multiplier = global_clip_multiplier
                .min(session_clip_multiplier)
                .min(daily_clip_multiplier)
                .min(loss_streak_clip_multiplier);
            // Per-market clip: a fraction of current equity (compounds), else
            // static fallback. Hard floor + ceiling for sanity.
            let clip = match cfg.clip_fraction_of_equity {
                Some(frac) => compounded_clip(bankroll, frac),
                None => cfg.max_clip_usdc,
            } * clip_multiplier;
            let runner_cfg = RunnerConfig {
                starting_cash_usdc: bankroll,
                market_open_ns: market_open_ns(&m),
                market_close_ns: market_close_ns(&m),
                resolved_yes,
                portfolio_limits: PortfolioLimits {
                    max_clip_usdc: clip * cfg.max_order_clip_multiplier,
                    max_per_market_exposure_usdc,
                    max_daily_exposure_usdc: bankroll * 5.0,
                    ..PortfolioLimits::default()
                },
                current_btc_net_shares: 0.0,
                current_eth_net_shares: 0.0,
                equity_curve_jsonl: None,
                snapshot_every_n: 1_000_000,
                maker_rebate_bps: cfg.maker_rebate_bps,
                taker_fee_bps: cfg.taker_fee_bps,
                decision_log_jsonl: cfg.decision_log_jsonl.clone(),
                decision_log_parquet: None,
                strategy_name: strat.name().to_string(),
                shared_model_state: if strat == StratId::ReactiveDirectional {
                    None
                } else {
                    shared_model_states.get(strat.name()).cloned()
                },
                update_model_state_on_resolution: meta_calibrator_snapshot.is_none(),
                meta_calibrator_snapshot: meta_calibrator_snapshot.clone(),
                enable_meta_calibration: cfg.enable_meta_calibration,
                model_market_context: model_market_context_for_cfg(cfg, &m),
                prior_market_range_1d: prior_market_range_mean(&results, 288),
                prior_market_range_3d: prior_market_range_mean(&results, 3 * 288),
                prior_market_range_7d: prior_market_range_mean(&results, 7 * 288),
                model_btc_whipsaw_risk_weight: cfg.model_btc_whipsaw_risk_weight,
                model_btc_path_inefficiency_risk_weight: cfg
                    .model_btc_path_inefficiency_risk_weight,
                model_btc_reversal_pressure_risk_weight: cfg
                    .model_btc_reversal_pressure_risk_weight,
                decision_log_every_n: cfg.decision_log_every_n,
                max_inventory_imbalance_shares: 1.5,
                taker_slippage_bps: 15.0,
                taker_latency_ms: cfg.taker_latency_ms,
                enforce_model_gate: cfg.enforce_model_gate,
                model_gate_min_confidence: cfg.model_gate_min_confidence,
                model_gate_max_risk: cfg.model_gate_max_risk,
                model_gate_min_edge: cfg.model_gate_min_edge,
                daily_start_cash_usdc: daily_start_equity,
                daily_loss_cap_pct: cfg.daily_loss_cap_pct,
                current_daily_loss_pct: daily_loss_pct,
            };
            match run_one_strategy(
                strat,
                cfg,
                events_for_run,
                &spot,
                &trades,
                &runner_cfg,
                bankroll,
                clip,
                shared_skew_tables.get(strat.name()).cloned(),
                shared_model_states.get(strat.name()).cloned(),
                if strat == StratId::BackToExplore {
                    bte_policy_scale
                } else {
                    1.0
                },
            ) {
                Ok(mut r) => {
                    for sample in &mut r.model_training_samples {
                        sample.market_idx = idx as u32;
                    }
                    if !captured_meta_sample_for_market && !r.model_training_samples.is_empty() {
                        oos_meta_samples.extend(r.model_training_samples.iter().copied());
                        captured_meta_sample_for_market = true;
                    }
                    equity_by_strategy.insert(strat.name(), r.end_equity_usdc);
                    peak_equity_by_strategy
                        .entry(strat.name())
                        .and_modify(|peak| *peak = peak.max(r.end_equity_usdc))
                        .or_insert(r.end_equity_usdc);
                    session_peak_equity_by_strategy
                        .entry(strat.name())
                        .and_modify(|peak| *peak = peak.max(r.end_equity_usdc))
                        .or_insert(r.end_equity_usdc);
                    if let Some(state) = loss_streak_cooldown_by_strategy.get_mut(strat.name()) {
                        if loss_streak_cooldown_active {
                            state.consume_cooldown_market();
                        } else {
                            state.record_completed_market(
                                r.orders_filled > 0,
                                r.pnl_usdc,
                                cfg.loss_streak_loss_threshold_usdc,
                                cfg.loss_streak_cooldown_after,
                                cfg.loss_streak_cooldown_markets,
                            );
                        }
                    }
                    per_strategy.insert(strat.name(), r);
                }
                Err(e) => {
                    tracing::warn!(market = %m.slug, strategy = strat.name(), error = %e, "strategy run failed");
                }
            }
        }

        let volatility_range = market_volatility_range(events_for_run);
        let volatility_band = volatility_band(volatility_range, cfg.volatility_regime_threshold);

        if (idx + 1) % 50 == 0 {
            let equity_strs: Vec<String> = cfg
                .strategies
                .iter()
                .map(|s| {
                    format!(
                        "{}={:.2}",
                        s.name(),
                        equity_by_strategy.get(s.name()).copied().unwrap_or(0.0)
                    )
                })
                .collect();
            tracing::info!(
                done = idx + 1,
                total = markets.len(),
                equity = %equity_strs.join(" "),
                "portfolio progress",
            );
        }

        results.push(MarketResult {
            asset_id: m.asset_id.clone(),
            slug: m.slug.clone(),
            close_ts: market_close_ts(&m),
            outcome_label: m.outcome.clone(),
            volatility_range,
            volatility_band,
            per_strategy,
        });

        if cfg.portfolio_checkpoint_every_markets > 0
            && results.len() % cfg.portfolio_checkpoint_every_markets == 0
        {
            write_portfolio_checkpoint(
                cfg,
                &results,
                meta_report.as_ref(),
                meta_calibrator_snapshot.as_ref(),
                &oos_meta_samples,
            )
            .with_context(|| {
                format!("write portfolio checkpoint after {} markets", results.len())
            })?;
        }
    }

    let mut summary = aggregate(&results, &cfg.strategies);
    summary.run_config = Some(summary_run_config(cfg));
    if let Some(report) = meta_report.as_mut() {
        report.oos_samples = oos_meta_samples.len();
        if let Some(snapshot) = meta_calibrator_snapshot.as_ref() {
            let filtered_oos_samples = filter_meta_samples_for_training(
                &oos_meta_samples,
                MetaSampleLimits::from_config(cfg),
            );
            let evaluation_samples = market_balanced_meta_samples(
                &filtered_oos_samples,
                cfg.meta_max_oos_evaluation_samples,
                cfg.meta_max_samples_per_market,
            );
            report.oos_evaluation_samples = evaluation_samples.len();
            report.oos = Some(evaluate_meta_calibration(snapshot, &evaluation_samples));
        }
        summary.meta_calibration = meta_report;
    }
    Ok((results, summary))
}

fn model_state_with_snapshot(snapshot: Option<&OnlineMetaCalibratorSnapshot>) -> ModelState {
    let mut state = ModelState::new();
    if let Some(snapshot) = snapshot {
        state.load_meta_calibrator_snapshot(snapshot.clone());
    }
    state
}

fn write_portfolio_checkpoint(
    cfg: &WalkForwardConfig,
    results: &[MarketResult],
    meta_report: Option<&MetaCalibrationReport>,
    meta_calibrator_snapshot: Option<&OnlineMetaCalibratorSnapshot>,
    oos_meta_samples: &[MetaTrainingSample],
) -> Result<()> {
    let mut summary = aggregate(results, &cfg.strategies);
    summary.run_config = Some(summary_run_config(cfg));
    if let Some(report) = meta_report {
        let mut report = report.clone();
        report.oos_samples = oos_meta_samples.len();
        if let Some(snapshot) = meta_calibrator_snapshot {
            let filtered_oos_samples = filter_meta_samples_for_training(
                oos_meta_samples,
                MetaSampleLimits::from_config(cfg),
            );
            let evaluation_samples = market_balanced_meta_samples(
                &filtered_oos_samples,
                cfg.meta_max_oos_evaluation_samples,
                cfg.meta_max_samples_per_market,
            );
            report.oos_evaluation_samples = evaluation_samples.len();
            report.oos = Some(evaluate_meta_calibration(snapshot, &evaluation_samples));
        }
        summary.meta_calibration = Some(report);
    }
    if let Some(path) = cfg.checkpoint_markets_out.as_deref() {
        write_market_results_jsonl_atomic(path, results)?;
        tracing::info!(
            ?path,
            markets = results.len(),
            "wrote portfolio markets checkpoint"
        );
    }
    if let Some(path) = cfg.checkpoint_summary_out.as_deref() {
        write_summary_json_atomic(path, &summary)?;
        tracing::info!(
            ?path,
            markets = results.len(),
            "wrote portfolio summary checkpoint"
        );
    }
    Ok(())
}

pub fn write_market_results_jsonl_atomic(
    path: &std::path::Path,
    results: &[MarketResult],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    let tmp = temp_sibling_path(path);
    {
        let mut file = std::fs::File::create(&tmp)
            .with_context(|| format!("create temp results {}", tmp.display()))?;
        for result in results {
            writeln!(file, "{}", serde_json::to_string(result)?)
                .with_context(|| format!("write temp results {}", tmp.display()))?;
        }
        file.flush()
            .with_context(|| format!("flush temp results {}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

pub fn write_summary_json_atomic(
    path: &std::path::Path,
    summary: &WalkForwardSummary,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    let tmp = temp_sibling_path(path);
    std::fs::write(&tmp, serde_json::to_string_pretty(summary)?)
        .with_context(|| format!("write temp summary {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

fn temp_sibling_path(path: &std::path::Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "checkpoint".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.{}.{}.tmp", std::process::id(), nanos))
}

fn aggregate_for_strategy(records: &[&StrategyMarketResult]) -> StrategyAggregate {
    let mut pnls: Vec<f64> = Vec::with_capacity(records.len());
    let mut markets_with_orders = 0usize;
    let mut total_orders_submitted = 0usize;
    let mut total_orders_filled = 0usize;
    let mut total_orders_filled_taker = 0usize;
    let mut total_orders_filled_maker = 0usize;
    let mut total_requested_notional = 0.0f64;
    let mut total_filled_notional = 0.0f64;
    let mut total_requested_shares = 0.0f64;
    let mut total_filled_shares = 0.0f64;
    let mut total_slippage_notional = 0.0f64;
    let mut total_orders_rejected_model_gate = 0usize;
    let mut total_orders_rejected_model_gate_confidence = 0usize;
    let mut total_orders_rejected_model_gate_risk = 0usize;
    let mut total_orders_rejected_model_gate_edge = 0usize;
    let mut tag_fills: HashMap<String, FillTagAccumulator> = HashMap::new();
    let mut model_fill_quality = ModelFillQualityAccumulator::default();
    let mut bonereaper_v2_gate_stats: Option<BonereaperV2GateStats> = None;
    let first_start_equity = records
        .first()
        .map(|r| r.start_equity_usdc)
        .unwrap_or_default();
    let mut last_end_equity = records
        .last()
        .map(|r| r.end_equity_usdc)
        .unwrap_or_default();
    let mut min_end_equity = f64::INFINITY;
    let mut max_end_equity = f64::NEG_INFINITY;
    let mut peak_end_equity = first_start_equity.max(0.0);
    let mut path_max_drawdown = 0.0f64;

    for r in records {
        pnls.push(r.pnl_usdc);
        if r.orders_filled > 0 {
            markets_with_orders += 1;
        }
        total_orders_submitted += r.orders_submitted;
        total_orders_filled += r.orders_filled;
        total_orders_filled_taker += r.orders_filled_taker;
        total_orders_filled_maker += r.orders_filled_maker;
        total_requested_notional += r.requested_notional_usdc;
        total_filled_notional += r.filled_notional_usdc;
        total_requested_shares += r.requested_shares;
        total_filled_shares += r.filled_shares;
        total_slippage_notional += r.avg_slippage_bps * r.filled_notional_usdc;
        total_orders_rejected_model_gate += r.orders_rejected_model_gate;
        total_orders_rejected_model_gate_confidence += r.orders_rejected_model_gate_confidence;
        total_orders_rejected_model_gate_risk += r.orders_rejected_model_gate_risk;
        total_orders_rejected_model_gate_edge += r.orders_rejected_model_gate_edge;
        for fill in &r.fills_detail {
            let pnl = fill_resolution_pnl(fill, r.yes_resolved);
            tag_fills
                .entry(fill.tag.clone())
                .or_default()
                .push(fill, pnl);
            model_fill_quality.push_fill(fill, r.yes_resolved);
        }
        model_fill_quality.push_market_majority(r);
        if let Some(stats) = r.bonereaper_v2_gate_stats {
            bonereaper_v2_gate_stats
                .get_or_insert_with(BonereaperV2GateStats::default)
                .add_assign(stats);
        }
        last_end_equity = r.end_equity_usdc;
        min_end_equity = min_end_equity.min(r.end_equity_usdc);
        max_end_equity = max_end_equity.max(r.end_equity_usdc);
        peak_end_equity = peak_end_equity.max(r.end_equity_usdc);
        if peak_end_equity > 0.0 {
            path_max_drawdown =
                path_max_drawdown.max((peak_end_equity - r.end_equity_usdc) / peak_end_equity);
        }
    }

    let total = pnls.iter().sum::<f64>();
    let n = pnls.len();
    let mean = if n > 0 { total / n as f64 } else { 0.0 };
    let stdev = if n > 1 {
        (pnls.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64).sqrt()
    } else {
        0.0
    };
    let mut sorted = pnls.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = if sorted.is_empty() {
        0.0
    } else if sorted.len() % 2 == 1 {
        sorted[sorted.len() / 2]
    } else {
        0.5 * (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2])
    };
    let hit_rate = if pnls.is_empty() {
        0.0
    } else {
        pnls.iter().filter(|p| **p > 0.0).count() as f64 / pnls.len() as f64
    };
    let best = sorted.last().copied().unwrap_or(0.0);
    let worst = sorted.first().copied().unwrap_or(0.0);
    if !min_end_equity.is_finite() {
        min_end_equity = 0.0;
    }
    if !max_end_equity.is_finite() {
        max_end_equity = 0.0;
    }
    let by_fill_tag = tag_fills
        .into_iter()
        .map(|(tag, fills)| (tag, fills.into_aggregate()))
        .collect();
    StrategyAggregate {
        total_pnl_usdc: total,
        first_start_equity_usdc: first_start_equity,
        last_end_equity_usdc: last_end_equity,
        min_end_equity_usdc: min_end_equity,
        max_end_equity_usdc: max_end_equity,
        compounded_return_pct: if first_start_equity > 0.0 {
            (last_end_equity / first_start_equity - 1.0) * 100.0
        } else {
            0.0
        },
        path_max_drawdown_pct: path_max_drawdown * 100.0,
        mean_pnl_usdc: mean,
        median_pnl_usdc: median,
        stdev_pnl_usdc: stdev,
        hit_rate,
        markets_with_orders,
        total_orders_submitted,
        total_orders_filled,
        total_orders_filled_taker,
        total_orders_filled_maker,
        maker_fill_rate: if total_orders_filled > 0 {
            total_orders_filled_maker as f64 / total_orders_filled as f64
        } else {
            0.0
        },
        total_requested_notional_usdc: total_requested_notional,
        total_filled_notional_usdc: total_filled_notional,
        fill_notional_ratio: if total_requested_notional > 0.0 {
            total_filled_notional / total_requested_notional
        } else {
            0.0
        },
        total_requested_shares,
        total_filled_shares,
        fill_shares_ratio: if total_requested_shares > 0.0 {
            total_filled_shares / total_requested_shares
        } else {
            0.0
        },
        avg_slippage_bps: if total_filled_notional > 0.0 {
            total_slippage_notional / total_filled_notional
        } else {
            0.0
        },
        total_orders_rejected_model_gate,
        total_orders_rejected_model_gate_confidence,
        total_orders_rejected_model_gate_risk,
        total_orders_rejected_model_gate_edge,
        worst_market_pnl: worst,
        best_market_pnl: best,
        sharpe_ratio: if stdev > 0.0 {
            mean / stdev * (records.len() as f64).sqrt()
        } else {
            0.0
        },
        by_fill_tag,
        model_fill_quality: model_fill_quality.into_summary(),
        bonereaper_v2_gate_stats,
    }
}

fn fill_resolution_pnl(fill: &crate::runner::Fill, yes_resolved: bool) -> f64 {
    let payout = match fill.side.as_str() {
        "BuyYes" => {
            if yes_resolved {
                fill.shares
            } else {
                0.0
            }
        }
        "BuyNo" => {
            if yes_resolved {
                0.0
            } else {
                fill.shares
            }
        }
        "SellYes" | "SellNo" => fill.notional,
        _ => 0.0,
    };
    payout - fill.notional + fill.rebate_usdc
}

fn buy_fill_won(fill: &crate::runner::Fill, yes_resolved: bool) -> Option<bool> {
    match fill.side.as_str() {
        "BuyYes" => Some(yes_resolved),
        "BuyNo" => Some(!yes_resolved),
        _ => None,
    }
}

fn aggregate(results: &[MarketResult], strategies: &[StratId]) -> WalkForwardSummary {
    let mut per_strategy = HashMap::new();
    for &strat in strategies {
        let name = strat.name();
        let records: Vec<_> = results
            .iter()
            .filter_map(|r| r.per_strategy.get(name))
            .collect();
        per_strategy.insert(name, aggregate_for_strategy(&records));
    }

    let mut by_volatility_band: HashMap<VolatilityBand, HashMap<&'static str, StrategyAggregate>> =
        HashMap::new();
    for band in [VolatilityBand::Low, VolatilityBand::High] {
        let mut band_per_strategy = HashMap::new();
        let band_results: Vec<_> = results
            .iter()
            .filter(|r| r.volatility_band == band)
            .collect();
        for &strat in strategies {
            let name = strat.name();
            let records: Vec<_> = band_results
                .iter()
                .filter_map(|r| r.per_strategy.get(name))
                .collect();
            band_per_strategy.insert(name, aggregate_for_strategy(&records));
        }
        by_volatility_band.insert(band, band_per_strategy);
    }

    WalkForwardSummary {
        markets_attempted: results.len(),
        markets_succeeded: results
            .iter()
            .filter(|r| !r.per_strategy.is_empty())
            .count(),
        run_config: None,
        per_strategy,
        by_volatility_band,
        fold_summaries: Vec::new(),
        meta_calibration: None,
    }
}

fn summary_run_config(cfg: &WalkForwardConfig) -> SummaryRunConfig {
    let mut strategies = Vec::with_capacity(cfg.strategies.len());
    for strat in &cfg.strategies {
        let config = match strat {
            StratId::BonereaperV2 => Some(
                serde_json::to_value(BonereaperV2SummaryConfig::from(cfg))
                    .expect("serialize bonereaper_v2 summary config"),
            ),
            StratId::BackToExplore => Some(
                serde_json::to_value(BackToExploreSummaryConfig::from(cfg))
                    .expect("serialize back_to_explore summary config"),
            ),
            _ => None,
        };
        strategies.push(StrategyRunConfig {
            strategy: strat.name(),
            config,
        });
    }
    SummaryRunConfig {
        shared: SharedRunConfig::from(cfg),
        strategies,
    }
}

pub fn print_summary(summary: &WalkForwardSummary) {
    fn print_table(title: &str, per_strategy: &HashMap<&'static str, StrategyAggregate>) {
        println!("{title}");
        println!(
            "{:>22}  {:>10}  {:>10}  {:>9}  {:>8}  {:>10}  {:>10}  {:>10}  {:>8}  {:>9}  {:>8}  {:>8}  {:>8}  {:>14}  {:>10}",
            "strategy",
            "total_pnl",
            "end_eq",
            "return",
            "max_dd",
            "mean_pnl",
            "median",
            "stdev",
            "hit",
            "sharpe",
            "sh_fill",
            "nt_fill",
            "slip",
            "fills",
            "worst",
        );
        let mut rows: Vec<(&&str, &StrategyAggregate)> = per_strategy.iter().collect();
        rows.sort_by(|a, b| {
            b.1.total_pnl_usdc
                .partial_cmp(&a.1.total_pnl_usdc)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (name, agg) in rows {
            println!(
                "{:>22}  {:>+10.4}  {:>10.2}  {:>+8.1}%  {:>7.1}%  {:>+10.4}  {:>+10.4}  {:>10.4}  {:>7.1}%  {:>+10.4}  {:>7.1}%  {:>7.1}%  {:>8.1}  {:>14}  {:>+10.4}",
                name,
                agg.total_pnl_usdc,
                agg.last_end_equity_usdc,
                agg.compounded_return_pct,
                agg.path_max_drawdown_pct,
                agg.mean_pnl_usdc,
                agg.median_pnl_usdc,
                agg.stdev_pnl_usdc,
                agg.hit_rate * 100.0,
                agg.sharpe_ratio,
                agg.fill_shares_ratio * 100.0,
                agg.fill_notional_ratio * 100.0,
                agg.avg_slippage_bps,
                agg.total_orders_filled,
                agg.worst_market_pnl,
            );
        }
        println!();
    }

    println!("== walk-forward summary ==");
    println!(
        "markets: attempted={}  succeeded={}",
        summary.markets_attempted, summary.markets_succeeded
    );
    println!();

    print_table("overall", &summary.per_strategy);

    let threshold = summary
        .run_config
        .as_ref()
        .map(|cfg| cfg.shared.volatility_regime_threshold)
        .unwrap_or_default();
    println!(
        "market YES-range buckets (threshold=max(yes_mid)-min(yes_mid) > {:.3}):",
        threshold
    );
    print_table(
        VolatilityBand::Low.as_str(),
        summary
            .by_volatility_band
            .get(&VolatilityBand::Low)
            .unwrap_or(&HashMap::new()),
    );
    print_table(
        VolatilityBand::High.as_str(),
        summary
            .by_volatility_band
            .get(&VolatilityBand::High)
            .unwrap_or(&HashMap::new()),
    );

    if !summary.fold_summaries.is_empty() {
        println!("folds:");
        for fold in &summary.fold_summaries {
            println!(
                "  [{}] train_end={} purge={} test=[{}, {})",
                fold.fold_idx,
                fold.train_end_exclusive,
                fold.purge_markets,
                fold.test_start,
                fold.test_end
            );
            if fold.test_start >= fold.test_end {
                println!("    (empty)");
                continue;
            }
            print_table(
                &format!(
                    "fold {} metrics ({}..{})",
                    fold.fold_idx, fold.test_start, fold.test_end
                ),
                &fold.fold_results.per_strategy,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tmp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    #[test]
    fn market_close_ts_normalizes_legacy_open_timestamp_rows() {
        let mut market = MarketHandle {
            asset_id: "asset".to_string(),
            slug: "btc-updown-5m-1778763000".to_string(),
            close_ts: 1_778_763_000,
            outcome: "Up".to_string(),
            date: "2026-05-14".to_string(),
        };

        assert_eq!(market_open_ts(&market), 1_778_763_000);
        assert_eq!(market_close_ts(&market), 1_778_763_300);

        market.close_ts = 1_778_763_300;
        assert_eq!(market_close_ts(&market), 1_778_763_300);
    }

    #[test]
    fn market_close_ts_uses_slug_duration_for_longer_horizons() {
        let mut market = MarketHandle {
            asset_id: "asset".to_string(),
            slug: "btc-updown-15m-1778763000".to_string(),
            close_ts: 1_778_763_000,
            outcome: "Up".to_string(),
            date: "2026-05-14".to_string(),
        };

        assert_eq!(market_open_ts(&market), 1_778_763_000);
        assert_eq!(market_close_ts(&market), 1_778_763_900);

        market.close_ts = 1_778_763_900;
        assert_eq!(market_close_ts(&market), 1_778_763_900);

        market.slug = "eth-updown-4h-1778760000".to_string();
        market.close_ts = 1_778_760_000;
        assert_eq!(market_open_ts(&market), 1_778_760_000);
        assert_eq!(market_close_ts(&market), 1_778_774_400);
    }

    #[test]
    fn outcome_label_validation_rejects_unknown_labels() {
        let markets = vec![MarketHandle {
            asset_id: "asset".to_string(),
            slug: "btc-updown-5m-1778763000".to_string(),
            close_ts: 1_778_763_300,
            outcome: "Unknown".to_string(),
            date: "2026-05-14".to_string(),
        }];

        let err = validate_outcome_labels(&markets).expect_err("Unknown must fail closed");
        assert!(err.to_string().contains("requires explicit Up/Down"));
    }

    #[test]
    fn outcome_label_validation_accepts_yes_no_and_up_down() {
        let markets = ["Up", "Down", "Yes", "No"]
            .into_iter()
            .enumerate()
            .map(|(idx, outcome)| MarketHandle {
                asset_id: format!("asset-{idx}"),
                slug: format!("btc-updown-5m-{}", 1_778_763_000 + idx as i64 * 300),
                close_ts: 1_778_763_300 + idx as i64 * 300,
                outcome: outcome.to_string(),
                date: "2026-05-14".to_string(),
            })
            .collect::<Vec<_>>();

        validate_outcome_labels(&markets).expect("explicit labels should pass");
    }

    #[test]
    fn profile_loads_nested_bonereaper_v2_section_without_zeroing_missing_fields() {
        let root = unique_tmp_dir("pm-profile-test");
        std::fs::create_dir_all(&root).expect("create profile temp dir");
        let path = root.join("leader.toml");
        std::fs::write(
            &path,
            r#"
[strategy]
name = "bonereaper_v2"

[bonereaper_v2]
replay_sample_ms = 1000
late_confirm_min_model_edge = 0.025
late_favourite_max_clips = 8
late_favourite_min_model_edge = 0.035
late_favourite_high_cert_full_clip_edge = 0.045
late_favourite_high_cert_min_model_edge = 0.015
tail_budget_favourite_spend_frac = 0.20
model_btc_whipsaw_risk_weight = 0.31
"#,
        )
        .expect("write profile");

        let profile = BonereaperV2Profile::load(&path).expect("load nested profile");
        let mut cfg = WalkForwardConfig::default();
        cfg.br2_late_favourite_min_ask = 0.77;
        profile.apply_to_walkforward_config(&mut cfg);
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(cfg.br2_late_favourite_max_clips, 8);
        assert_eq!(cfg.replay_sample_ms, 1000);
        assert!((cfg.br2_late_confirm_min_model_edge - 0.025).abs() < f32::EPSILON);
        assert!((cfg.br2_late_favourite_min_model_edge - 0.035).abs() < f32::EPSILON);
        assert!((cfg.br2_late_favourite_high_cert_full_clip_edge - 0.045).abs() < f32::EPSILON);
        assert!((cfg.br2_late_favourite_high_cert_min_model_edge - 0.015).abs() < f32::EPSILON);
        assert!((cfg.br2_tail_budget_favourite_spend_frac - 0.20).abs() < f32::EPSILON);
        assert!((cfg.model_btc_whipsaw_risk_weight - 0.31).abs() < f32::EPSILON);
        assert!((cfg.br2_late_favourite_min_ask - 0.77).abs() < f32::EPSILON);
    }

    #[test]
    fn profile_load_resolves_strategy_to_back_to_explore() {
        let root = unique_tmp_dir("pm-profile-test-bte");
        std::fs::create_dir_all(&root).expect("create profile temp dir");
        let path = root.join("back_to_explore.toml");
        std::fs::write(
            &path,
            r#"
[strategy]
name = "back_to_explore"

[back_to_explore]
base_clip_usdc = 9.0
max_clip_usdc = 88.0
refresh_secs = 1.5
high_activity_hours = [10, 11]
range_soft_throttle = 0.65
range_hard_throttle = 0.95
range_min_clip_multiplier = 0.20
range_repair_min_clip_multiplier = 0.70
range_chop_min_range = 0.20
range_clean_path_efficiency = 0.78
clean_path_directional_clip_multiplier = 1.25
reversal_pressure_clip_multiplier = 0.0
policy_scales_jsonl = "data/runs/regime_clusters/bte_scales.jsonl"
debug_signals = true
"#,
        )
        .expect("write profile");

        let file = StrategyProfileFile::load(&path).expect("load profile");
        assert_eq!(file.strategy_name(), Some("back_to_explore"));

        let inactive = file
            .selected_strategy_profile(&[StratId::BonereaperV2])
            .expect("selected strategy should be parsed");
        assert!(inactive.is_none());

        let resolved = file
            .selected_strategy_profile(&[StratId::BackToExplore])
            .expect("selected strategy should resolve")
            .expect("strategy selected");
        assert_eq!(resolved.strategy_name(), "back_to_explore");
        let mut cfg = WalkForwardConfig::default();
        resolved.apply_to_walkforward_config(&mut cfg);
        assert!((cfg.back_to_explore_base_clip_usdc - 9.0).abs() < f64::EPSILON);
        assert!((cfg.back_to_explore_max_clip_usdc - 88.0).abs() < f64::EPSILON);
        assert!((cfg.back_to_explore_refresh_secs - 1.5).abs() < f64::EPSILON);
        assert_eq!(cfg.back_to_explore_high_activity_hours, vec![10, 11]);
        assert!((cfg.back_to_explore_range_soft_throttle - 0.65).abs() < f32::EPSILON);
        assert!((cfg.back_to_explore_range_hard_throttle - 0.95).abs() < f32::EPSILON);
        assert!((cfg.back_to_explore_range_min_clip_multiplier - 0.20).abs() < f64::EPSILON);
        assert!((cfg.back_to_explore_range_repair_min_clip_multiplier - 0.70).abs() < f64::EPSILON);
        assert!((cfg.back_to_explore_range_chop_min_range - 0.20).abs() < f32::EPSILON);
        assert!((cfg.back_to_explore_range_clean_path_efficiency - 0.78).abs() < f32::EPSILON);
        assert!(
            (cfg.back_to_explore_clean_path_directional_clip_multiplier - 1.25).abs()
                < f64::EPSILON
        );
        assert!((cfg.back_to_explore_reversal_pressure_clip_multiplier - 0.0).abs() < f64::EPSILON);
        assert_eq!(
            cfg.back_to_explore_policy_scales_jsonl.as_deref(),
            Some(Path::new("data/runs/regime_clusters/bte_scales.jsonl"))
        );
        assert!(cfg.back_to_explore_debug_signals);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn back_to_explore_config_uses_walkforward_fields_and_run_clip_floor() {
        let mut cfg = WalkForwardConfig::default();
        cfg.back_to_explore_base_clip_usdc = 15.0;
        cfg.back_to_explore_max_clip_usdc = 20.0;
        cfg.back_to_explore_base_participation_rate = 0.77;
        cfg.back_to_explore_range_soft_throttle = 0.64;
        cfg.back_to_explore_range_min_clip_multiplier = 0.25;
        cfg.back_to_explore_range_repair_min_clip_multiplier = 0.75;
        cfg.back_to_explore_range_chop_min_range = 0.22;
        cfg.back_to_explore_clean_path_directional_clip_multiplier = 1.20;
        cfg.back_to_explore_reversal_pressure_clip_multiplier = 0.15;

        let bte_cfg = cfg.build_back_to_explore_config(6.0, 123_456_789, 0.25);

        assert!((bte_cfg.base_clip_usdc - 15.0).abs() < f64::EPSILON);
        assert!((bte_cfg.market_window_ns - 123_456_789) == 0);
        assert!((bte_cfg.base_participation_rate - 0.77).abs() < f64::EPSILON);
        assert!((bte_cfg.range_soft_throttle - 0.64).abs() < f32::EPSILON);
        assert!((bte_cfg.range_min_clip_multiplier - 0.25).abs() < f64::EPSILON);
        assert!((bte_cfg.range_repair_min_clip_multiplier - 0.75).abs() < f64::EPSILON);
        assert!((bte_cfg.range_chop_min_range - 0.22).abs() < f32::EPSILON);
        assert!((bte_cfg.clean_path_directional_clip_multiplier - 1.20).abs() < f64::EPSILON);
        assert!((bte_cfg.reversal_pressure_clip_multiplier - 0.15).abs() < f64::EPSILON);
        assert!((bte_cfg.external_risk_multiplier - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn summary_run_config_keeps_archived_knobs_out_of_shared_config() {
        let mut cfg = WalkForwardConfig::default();
        cfg.strategies = vec![StratId::BackToExplore];

        let value = serde_json::to_value(summary_run_config(&cfg)).unwrap();
        let shared = value.get("shared").expect("shared config missing");
        let strategies = value
            .get("strategies")
            .and_then(|v| v.as_array())
            .expect("strategy config missing");

        assert!(shared.get("spot_symbol").is_none());
        assert!(shared.get("model_btc_whipsaw_risk_weight").is_none());
        assert!(
            shared
                .get("model_btc_path_inefficiency_risk_weight")
                .is_none()
        );
        assert!(
            shared
                .get("model_btc_reversal_pressure_risk_weight")
                .is_none()
        );
        assert_eq!(shared.get("spot_symbol_mode").unwrap(), "auto");
        assert!(shared.get("spot_symbol_override").is_none());
        assert!(shared.get("model_spot_whipsaw_risk_weight").is_some());
        assert!(
            shared
                .get("model_spot_path_inefficiency_risk_weight")
                .is_some()
        );
        assert!(
            shared
                .get("model_spot_reversal_pressure_risk_weight")
                .is_some()
        );
        assert_eq!(strategies[0].get("strategy").unwrap(), "back_to_explore");
        assert!(strategies[0].get("config").is_some());
    }

    #[test]
    fn hedged_first_profile_enables_arb_base_and_overlay_cap() {
        // The shipped profile must parse and turn the hedge-first lane on while
        // capping the directional overlay. Resolve the path relative to the
        // workspace root (CARGO_MANIFEST_DIR points at crates/pm-app).
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = manifest
            .join("../../configs/archive/2026-06-02/bonereaper_v2_hedged_first.toml")
            .canonicalize()
            .expect("resolve hedged-first profile path");

        let profile = BonereaperV2Profile::load(&path).expect("load hedged-first profile");
        let mut cfg = WalkForwardConfig::default();
        profile.apply_to_walkforward_config(&mut cfg);

        assert!(cfg.br2_hedged_base_enabled);
        assert!((cfg.br2_hedged_base_max_notional_usdc - 40.0).abs() < f32::EPSILON);
        assert!((cfg.br2_hedged_base_max_pair_cost - 0.98).abs() < f32::EPSILON);
        assert!((cfg.br2_late_directional_overlay_frac - 0.064).abs() < f32::EPSILON);
    }

    #[test]
    fn replay_event_cache_round_trips_jsonl() {
        let root = unique_tmp_dir("pm-replay-cache-test");
        let path = root.join("events.jsonl");
        let event = pm_types::ReplayEvent {
            ts_ns: 123,
            market_id: MarketId(7),
            yes_mid: 0.51,
            yes_bid: 0.50,
            yes_ask: 0.52,
            volume: 10.0,
            bids: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            asks: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            spot_price: 105_000.0,
            flags: pm_types::ReplayFlags::BOOK_UPDATE,
        };

        write_replay_event_cache(&path, &[event]).expect("write replay cache");
        let loaded = read_replay_event_cache(&path).expect("read replay cache");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(loaded, vec![event]);
    }

    #[test]
    fn replay_event_cache_market_ids_are_rebound_for_current_run() {
        let mut events = vec![pm_types::ReplayEvent {
            ts_ns: 123,
            market_id: MarketId(7),
            yes_mid: 0.51,
            yes_bid: 0.50,
            yes_ask: 0.52,
            volume: 10.0,
            bids: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            asks: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            spot_price: 105_000.0,
            flags: pm_types::ReplayFlags::BOOK_UPDATE,
        }];

        rebind_replay_event_market_ids(&mut events, MarketId(99));

        assert_eq!(events[0].market_id, MarketId(99));
    }

    #[test]
    fn aggregate_handles_per_strategy_metrics() {
        let mut result_reactive = HashMap::new();
        result_reactive.insert(
            StratId::ReactiveDirectional.name(),
            StrategyMarketResult {
                orders_submitted: 10,
                orders_filled: 6,
                orders_filled_taker: 4,
                orders_filled_maker: 2,
                orders_rejected_model_gate: 0,
                orders_rejected_model_gate_confidence: 0,
                orders_rejected_model_gate_risk: 0,
                orders_rejected_model_gate_edge: 0,
                pnl_usdc: 4.0,
                start_equity_usdc: 100.0,
                end_equity_usdc: 104.0,
                max_drawdown_pct: 0.12,
                fills: 6,
                maker_rebates_usdc: 0.1,
                requested_shares: 12.0,
                filled_shares: 10.0,
                fill_shares_ratio: 10.0 / 12.0,
                requested_notional_usdc: 60.0,
                filled_notional_usdc: 54.0,
                fill_notional_ratio: 0.9,
                avg_slippage_bps: 12.0,
                clip_used_usdc: 2.0,
                yes_resolved: true,
                fills_detail: vec![],
                bonereaper_v2_gate_stats: None,
                model_training_samples: vec![],
            },
        );

        let mut result_bonereaper = HashMap::new();
        result_bonereaper.insert(
            StratId::BonereaperLite.name(),
            StrategyMarketResult {
                orders_submitted: 3,
                orders_filled: 1,
                orders_filled_taker: 1,
                orders_filled_maker: 0,
                orders_rejected_model_gate: 0,
                orders_rejected_model_gate_confidence: 0,
                orders_rejected_model_gate_risk: 0,
                orders_rejected_model_gate_edge: 0,
                pnl_usdc: -2.0,
                start_equity_usdc: 100.0,
                end_equity_usdc: 98.0,
                max_drawdown_pct: 0.05,
                fills: 1,
                maker_rebates_usdc: 0.0,
                requested_shares: 2.0,
                filled_shares: 2.0,
                fill_shares_ratio: 1.0,
                requested_notional_usdc: 10.0,
                filled_notional_usdc: 10.0,
                fill_notional_ratio: 1.0,
                avg_slippage_bps: 15.0,
                clip_used_usdc: 1.5,
                yes_resolved: false,
                fills_detail: vec![],
                bonereaper_v2_gate_stats: None,
                model_training_samples: vec![],
            },
        );

        let mut result_no_order = HashMap::new();
        result_no_order.insert(
            StratId::PairedMm.name(),
            StrategyMarketResult {
                orders_submitted: 0,
                orders_filled: 0,
                orders_filled_taker: 0,
                orders_filled_maker: 0,
                orders_rejected_model_gate: 0,
                orders_rejected_model_gate_confidence: 0,
                orders_rejected_model_gate_risk: 0,
                orders_rejected_model_gate_edge: 0,
                pnl_usdc: 0.0,
                start_equity_usdc: 100.0,
                end_equity_usdc: 100.0,
                max_drawdown_pct: 0.0,
                fills: 0,
                maker_rebates_usdc: 0.0,
                requested_shares: 0.0,
                filled_shares: 0.0,
                fill_shares_ratio: 0.0,
                requested_notional_usdc: 0.0,
                filled_notional_usdc: 0.0,
                fill_notional_ratio: 0.0,
                avg_slippage_bps: 0.0,
                clip_used_usdc: 3.0,
                yes_resolved: true,
                fills_detail: vec![],
                bonereaper_v2_gate_stats: None,
                model_training_samples: vec![],
            },
        );

        let results = vec![
            MarketResult {
                asset_id: "1".to_string(),
                slug: "a".to_string(),
                close_ts: 0,
                outcome_label: "Yes".to_string(),
                volatility_range: 0.02,
                volatility_band: VolatilityBand::Low,
                per_strategy: result_reactive,
            },
            MarketResult {
                asset_id: "2".to_string(),
                slug: "b".to_string(),
                close_ts: 0,
                outcome_label: "No".to_string(),
                volatility_range: 0.20,
                volatility_band: VolatilityBand::High,
                per_strategy: result_bonereaper,
            },
            MarketResult {
                asset_id: "3".to_string(),
                slug: "c".to_string(),
                close_ts: 0,
                outcome_label: "Yes".to_string(),
                volatility_range: 0.10,
                volatility_band: VolatilityBand::Low,
                per_strategy: result_no_order,
            },
        ];

        let summary = aggregate(
            &results,
            &[
                StratId::ReactiveDirectional,
                StratId::BonereaperLite,
                StratId::PairedMm,
            ],
        );
        assert_eq!(summary.markets_attempted, 3);
        assert_eq!(summary.markets_succeeded, 3);

        let reactive = summary
            .per_strategy
            .get(StratId::ReactiveDirectional.name())
            .expect("reactive missing");
        assert_eq!(reactive.markets_with_orders, 1);
        assert_eq!(reactive.total_orders_filled, 6);
        assert_eq!(reactive.total_orders_filled_taker, 4);
        assert_eq!(reactive.total_orders_filled_maker, 2);
        assert!((reactive.fill_shares_ratio - 10.0 / 12.0).abs() < f64::EPSILON);
        assert!((reactive.fill_notional_ratio - 0.9).abs() < f64::EPSILON);
        assert!((reactive.avg_slippage_bps - 12.0).abs() < f64::EPSILON);
        assert_eq!(reactive.best_market_pnl, 4.0);
        assert_eq!(reactive.worst_market_pnl, 4.0);
        assert!((reactive.hit_rate - 1.0).abs() < f64::EPSILON);

        let paired = summary
            .per_strategy
            .get(StratId::PairedMm.name())
            .expect("paired missing");
        assert_eq!(paired.markets_with_orders, 0);
        assert_eq!(paired.total_orders_filled, 0);
        assert_eq!(paired.total_pnl_usdc, 0.0);

        let low = summary
            .by_volatility_band
            .get(&VolatilityBand::Low)
            .expect("low band missing");
        let low_reactive = low
            .get(StratId::ReactiveDirectional.name())
            .expect("low reactive missing");
        assert_eq!(low_reactive.total_pnl_usdc, 4.0);

        let high = summary
            .by_volatility_band
            .get(&VolatilityBand::High)
            .expect("high band missing");
        let high_bonereaper = high
            .get(StratId::BonereaperLite.name())
            .expect("high bonereaper missing");
        assert_eq!(high_bonereaper.total_pnl_usdc, -2.0);
        assert_eq!(reactive.sharpe_ratio, 0.0);
    }

    fn test_fill(
        side: &str,
        side_model_p: f32,
        notional: f64,
        range: f32,
        whipsaw: f32,
    ) -> crate::runner::Fill {
        crate::runner::Fill {
            ts_ns: 0,
            side: side.to_string(),
            shares: 10.0,
            price: (notional / 10.0) as f32,
            notional,
            tag: "test".to_string(),
            maker: false,
            rebate_usdc: 0.0,
            slippage_bps: 0.0,
            yes_mid: Some(0.50),
            yes_bid: Some(0.49),
            yes_ask: Some(0.51),
            side_model_p: Some(side_model_p),
            side_edge_vs_mid: Some(side_model_p - 0.50),
            side_edge_vs_fill: Some(side_model_p - (notional / 10.0) as f32),
            direction_score: Some(0.40),
            confidence_score: Some(0.75),
            calibrated_p: Some(side_model_p),
            risk_score: Some(0.30),
            market_yes_range_so_far: Some(range),
            seconds_since_open: Some(240.0),
            seconds_to_close: Some(60.0),
            regime_whipsaw_score: Some(whipsaw),
            regime_path_efficiency: Some(0.70),
            regime_reversal_pressure: Some(0.20),
            regime_sign_flip_rate: Some(0.10),
            regime_realized_vol_180s_bps: Some(25.0),
            binance_flow_imbal_5s: None,
            binance_flow_imbal_15s: None,
            binance_flow_imbal_30s: None,
            binance_adverse_vol_5s: None,
            binance_adverse_vol_15s: None,
            binance_adverse_vol_30s: None,
            binance_large_adverse_count_10s: None,
            binance_trade_intensity_15s: None,
            spot_ret_5s: None,
            spot_ret_15s: None,
            spot_ret_30s: None,
            spot_accel_15s_vs_30s: None,
            spot_accel_5s_vs_15s: None,
            post_fill_path: None,
        }
    }

    fn strategy_result_with_fills(
        yes_resolved: bool,
        fills_detail: Vec<crate::runner::Fill>,
    ) -> StrategyMarketResult {
        let filled_notional_usdc = fills_detail.iter().map(|fill| fill.notional).sum::<f64>();
        let filled_shares = fills_detail.iter().map(|fill| fill.shares).sum::<f64>();
        StrategyMarketResult {
            orders_submitted: fills_detail.len(),
            orders_filled: fills_detail.len(),
            orders_filled_taker: fills_detail.len(),
            orders_filled_maker: 0,
            orders_rejected_model_gate: 0,
            orders_rejected_model_gate_confidence: 0,
            orders_rejected_model_gate_risk: 0,
            orders_rejected_model_gate_edge: 0,
            pnl_usdc: 0.0,
            start_equity_usdc: 100.0,
            end_equity_usdc: 100.0,
            max_drawdown_pct: 0.0,
            fills: fills_detail.len(),
            maker_rebates_usdc: 0.0,
            requested_shares: filled_shares,
            filled_shares,
            fill_shares_ratio: if filled_shares > 0.0 { 1.0 } else { 0.0 },
            requested_notional_usdc: filled_notional_usdc,
            filled_notional_usdc,
            fill_notional_ratio: if filled_notional_usdc > 0.0 { 1.0 } else { 0.0 },
            avg_slippage_bps: 0.0,
            clip_used_usdc: filled_notional_usdc,
            yes_resolved,
            fills_detail,
            bonereaper_v2_gate_stats: None,
            model_training_samples: vec![],
        }
    }

    #[test]
    fn aggregate_reports_model_fill_quality() {
        let win_yes = test_fill("BuyYes", 0.80, 8.0, 0.60, 0.40);
        let win_no = test_fill("BuyNo", 0.70, 7.0, 0.40, 0.20);
        let lose_yes = test_fill("BuyYes", 0.90, 9.0, 0.60, 0.50);
        let first = strategy_result_with_fills(true, vec![win_yes]);
        let second = strategy_result_with_fills(false, vec![win_no, lose_yes]);
        let records = vec![&first, &second];

        let agg = aggregate_for_strategy(&records);
        let quality = &agg.model_fill_quality;

        assert_eq!(quality.all.fills, 3);
        assert!((quality.all.hit_rate - 2.0 / 3.0).abs() < 1e-12);
        assert!((quality.all.avg_predicted_p - 0.80).abs() < 1e-6);
        assert!((quality.all.brier - ((0.04 + 0.09 + 0.81) / 3.0)).abs() < 1e-6);
        assert!(quality.all.log_loss > 0.95 && quality.all.log_loss < 0.97);
        assert_eq!(quality.range_ge_050.fills, 2);
        assert_eq!(quality.range_lt_050.fills, 1);
        assert_eq!(quality.whipsaw_ge_035.fills, 2);
        assert_eq!(quality.whipsaw_lt_035.fills, 1);
        assert_eq!(quality.market_samples, 2);
        assert!((quality.market_majority_side_accuracy - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn drawdown_clip_multiplier_can_keep_recovery_floor() {
        assert!((drawdown_clip_multiplier(0.10, 0.20, 0.40, 0.10) - 1.0).abs() < 1e-12);
        assert!((drawdown_clip_multiplier(0.30, 0.20, 0.40, 0.10) - 0.55).abs() < 1e-12);
        assert!((drawdown_clip_multiplier(0.50, 0.20, 0.40, 0.10) - 0.10).abs() < 1e-12);
    }

    #[test]
    fn drawdown_clip_multiplier_preserves_zero_hard_stop_by_default() {
        assert!((drawdown_clip_multiplier(0.50, 0.20, 0.40, 0.0) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn daily_remaining_loss_budget_caps_next_market_exposure() {
        assert_eq!(daily_remaining_loss_budget_usdc(2700.0, 2700.0, 1.0), None);
        assert_eq!(
            daily_remaining_loss_budget_usdc(2700.0, 2700.0, 0.05),
            Some(135.0)
        );
        assert!(
            (daily_remaining_loss_budget_usdc(2700.0, 2573.0, 0.05).unwrap() - 8.0).abs() < 1e-12
        );
        assert_eq!(
            daily_remaining_loss_budget_usdc(2700.0, 2542.0, 0.05),
            Some(0.0)
        );
    }

    #[test]
    fn loss_streak_cooldown_uses_only_completed_traded_markets() {
        let mut state = LossStreakCooldownState::default();
        state.record_completed_market(true, -1.0, 0.0, 2, 2);
        assert!(!state.is_active());
        state.record_completed_market(false, -10.0, 0.0, 2, 2);
        assert!(!state.is_active());
        state.record_completed_market(true, -0.01, 0.0, 2, 2);
        assert!(state.is_active());

        state.consume_cooldown_market();
        assert!(state.is_active());
        state.consume_cooldown_market();
        assert!(!state.is_active());
        state.record_completed_market(true, 0.01, 0.0, 2, 2);
        assert_eq!(state.consecutive_losses, 0);
    }

    #[test]
    fn fold_plan_with_fold_size() {
        let cfg = WalkForwardConfig {
            fold_size: Some(3),
            ..WalkForwardConfig::default()
        };
        let plan = build_fold_plan(10, &cfg).expect("plan");
        assert_eq!(plan, vec![(0, 0, 3), (3, 3, 6), (6, 6, 9), (9, 9, 10)]);
    }

    #[test]
    fn fold_plan_skips_until_min_train_markets() {
        let cfg = WalkForwardConfig {
            fold_size: Some(3),
            min_train_markets: 6,
            ..WalkForwardConfig::default()
        };
        let plan = build_fold_plan(10, &cfg).expect("plan");
        assert_eq!(plan, vec![(6, 6, 9), (9, 9, 10)]);
    }

    #[test]
    fn fold_plan_errors_when_min_train_markets_impossible() {
        let cfg = WalkForwardConfig {
            fold_size: Some(3),
            min_train_markets: 12,
            ..WalkForwardConfig::default()
        };
        let err = build_fold_plan(10, &cfg).expect_err("should reject impossible min train");
        assert!(
            err.to_string().contains("no walk-forward folds satisfy"),
            "{err}"
        );
    }

    #[test]
    fn fold_plan_with_folds_and_purge() {
        let cfg = WalkForwardConfig {
            walk_forward_folds: Some(2),
            purge_markets: 2,
            ..WalkForwardConfig::default()
        };
        let plan = build_fold_plan(10, &cfg).expect("plan");
        assert_eq!(plan, vec![(0, 0, 5), (3, 5, 10)]);
    }

    #[test]
    fn no_fold_config_uses_single_window() {
        let cfg = WalkForwardConfig::default();
        let plan = build_fold_plan(7, &cfg).expect("plan");
        assert_eq!(plan, vec![(0, 0, 7)]);
    }

    #[test]
    fn compounded_clip_does_not_panic_on_tiny_bankroll() {
        assert_eq!(compounded_clip(0.16, 0.02), 0.0032);
        assert_eq!(compounded_clip(0.0, 0.02), 0.0);
        assert_eq!(compounded_clip(1000.0, 0.02), 20.0);
    }

    #[test]
    fn sample_replay_events_keeps_latest_tick_per_interval() {
        fn event(ts_ms: i64, mid: f32) -> pm_types::ReplayEvent {
            pm_types::ReplayEvent {
                ts_ns: ts_ms * 1_000_000,
                market_id: pm_types::MarketId(1),
                yes_mid: mid,
                yes_bid: mid - 0.01,
                yes_ask: mid + 0.01,
                volume: 0.0,
                bids: [pm_types::BookLevel::default(); pm_types::TAPE_DEPTH],
                asks: [pm_types::BookLevel::default(); pm_types::TAPE_DEPTH],
                spot_price: 100.0,
                flags: pm_types::ReplayFlags::BOOK_UPDATE,
            }
        }

        let events = vec![
            event(0, 0.50),
            event(100, 0.51),
            event(900, 0.52),
            event(1100, 0.53),
            event(1900, 0.54),
            event(2100, 0.55),
            event(3000, 0.56),
        ];

        let sampled = sample_replay_events(&events, 1000);
        let mids: Vec<_> = sampled.iter().map(|event| event.yes_mid).collect();
        assert_eq!(mids, vec![0.50, 0.52, 0.54, 0.55, 0.56]);
    }

    #[test]
    fn market_balanced_meta_samples_caps_each_market_and_total() {
        let mut samples = Vec::new();
        for market_idx in 0..5 {
            for sample_idx in 0..10 {
                let mut features = pm_model::MetaFeatures::default();
                features.values[0] = sample_idx as f32;
                samples.push(MetaTrainingSample {
                    features,
                    market_idx,
                    base_side_probability: 0.5,
                    side_observed: market_idx % 2 == 0,
                });
            }
        }

        let selected = market_balanced_meta_samples(&samples, 12, 4);
        assert_eq!(selected.len(), 10);
        for market_idx in 0..5 {
            let count = selected
                .iter()
                .filter(|sample| sample.market_idx == market_idx)
                .count();
            assert_eq!(count, 2);
        }
    }

    #[test]
    fn split_meta_samples_by_market_uses_chronological_markets() {
        let samples: Vec<MetaTrainingSample> = (0..4)
            .flat_map(|market_idx| {
                (0..3).map(move |_| MetaTrainingSample {
                    features: pm_model::MetaFeatures::default(),
                    market_idx,
                    base_side_probability: 0.5,
                    side_observed: market_idx % 2 == 0,
                })
            })
            .collect();

        let (fit, validation) = split_meta_samples_by_market(&samples, 0.5);
        assert_eq!(fit.len(), 6);
        assert_eq!(validation.len(), 6);
        assert!(fit.iter().all(|sample| sample.market_idx <= 1));
        assert!(validation.iter().all(|sample| sample.market_idx >= 2));
    }

    #[test]
    fn previous_date_handles_month_boundary() {
        assert_eq!(
            previous_date("2026-03-01").unwrap(),
            Some("2026-02-28".to_string())
        );
    }
}
