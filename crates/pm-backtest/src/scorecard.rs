//! Walk-forward scorecard types: per-strategy aggregates, meta-calibration
//! reports, calibration diagnostics, and the summary/report `println!`
//! presentation plus atomic JSON/JSONL writers.

use anyhow::{Context, Result};
use pm_model::{MetaFeatureWeight, MetaTrainingConfig, MetaTrainingSample, MetaTrainingStats, OnlineMetaCalibratorSnapshot};
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use crate::fills::Fill;

use crate::accounting::{MarketResult, StrategyMarketResult, buy_fill_won, fill_resolution_pnl};
use crate::config::WalkForwardConfig;
use crate::engine::{MetaSampleLimits, StratId, evaluate_meta_calibration, filter_meta_samples_for_training, market_balanced_meta_samples};
use crate::fingerprint::config_fingerprint;
use crate::jitter::JitterReport;
use crate::portfolio::{SharedRunConfig, VolatilityBand};
use crate::settlement::SettlementEra;

mod summary;
pub use summary::{
    DailyResultSummary, FillTagSummary, ResultSummary, print_result_summary,
    summarize_markets_jsonl, write_result_summary_json,
};

#[derive(Debug, Clone, Serialize)]
pub struct WalkForwardSummary {
    pub markets_attempted: usize,
    pub markets_succeeded: usize,
    /// 16-hex sha256 prefix of the canonical JSON of the resolved run config.
    pub config_fingerprint: Option<String>,
    /// `"FANTASY"` when the run was executed below the truthful latency floor
    /// under an explicit `--fantasy` grant. `None` for truthful runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub watermark: Option<String>,
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
    /// P&L spread across N jittered-latency replay runs. `None` (omitted from
    /// JSON) for ordinary single-run backtests where `--jitter` is 0.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter: Option<JitterReport>,
    /// Optional window label (e.g. `feb2026`) for multi-window validation
    /// drivers. `None` (omitted from JSON) when `--window-label` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_label: Option<String>,
    /// Validation status. `"UNVALIDATED"` unless the run's window label is a
    /// member of [`VALIDATED_WINDOWS`] AND the caller asserted the full window
    /// set ran (`--validated-set-complete`). A single run can never claim
    /// validated status by itself; only the multi-window driver script can.
    pub validation: String,
    /// Sizing-realism block: fractional sizing at a given bankroll with the
    /// 0.82 realization haircut and the 5-share floor/ruin check. `None`
    /// (omitted from JSON) unless `--bankroll` was passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizing: Option<SizingRealism>,
    /// Per-settlement-era breakdown (market counts, net P&L, label-vs-model
    /// disagreement count). `None` (omitted from JSON) unless at least one
    /// market lacks an outcome label or `--era-diagnostics` is passed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub era_breakdown: Option<Vec<EraBreakdown>>,
}

/// Per-era summary: how many markets settled under the era, their net P&L, and
/// how often the era-model outcome disagreed with the outcome label.
#[derive(Debug, Clone, Serialize)]
pub struct EraBreakdown {
    pub era: SettlementEra,
    pub markets: usize,
    pub net_pnl_usdc: f64,
    pub disagreements: usize,
}

/// Canonical set of validated backtest windows. A run is labeled `"VALIDATED"`
/// only when its `--window-label` is a member of this set AND the multi-window
/// driver asserted `--validated-set-complete`. This list extends as data
/// accrues (add the next month's window once its tape is pinned).
pub const VALIDATED_WINDOWS: &[&str] = &["feb2026", "mar2026", "apr2026", "may2026", "jun2026"];

/// Compute the validation label for a run. Returns `"VALIDATED"` only when
/// `window_label` is a member of [`VALIDATED_WINDOWS`] and the caller asserted
/// the full validated set ran; otherwise `"UNVALIDATED"`. A single run passing
/// `--validated-set-complete` without a canonical label is still unvalidated.
pub fn validation_label(window_label: Option<&str>, validated_set_complete: bool) -> String {
    match window_label {
        Some(label) if validated_set_complete && VALIDATED_WINDOWS.contains(&label) => {
            "VALIDATED".to_string()
        }
        _ => "UNVALIDATED".to_string(),
    }
}

/// Sizing-realism summary: what the run's P&L looks like under fractional
/// sizing at a real bankroll, with the standing realization haircut (winners
/// scaled by 0.82, losers full size) and the 5-share floor / ruin check.
///
/// The 0.82 haircut is the standing realization-haircut convention from
/// `docs/drawdown-sizing-2026-07.md`: winning positions are discounted to
/// reflect that not all of a backtested win is realized at size in production,
/// while losing positions are taken at full size (the pessimistic assumption).
#[derive(Debug, Clone, Serialize)]
pub struct SizingRealism {
    pub bankroll_usd: f64,
    pub clip_fraction: f64,
    pub raw_net_pnl: f64,
    pub haircut_net_pnl: f64,
    pub min_clip_usd: f64,
    pub five_share_floor_breached: bool,
}

/// Apply the realization haircut to a slice of per-market P&Ls: winners scaled
/// by 0.82, losers taken at full size. `100 * 0.82 - 50 = 32` for `[+100, -50]`.
pub fn haircut_net_pnl(per_market_pnls: &[f64]) -> f64 {
    per_market_pnls
        .iter()
        .map(|&pnl| if pnl > 0.0 { pnl * 0.82 } else { pnl })
        .sum()
}

/// Median of a slice (linear interpolation of the two middle values for an
/// even-length slice). Returns `0.0` for an empty slice.
pub fn median_f64(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut s = values.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        0.5 * (s[n / 2 - 1] + s[n / 2])
    }
}

/// The 5-share floor is breached when the smallest clip cannot purchase 5
/// shares at the run's median entry price, i.e. `min_clip_usd < 5 * median_price`.
pub fn five_share_floor_breached(min_clip_usd: f64, median_entry_price: f64) -> bool {
    min_clip_usd < 5.0 * median_entry_price
}

/// Build the sizing-realism block from per-market results. `clip_fraction`
/// comes from `cfg.clip_fraction_of_equity` (or 0.01 when unset). The raw net
/// P&L, haircut, and median entry price are all derived from the run's
/// per-market, per-strategy results so a single strategy run and a combined
/// run are both handled consistently.
pub fn sizing_realism(
    results: &[MarketResult],
    strategies: &[StratId],
    bankroll_usd: f64,
    clip_fraction: f64,
) -> SizingRealism {
    let strategy_names: Vec<&'static str> = strategies.iter().map(|s| s.name()).collect();
    // Per-market net P&L summed across the active strategies, and every fill
    // price for the median-entry-price computation.
    let mut per_market_pnls: Vec<f64> = Vec::with_capacity(results.len());
    let mut fill_prices: Vec<f64> = Vec::new();
    for r in results {
        let mut market_pnl = 0.0;
        for name in &strategy_names {
            if let Some(s) = r.per_strategy.get(name) {
                market_pnl += s.pnl_usdc;
                for fill in &s.fills_detail {
                    fill_prices.push(fill.price as f64);
                }
            }
        }
        per_market_pnls.push(market_pnl);
    }
    let raw_net_pnl: f64 = per_market_pnls.iter().sum();
    let haircut = haircut_net_pnl(&per_market_pnls);
    let median_price = median_f64(&fill_prices);
    let min_clip_usd = bankroll_usd * clip_fraction;
    let breached = five_share_floor_breached(min_clip_usd, median_price);
    SizingRealism {
        bankroll_usd,
        clip_fraction,
        raw_net_pnl,
        haircut_net_pnl: haircut,
        min_clip_usd,
        five_share_floor_breached: breached,
    }
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
pub struct StrategyRunConfig {
    pub strategy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
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
    fn push_fill(&mut self, fill: &Fill, yes_resolved: bool) {
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
    fn push(&mut self, fill: &Fill, pnl: f64) {
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


pub fn meta_calibration_report(
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


pub fn prediction_distribution(predictions: &mut [f32]) -> PredictionDistribution {
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
pub struct CalibrationBinAccumulator {
    pub samples: usize,
    pub sum_predicted: f32,
    pub observed: usize,
}


#[derive(Debug, Clone, Copy, Default)]
pub struct MarketCalibrationAccumulator {
    pub samples: usize,
    pub observed: usize,
    pub base_log_loss: f32,
    pub calibrated_log_loss: f32,
    pub base_brier: f32,
    pub calibrated_brier: f32,
}


pub fn binary_log_loss(p: f32, observed: bool) -> f32 {
    let p = p.clamp(1.0e-6, 1.0 - 1.0e-6);
    if observed { -p.ln() } else { -(1.0 - p).ln() }
}

/// The summary watermark for a run, combining every applicable marker:
/// `"FANTASY"` when the config carries an explicit fantasy grant, and
/// `"MIXED-BASIS"` when a Binance spot tape is run against an Official strike
/// under `--allow-mixed-basis`. Multiple markers are comma-separated (e.g.
/// `"FANTASY,MIXED-BASIS"`). `None` for an unmarked truthful same-basis run.
pub fn run_watermark(cfg: &WalkForwardConfig) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    if cfg.fantasy {
        parts.push("FANTASY");
    }
    if cfg.allow_mixed_basis
        && cfg.spot_source == crate::config::SpotSource::Binance
        && cfg.strike_source == crate::config::StrikeSource::Official
    {
        parts.push("MIXED-BASIS");
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(","))
    }
}


pub fn write_portfolio_checkpoint(
    cfg: &WalkForwardConfig,
    results: &[MarketResult],
    meta_report: Option<&MetaCalibrationReport>,
    meta_calibrator_snapshot: Option<&OnlineMetaCalibratorSnapshot>,
    oos_meta_samples: &[MetaTrainingSample],
) -> Result<()> {
    let mut summary = aggregate(results, &cfg.strategies);
    summary.config_fingerprint = Some(config_fingerprint(cfg));
    summary.watermark = run_watermark(cfg);
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


pub fn aggregate_for_strategy(records: &[&StrategyMarketResult]) -> StrategyAggregate {
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
    }
}


pub fn aggregate(results: &[MarketResult], strategies: &[StratId]) -> WalkForwardSummary {
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
        config_fingerprint: None,
        watermark: None,
        run_config: None,
        per_strategy,
        by_volatility_band,
        fold_summaries: Vec::new(),
        meta_calibration: None,
        jitter: None,
        window_label: None,
        validation: "UNVALIDATED".to_string(),
        sizing: None,
        era_breakdown: None,
    }
}


pub fn summary_run_config(cfg: &WalkForwardConfig) -> SummaryRunConfig {
    let mut strategies = Vec::with_capacity(cfg.strategies.len());
    for strat in &cfg.strategies {
        strategies.push(StrategyRunConfig {
            strategy: strat.name(),
            config: None,
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

    #[test]
    fn single_run_is_unvalidated() {
        // No window label and no completeness flag: unvalidated.
        assert_eq!(validation_label(None, false), "UNVALIDATED");
        // A canonical label but no completeness assertion (a single run): still
        // unvalidated. A single run can never claim validated status by itself.
        assert_eq!(validation_label(Some("feb2026"), false), "UNVALIDATED");
        // Completeness flag set but the label is not canonical: unvalidated.
        assert_eq!(validation_label(Some("research_only"), true), "UNVALIDATED");
        // Non-canonical label without the flag: unvalidated.
        assert_eq!(validation_label(Some("jan2026"), false), "UNVALIDATED");
        // Only a canonical label AND the completeness flag validates.
        assert_eq!(validation_label(Some("feb2026"), true), "VALIDATED");
        assert_eq!(validation_label(Some("jun2026"), true), "VALIDATED");

        // A fresh aggregate summary (the single-run path) defaults to UNVALIDATED.
        let summary = aggregate(&[], &[]);
        assert_eq!(summary.validation, "UNVALIDATED");
        assert!(summary.window_label.is_none());
        assert!(summary.sizing.is_none());
    }

    #[test]
    fn haircut_scales_only_winners() {
        // Two per-market P&Ls: +100 (winner) and -50 (loser).
        // Winners scale by 0.82, losers stay full size: 100*0.82 - 50 = 32.
        let haircut = haircut_net_pnl(&[100.0, -50.0]);
        assert!((haircut - 32.0).abs() < 1e-9, "haircut {haircut}");

        // A flat-zero market contributes nothing.
        let h2 = haircut_net_pnl(&[100.0, 0.0, -50.0]);
        assert!((h2 - 32.0).abs() < 1e-9, "h2 {h2}");

        // All winners: pure 0.82 scaling.
        let h3 = haircut_net_pnl(&[10.0, 20.0, 30.0]);
        assert!((h3 - 0.82 * 60.0).abs() < 1e-9, "h3 {h3}");

        // All losers: no scaling, full loss retained.
        let h4 = haircut_net_pnl(&[-10.0, -20.0]);
        assert!((h4 - (-30.0)).abs() < 1e-9, "h4 {h4}");

        // Empty: zero.
        assert_eq!(haircut_net_pnl(&[]), 0.0);
    }

    #[test]
    fn five_share_floor_flags_small_bankroll() {
        // min_clip = bankroll * fraction. The floor is breached when the clip
        // cannot buy 5 shares at the median entry price:
        //   min_clip_usd < 5 * median_price.
        //
        // Small bankroll: 200 * 0.01 = $2.00 clip; 5 shares at $0.50 = $2.50.
        // $2.00 < $2.50 -> breached.
        let min_clip_small = 200.0 * 0.01;
        assert!(five_share_floor_breached(min_clip_small, 0.5));
        // Larger bankroll: 300 * 0.01 = $3.00 clip; $3.00 >= $2.50 -> not breached.
        let min_clip_ok = 300.0 * 0.01;
        assert!(!five_share_floor_breached(min_clip_ok, 0.5));
        // Boundary: clip exactly 5 * price is not a breach (strict <).
        assert!(!five_share_floor_breached(2.5, 0.5));
        // A higher median price makes the same clip breach.
        assert!(five_share_floor_breached(3.0, 0.7));
    }
}
