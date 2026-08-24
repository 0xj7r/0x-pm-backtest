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

use anyhow::{Result, anyhow};
use pm_model::MetaTrainingConfig;
use serde::{Deserialize, Serialize};

use crate::engine::{DEFAULT_META_MAX_FIT_SAMPLES, DEFAULT_META_MAX_OOS_EVALUATION_SAMPLES, DEFAULT_META_MAX_SAMPLES_PER_MARKET, DEFAULT_META_MAX_VALIDATION_SAMPLES, StratId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketHandle {
    pub asset_id: String,
    pub slug: String,
    /// Resolution timestamp (Unix seconds, UTC).
    pub close_ts: i64,
    /// Outcome label as returned by Telonex (e.g. "Up", "Down", "Yes", "No").
    pub outcome: String,
    /// Date partition (`YYYY-MM-DD`).
    pub date: String,
}

pub fn spot_symbol_for_market(configured: &str, slug: &str) -> Result<Option<String>> {
    if configured.is_empty() {
        return Ok(None);
    }
    if !configured.eq_ignore_ascii_case("auto") {
        return Ok(Some(configured.to_string()));
    }
    infer_spot_symbol_from_slug(slug)
        .map(|symbol| Some(symbol.to_string()))
        .ok_or_else(|| anyhow!("cannot infer spot symbol from market slug: {slug}"))
}

pub fn spot_cache_key(symbol: &str, date: &str) -> String {
    format!("{}|{}", symbol.to_ascii_uppercase(), date)
}

fn infer_spot_symbol_from_slug(slug: &str) -> Option<&'static str> {
    let slug = slug.to_ascii_lowercase();
    if slug.starts_with("btc-updown-") {
        Some("BTCUSDT")
    } else if slug.starts_with("eth-updown-") {
        Some("ETHUSDT")
    } else if slug.starts_with("sol-updown-") {
        Some("SOLUSDT")
    } else if slug.starts_with("xrp-updown-") {
        Some("XRPUSDT")
    } else {
        None
    }
}

/// Parse `btc-updown-5m-1778587500` -> 1778587500.
pub fn parse_close_ts(slug: &str) -> Option<i64> {
    slug.rsplit('-').next().and_then(|t| t.parse::<i64>().ok())
}


#[derive(Debug, Clone, serde::Serialize)]
pub struct WalkForwardConfig {
    pub starting_cash_usdc: f64,
    pub kelly_fraction: f64,
    pub max_clip_usdc: f64,
    pub max_order_clip_multiplier: f64,
    pub max_per_market_exposure_usdc: f64,
    pub max_per_market_exposure_frac: Option<f64>,
    pub spot_symbol: String,
    /// Binance USD-M futures symbol for exo_fade perp-led belief (e.g. BTCUSDT).
    /// When unset and exo_fade is active, defaults to `spot_symbol` if not `auto`.
    pub perp_symbol: Option<String>,
    /// Cache root for perp parquets (defaults to `data/cache`).
    pub perp_cache_dir: Option<PathBuf>,
    /// Experimental clean-directional tilt for exo_fade (0 = off = validated baseline).
    pub directional_tilt_strength: f64,
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
    /// Explicit grant to run below `TRUTHFUL_LATENCY_FLOOR_MS`. Fantasy runs
    /// proceed but are watermarked (`"FANTASY"`) in the summary and output
    /// filenames so they are never mistaken for a truthful backtest.
    pub fantasy: bool,
    /// Number of jittered-latency replay runs. `0` (default) runs the walk
    /// forward once with a point-estimate P&L. `N > 0` runs it N times at
    /// deterministically seeded perturbed latencies and attaches a p10/p50/p90
    /// P&L spread to the summary. Runs are serial, so wall time scales ~Nx.
    pub jitter: usize,
    /// Half-width of the uniform latency jitter band around
    /// `taker_latency_ms` (ms). Each jittered draw is a uniform integer in
    /// `[taker_latency_ms - spread, taker_latency_ms + spread]`, clamped up to
    /// `TRUTHFUL_LATENCY_FLOOR_MS` unless `fantasy` is granted.
    pub jitter_latency_spread_ms: u64,
    /// Seed for the jitter PRNG (splitmix64, inlined; no new deps). Same seed
    /// yields the same latency vec across runs for reproducibility.
    pub jitter_seed: u64,
    /// Optional multi-window validation label (e.g. `feb2026`). When set, the
    /// summary records it and the validation status is derived from whether it
    /// is a member of the canonical validated set AND the caller asserted the
    /// full set ran. `None` for ordinary single runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_label: Option<String>,
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
            perp_symbol: None,
            perp_cache_dir: None,
            directional_tilt_strength: 0.0,
            strategies: StratId::ACTIVE.to_vec(),
            max_concurrent_fetches: 64,
            replay_sample_ms: 0,
            replay_event_cache_dir: None,
            load_pm_trades: true,
            use_outcome_label: false,
            maker_rebate_bps: 0.0,
            taker_fee_bps: 0.0,
            taker_latency_ms: 0,
            fantasy: false,
            jitter: 0,
            jitter_latency_spread_ms: 250,
            jitter_seed: 42,
            window_label: None,
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
