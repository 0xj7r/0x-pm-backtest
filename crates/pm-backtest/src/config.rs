//! Runner configuration for a single `run_backtest` invocation.

use pm_model::{
    ModelConfig, ModelMarketContext, ModelState, OnlineMetaCalibratorSnapshot,
};
use pm_risk::PortfolioLimits;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub starting_cash_usdc: f64,
    /// Market open timestamp in nanoseconds. When set, replay ignores stale
    /// book snapshots before this point and timing gates are measured from it.
    pub market_open_ns: i64,
    pub market_close_ns: i64,
    pub resolved_yes: Option<bool>,
    pub portfolio_limits: PortfolioLimits,
    pub equity_curve_jsonl: Option<PathBuf>,
    pub snapshot_every_n: usize,
    /// Maker rebate (in basis points of notional). Polymarket has run
    /// programs in the 5–20 bps range — default 0 keeps it neutral.
    pub maker_rebate_bps: f64,
    /// Taker fee (bps). Default 0; configure per market regime.
    pub taker_fee_bps: f64,
    /// If |yes_shares - no_shares| exceeds this AFTER a maker fill, cancel
    /// resting orders on the heavy side. Critical safety for paired-MM
    /// strategies: without this, a one-sided book trend can run inventory
    /// far beyond the strategy's emission caps. `f64::INFINITY` disables.
    pub max_inventory_imbalance_shares: f64,
    /// Slippage on taker fills (basis points). Worsens the fill price (buyer
    /// pays more, seller gets less). Approximates queue / latency cost
    /// between strategy decision and venue execution. Default 0.
    pub taker_slippage_bps: f64,
    /// Delay between strategy decision and taker execution. A non-zero value
    /// executes against the first later book snapshot at or after this delay.
    pub taker_latency_ms: u64,
    /// Optional per-decision log (JSONL). Useful for attribution and
    /// post-hoc analysis of every strategy callback.
    pub decision_log_jsonl: Option<PathBuf>,
    /// Optional per-decision attribution log (Parquet).
    pub decision_log_parquet: Option<PathBuf>,
    /// Strategy name for decision-log rows when a shared walk-forward run logs
    /// multiple strategies to the same file.
    pub strategy_name: String,
    /// Optional shared canonical model state for walk-forward or portfolio
    /// calibration continuity across markets.
    pub shared_model_state: Option<Arc<Mutex<ModelState>>>,
    /// Allow `record_market_result` to update shared/local model state after
    /// resolution. Disable for frozen snapshot evaluation.
    pub update_model_state_on_resolution: bool,
    /// Frozen meta-calibrator snapshot loaded into a local canonical model
    /// state. Intended for walk-forward test folds.
    pub meta_calibrator_snapshot: Option<OnlineMetaCalibratorSnapshot>,
    /// Enable the online meta-calibrator adjustment in the canonical model.
    /// Disable this for strategy-only A/B runs against the hand-crafted score.
    pub enable_meta_calibration: bool,
    /// Optional asset/timeframe context for explicit mixed-universe calibration.
    /// Defaults to unknown so BTC 5m baseline runs remain comparable unless a
    /// caller opts into market-context features.
    pub model_market_context: ModelMarketContext,
    /// Replay-safe prior completed-market range features for strategy regime
    /// gates. In portfolio mode these are computed before the current market.
    pub prior_market_range_1d: f32,
    pub prior_market_range_3d: f32,
    pub prior_market_range_7d: f32,
    /// Weight for BTC spot whipsaw risk in the canonical model risk score.
    pub model_btc_whipsaw_risk_weight: f32,
    /// Weight for BTC spot path inefficiency in the canonical model risk score.
    pub model_btc_path_inefficiency_risk_weight: f32,
    /// Weight for short-term BTC reversal pressure in the canonical model risk score.
    pub model_btc_reversal_pressure_risk_weight: f32,
    /// Log every Nth decision event to avoid enormous files.
    pub decision_log_every_n: usize,
    /// Gate per-tick orders by model-derived entry constraints.
    pub enforce_model_gate: bool,
    /// Minimum `ModelOutput::confidence_score` required for an order.
    pub model_gate_min_confidence: f32,
    /// Maximum `ModelOutput::risk_score` allowed for an order.
    pub model_gate_max_risk: f32,
    /// Minimum edge over implied side probability required for an order.
    pub model_gate_min_edge: f32,

    /// Current net ladder exposure for the asset (yes - no shares summed across open windows).
    /// Used by BackToExplore and similar strategies for cross-market hedging and sizing.
    /// Populated by the walk-forward harness in portfolio mode.
    pub current_btc_net_shares: f64,
    pub current_eth_net_shares: f64,

    /// Daily loss tracking for smart capping inside strategies (e.g. BackToExplore
    /// can still do pair/repair on capped days instead of blunt stop-everything).
    pub daily_start_cash_usdc: f64,
    pub daily_loss_cap_pct: f64,

    /// Current realized loss this day (fraction of daily_start_equity). Passed from
    /// walkforward so strategies can adapt sizing/pair/target instead of hard zeroing.
    pub current_daily_loss_pct: f64,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            starting_cash_usdc: 100.0,
            market_open_ns: 0,
            market_close_ns: 0,
            resolved_yes: None,
            portfolio_limits: PortfolioLimits::default(),
            equity_curve_jsonl: None,
            snapshot_every_n: 200,
            maker_rebate_bps: 0.0,
            taker_fee_bps: 0.0,
            max_inventory_imbalance_shares: f64::INFINITY,
            taker_slippage_bps: 0.0,
            taker_latency_ms: 0,
            decision_log_jsonl: None,
            decision_log_parquet: None,
            strategy_name: "unknown".to_string(),
            shared_model_state: None,
            update_model_state_on_resolution: true,
            meta_calibrator_snapshot: None,
            enable_meta_calibration: true,
            model_market_context: ModelMarketContext::default(),
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_btc_whipsaw_risk_weight: ModelConfig::default().btc_whipsaw_risk_weight,
            model_btc_path_inefficiency_risk_weight: ModelConfig::default()
                .btc_path_inefficiency_risk_weight,
            model_btc_reversal_pressure_risk_weight: ModelConfig::default()
                .btc_reversal_pressure_risk_weight,
            decision_log_every_n: 1,
            enforce_model_gate: false,
            model_gate_min_confidence: 0.68,
            model_gate_max_risk: 0.72,
            model_gate_min_edge: 0.05,
            current_btc_net_shares: 0.0,
            current_eth_net_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
        }
    }
}
