#![recursion_limit = "256"]

use anyhow::{Context, Result, anyhow};
use arrow::array::{Array, Int64Array, StringArray};
use arrow::record_batch::RecordBatch;
use chrono::{DateTime, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use pm_model::MetaTrainingConfig;
use pm_risk::PortfolioLimits;
use pm_strategy::{
    BonereaperV2, PairedMmDense,
    bonereaper_v2::BonereaperV2Config, paired_mm::PairedMmDenseConfig,
};
use pm_telonex_loader::{
    Channel, TelonexStore, TelonexStoreConfig, load_binance_agg_trades_async,
    load_book_snapshot_async, load_pm_trades_async, polymarket_instrument_id, resolve_binance_day,
    resolve_pm_trades_day, to_quote_tick,
};
use pm_types::{MarketId, SpotHistory, TradeHistory};
use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

mod alpha;
mod discovery;
mod engine_driver;
mod prep_cache;
mod result_summary;
mod runner;
mod walkforward;

use result_summary::{print_result_summary, summarize_markets_jsonl, write_result_summary_json};
use runner::{RunnerConfig, pretty_print, run_backtest};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use walkforward::{
    StratId, StrategyProfileFile, WalkForwardConfig, print_summary, run_walkforward,
    write_market_results_jsonl_atomic, write_summary_json_atomic,
};

#[derive(Parser, Debug)]
#[command(
    name = "pm-app",
    version,
    about = "Polymarket backtest engine (Nautilus pure Rust)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum StrategyKind {
    PairedMm,
    BonereaperV2,
}

#[derive(Debug, Clone, Copy)]
enum MarketRunMode {
    Backtest,
    Paper,
    Live,
}

impl MarketRunMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Backtest => "backtest",
            Self::Paper => "paper",
            Self::Live => "live",
        }
    }
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// pm-alpha exogenous edge hunt: replay markets through the pm-alpha
    /// validation harness (latency-modeled, cost-aware, leakage-free belief).
    Alpha {
        /// Markets JSONL (MarketHandle rows, e.g.
        /// data/manifests/may2026_focused/markets_btc.jsonl).
        #[arg(long)]
        markets: PathBuf,
        /// Slug prefix filter.
        #[arg(long, default_value = "btc-updown-5m-")]
        slug_prefix: String,
        /// Inclusive date range start (YYYY-MM-DD).
        #[arg(long)]
        date_start: Option<String>,
        /// Inclusive date range end (YYYY-MM-DD).
        #[arg(long)]
        date_end: Option<String>,
        /// Cap on number of markets (0 = all).
        #[arg(long, default_value = "0")]
        max_markets: usize,
        /// Read from a local cache mirror instead of S3.
        #[arg(long)]
        local_cache_dir: Option<PathBuf>,
        /// ReplayEvent disk cache for repeated runs.
        #[arg(long)]
        replay_event_cache_dir: Option<PathBuf>,
        /// Entry latency in ms (single run); ignored when --latency-sweep.
        #[arg(long, default_value = "150")]
        latency_ms: u64,
        /// Sweep latencies 0/50/150/300/500/1000 ms.
        #[arg(long)]
        latency_sweep: bool,
        /// Comma-separated edge thresholds to grid over.
        #[arg(long, value_delimiter = ',', default_value = "0.05")]
        edge_thresholds: Vec<f64>,
        #[arg(long, default_value = "0.0")]
        fee_bps: f64,
        #[arg(long, default_value = "50.0")]
        notional_usdc: f64,
        #[arg(long, default_value = "1000")]
        decision_dt_ms: u64,
        #[arg(long, default_value = "10")]
        stop_before_close_s: u32,
        /// Max laddered clip entries per market.
        #[arg(long, default_value = "1")]
        max_clips: u32,
        /// Minimum ms between clip entries.
        #[arg(long, default_value = "5000")]
        clip_cooldown_ms: u64,
        /// Exit at the book N seconds after fill (0 = hold to resolution).
        #[arg(long, default_value = "0")]
        exit_after_s: u32,
        /// Skip entries in calm_low_vol regime.
        #[arg(long)]
        skip_calm: bool,
        /// Infer missing outcome labels from the final tape mid.
        #[arg(long)]
        infer_outcome: bool,
        #[arg(long, default_value = "1800")]
        vol_lookback_s: u32,
        /// 0 disables the momentum drift term (base model).
        #[arg(long, default_value = "0")]
        momentum_lookback_s: u32,
        #[arg(long, default_value = "1.0")]
        momentum_weight: f64,
        /// Write the full JSON report here.
        #[arg(long)]
        out_json: Option<PathBuf>,
        /// Train the exogenous calibrator on dates strictly before this
        /// (YYYY-MM-DD) and evaluate on dates at-or-after it.
        #[arg(long)]
        calibrate_split: Option<String>,
        /// Save the trained calibrator snapshot (JSON).
        #[arg(long)]
        calibrator_out: Option<PathBuf>,
        /// Load a calibrator snapshot instead of training.
        #[arg(long)]
        calibrator_in: Option<PathBuf>,
        /// Dump per-trade records (first grid cell) to this JSONL path.
        #[arg(long)]
        trades_out: Option<PathBuf>,
        /// JSONL of Down-token assets (metadata discovery, --token-outcome
        /// Down); enables real NO ladders instead of the synthetic 1-yes.
        #[arg(long)]
        down_assets: Option<PathBuf>,
        /// Compact merged-tick cache dir (bincode+zstd; ~10x smaller than
        /// parquet re-decode, written through on miss).
        #[arg(long)]
        tick_cache_dir: Option<PathBuf>,
    },
    /// Stream a Telonex book_snapshot parquet from S3 and print sanity stats.
    InspectS3 {
        #[arg(long, default_value = "polymarket")]
        exchange: String,
        #[arg(long, default_value = "book_snapshot_25")]
        channel: String,
        #[arg(long)]
        date: String,
        #[arg(long)]
        asset_id: String,
        #[arg(long, default_value = "1")]
        market_id: u32,
        #[arg(long, default_value = "5")]
        head: usize,
        /// Read from a local cache mirror instead of S3.
        #[arg(long)]
        local_cache_dir: Option<PathBuf>,
    },
    /// Run a backtest on one Polymarket asset using the selected strategy.
    BacktestS3 {
        #[arg(long, default_value = "polymarket")]
        exchange: String,
        #[arg(long, default_value = "book_snapshot_25")]
        channel: String,
        #[arg(long)]
        date: String,
        #[arg(long)]
        asset_id: String,
        /// Slug of the market (e.g. btc-updown-5m-1778587500). Used to parse
        /// the resolution timestamp when --close-ts is omitted.
        #[arg(long)]
        slug: Option<String>,
        /// Resolution Unix epoch seconds. Overrides --slug parsing.
        #[arg(long)]
        close_ts: Option<i64>,
        /// Force the resolution outcome (true = YES won). When omitted, infer
        /// from final yes_mid >= 0.5.
        #[arg(long)]
        resolved_yes: Option<bool>,
        #[arg(long, default_value = "1")]
        market_id: u32,
        #[arg(long, value_enum, default_value = "bonereaper-v2")]
        strategy: StrategyKind,
        #[arg(long, default_value = "100.0")]
        starting_cash: f64,
        #[arg(long, default_value = "5.0")]
        max_clip_usdc: f64,
        #[arg(long, default_value = "0.30")]
        max_drawdown_pct: f64,
        #[arg(long, default_value = "250.0")]
        max_daily_exposure_usdc: f64,
        /// Binance spot symbol to load for momentum + regime signals (e.g.
        /// BTCUSDT). Set to empty string to disable spot.
        #[arg(long, default_value = "BTCUSDT")]
        spot_symbol: String,
        /// If set, write the JSON report to this path.
        #[arg(long)]
        out: Option<PathBuf>,
        /// If set, write per-snapshot portfolio rows (JSONL) to this path.
        #[arg(long)]
        equity_curve: Option<PathBuf>,
        /// Write per-decision attribution rows (JSONL) to this path.
        #[arg(long)]
        decision_log: Option<PathBuf>,
        /// Only log every Nth decision event when writing `--decision-log`.
        #[arg(long, default_value = "1")]
        decision_log_every_n: usize,
        /// Read data from a local cache mirror instead of S3.
        #[arg(long)]
        local_cache_dir: Option<PathBuf>,
    },
    /// Stream the tape from S3 and emit Nautilus QuoteTicks (validates that
    /// nautilus-model types are usable downstream).
    QuotesS3 {
        #[arg(long, default_value = "polymarket")]
        exchange: String,
        #[arg(long, default_value = "book_snapshot_25")]
        channel: String,
        #[arg(long)]
        date: String,
        #[arg(long)]
        asset_id: String,
        #[arg(long)]
        slug: String,
        #[arg(long, default_value = "1")]
        market_id: u32,
        #[arg(long, default_value = "5")]
        head: usize,
        /// Read from a local cache mirror instead of S3.
        #[arg(long)]
        local_cache_dir: Option<PathBuf>,
    },
    /// Discover Polymarket BTC-updown-5m markets for a given date and write
    /// the (asset_id, slug, close_ts, outcome) list to JSONL.
    DiscoverDay {
        #[arg(long)]
        date: String,
        #[arg(long, default_value = "btc-updown-5m-")]
        slug_prefix: String,
        #[arg(long, default_value = "32")]
        max_concurrent: usize,
        /// JSONL cache for Telonex asset_id -> slug/outcome availability lookups.
        #[arg(long)]
        availability_cache: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Discover markets from a local cache mirror without listing S3.
    DiscoverLocalCacheDay {
        #[arg(long)]
        cache_dir: PathBuf,
        #[arg(long)]
        date: String,
        #[arg(long, default_value = "btc-updown-5m-")]
        slug_prefix: String,
        #[arg(long, default_value = "32")]
        max_concurrent: usize,
        /// Limit availability lookups for smoke/local iteration. 0 means all cached assets.
        #[arg(long, default_value = "0")]
        max_assets: usize,
        /// JSONL cache for Telonex asset_id -> slug/outcome availability lookups.
        #[arg(long)]
        availability_cache: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Discover BTC-updown-5m markets directly from local cached book parquet
    /// metadata. This avoids Telonex availability lookups.
    DiscoverLocalCacheBookMetadata {
        #[arg(long)]
        cache_dir: PathBuf,
        #[arg(long)]
        date: String,
        #[arg(long, default_value = "btc-updown-5m-")]
        slug_prefix: String,
        /// Canonical token side to backtest. Use `Up` for BTC up/down YES.
        #[arg(long, default_value = "Up")]
        token_outcome: String,
        #[arg(long)]
        out: PathBuf,
    },
    /// Discover Polymarket BTC-updown-5m markets for a date range.
    DiscoverRange {
        #[arg(long)]
        start_date: String,
        #[arg(long)]
        end_date: String,
        #[arg(long, default_value = "btc-updown-5m-")]
        slug_prefix: String,
        #[arg(long, default_value = "32")]
        max_concurrent: usize,
        /// JSONL cache for Telonex asset_id -> slug/outcome availability lookups.
        #[arg(long)]
        availability_cache: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Generate MarketHandle JSONL from the master Polymarket markets parquet.
    DiscoverMarketsParquet {
        #[arg(long)]
        markets_parquet: PathBuf,
        #[arg(long)]
        start_date: String,
        #[arg(long)]
        end_date: String,
        /// Slug prefix or comma-separated prefixes.
        #[arg(long, default_value = "btc-updown-5m-")]
        slug_prefix: String,
        #[arg(long, default_value_t = false)]
        require_book_s3: bool,
        #[arg(long)]
        out: PathBuf,
    },
    /// Pre-download all parquets for a market list to a local cache directory.
    /// Once cached, walk-forward with `--local-cache-dir <dir>` is mmap-fast.
    PrepCache {
        #[arg(long)]
        markets: PathBuf,
        #[arg(long)]
        cache_dir: PathBuf,
        #[arg(long, default_value = "BTCUSDT")]
        spot_symbol: String,
        #[arg(long, default_value = "32")]
        max_concurrent: usize,
        #[arg(long, default_value_t = true)]
        skip_existing: bool,
    },
    /// Run the standalone pm-engine BTC-5m backtest (faithful both-book fill
    /// model). `walk-forward` remains the default path; run both to measure the
    /// both-book P&L delta. Discovers YES+NO leg pairs from the local book cache.
    EngineBacktest {
        #[arg(long)]
        local_cache_dir: PathBuf,
        #[arg(long)]
        start_date: String,
        #[arg(long)]
        end_date: String,
        #[arg(long, default_value = "btc-updown-5m")]
        slug_prefix: String,
        #[arg(long, default_value = "BTCUSDT")]
        spot_symbol: String,
        #[arg(long, default_value = "data/snap062901.json")]
        meta_calibrator_snapshot_in: PathBuf,
        #[arg(long, default_value_t = 1000.0)]
        starting_cash: f64,
        #[arg(long, default_value_t = 30.0)]
        max_clip_usdc: f64,
        #[arg(long, default_value_t = 500)]
        taker_latency_ms: u64,
        #[arg(long, default_value_t = 0.0)]
        taker_fee_bps: f64,
        #[arg(long, default_value_t = 0.0)]
        maker_rebate_bps: f64,
        /// Book-event thinning in ms (champion ran 1000). 0 = keep every event.
        #[arg(long, default_value_t = 1000)]
        replay_sample_ms: i64,
        /// Cap on markets (for small fixed slices). Omit to use all discovered.
        #[arg(long)]
        max_markets: Option<usize>,
        /// Strategy to run: bonereaper_v2 (legacy taker) or convex (directional convex-book).
        #[arg(long, default_value = "convex")]
        strategy: String,
        /// Convex: minimum model edge over entry price to gate a favourite load.
        #[arg(long, default_value_t = 0.03)]
        signal_min_edge: f32,
        /// Convex: minimum model confidence to gate a favourite load.
        #[arg(long, default_value_t = 0.68)]
        signal_min_confidence: f32,
        /// Convex: maximum model risk to gate a favourite load.
        #[arg(long, default_value_t = 0.72)]
        signal_max_risk: f32,
        /// Convex: seconds into the market window before favourite loads begin.
        #[arg(long, default_value_t = 180.0)]
        favourite_start_secs: f32,
        /// Convex: favourite loads stop when secs_to_close drops below this floor (0 = disabled).
        #[arg(long, default_value_t = 0.0)]
        favourite_stop_secs_before_close: f32,
        /// Convex: tail target as fraction of favourite shares (1.0 = share-balanced).
        #[arg(long, default_value_t = 1.0)]
        tail_balance_frac: f64,
    },
    /// Run a walk-forward backtest over many markets.
    WalkForward {
        /// JSONL of `MarketHandle` rows from `discover-day`.
        #[arg(long)]
        markets: PathBuf,
        /// Optional strategy profile (TOML). Profile values override CLI/default
        /// values for the fields present in the profile.
        #[arg(long)]
        profile: Option<PathBuf>,
        /// Optional per-market BTE risk scale JSONL exported by
        /// scripts/bte_cluster_policy_search.py --out-scale-jsonl.
        #[arg(long)]
        back_to_explore_policy_scales_jsonl: Option<PathBuf>,
        /// Chronological offset for smoke/diagnostic slices.
        #[arg(long, default_value_t = 0)]
        skip_markets: usize,
        /// Chronological cap for smoke/diagnostic runs. 0 means use all markets.
        #[arg(long, default_value_t = 0)]
        max_markets: usize,
        #[arg(long, default_value = "100.0")]
        starting_cash: f64,
        #[arg(long, default_value = "0.25")]
        kelly_fraction: f64,
        #[arg(long, default_value = "5.0")]
        max_clip_usdc: f64,
        /// Per-order risk cap as a multiple of the base strategy clip. Allows
        /// heavy lanes while `max_clip_usdc` still defines normal entry size.
        #[arg(long, default_value = "2.0")]
        max_order_clip_multiplier: f64,
        /// Maximum cumulative gross buy outlay allowed per market.
        #[arg(long, default_value = "50.0")]
        max_per_market_exposure_usdc: f64,
        /// Optional portfolio-mode cap for per-market gross buy outlay as a fraction of equity.
        #[arg(long)]
        max_per_market_exposure_frac: Option<f64>,
        #[arg(long, default_value = "BTCUSDT")]
        spot_symbol: String,
        /// Comma-separated active strategy IDs.
        ///
        /// Active set by default:
        /// `back_to_explore,paired_mm,bonereaper_v2`.
        /// Use `--allow-legacy-strategies` to enable legacy names.
        #[arg(long, default_value = "back_to_explore,paired_mm,bonereaper_v2")]
        strategies: String,
        /// Allow previously archived strategy identifiers (for historical experiments).
        #[arg(long, default_value_t = false)]
        allow_legacy_strategies: bool,
        #[arg(long, default_value = "64")]
        max_concurrent_fetches: usize,
        /// Research-speed replay thinning in milliseconds. 0 keeps every raw event.
        #[arg(long, default_value = "0")]
        replay_sample_ms: u64,
        /// Simulated delay before taker orders execute against the book.
        #[arg(long, default_value = "0")]
        taker_latency_ms: u64,
        /// Directory for on-disk cache of raw ReplayEvents (JSONL). Huge win on AWS
        /// for repeated runs on the same dates — avoids re-downloading parquets from S3.
        #[arg(long)]
        replay_event_cache_dir: Option<PathBuf>,
        /// Skip loading Polymarket trade prints and run with empty trade-flow history.
        #[arg(long, default_value_t = false)]
        disable_pm_trades: bool,
        /// If set, use the outcome label from discovery instead of inferring
        /// from yes_mid.
        #[arg(long, default_value_t = false)]
        use_outcome_label: bool,
        /// Portfolio mode: process markets in chronological order, compound
        /// equity across markets. Disables parallelism.
        #[arg(long, default_value_t = false)]
        portfolio_mode: bool,
        /// Per-market volatility split for summary stats: `high` if
        /// max(yes_mid) - min(yes_mid) > threshold.
        #[arg(long, default_value = "0.08")]
        volatility_regime_threshold: f64,
        /// In portfolio mode, override max_clip_usdc per market to be this
        /// fraction of current equity (e.g., 0.005 = 0.5% per bet).
        /// Omitted = use static max_clip_usdc.
        #[arg(long)]
        clip_fraction_of_equity: Option<f64>,
        /// Portfolio drawdown fraction where clip sizing starts scaling down.
        /// Example: 0.12 starts de-risking at 12% below peak. Disabled unless
        /// below --clip-drawdown-hard-pct.
        #[arg(long, default_value = "1.0")]
        clip_drawdown_soft_pct: f64,
        /// Portfolio drawdown fraction where clip sizing reaches zero.
        /// Example: 0.25 stops new sizing at 25% below peak.
        #[arg(long, default_value = "1.0")]
        clip_drawdown_hard_pct: f64,
        /// Minimum clip multiplier after the hard drawdown threshold.
        /// Example: 0.10 keeps recovery-sized trading instead of freezing.
        #[arg(long, default_value = "0.0")]
        clip_drawdown_min_multiplier: f64,
        /// Session/day drawdown fraction where clip sizing starts scaling down.
        /// Resets when the market date changes. Disabled unless below
        /// --clip-session-drawdown-hard-pct.
        #[arg(long, default_value = "1.0")]
        clip_session_drawdown_soft_pct: f64,
        /// Session/day drawdown fraction where clip sizing reaches the floor.
        #[arg(long, default_value = "1.0")]
        clip_session_drawdown_hard_pct: f64,
        /// Minimum clip multiplier after the session hard drawdown threshold.
        #[arg(long, default_value = "0.0")]
        clip_session_drawdown_min_multiplier: f64,
        /// Cap daily losses (from the equity at the start of the calendar day / session date)
        /// at this fraction of bankroll. E.g. 0.05 = stop/risk 0 for rest of day once down 5% from day's open equity.
        /// 1.0 or higher disables. Simple hard daily loss limit on top of drawdown scaling.
        #[arg(long, default_value = "1.0")]
        daily_loss_cap_pct: f64,
        /// Disable Bonereaper v2's internal model gates for pure heuristic strategy tests.
        #[arg(long, default_value_t = false)]
        br2_disable_internal_model_gates: bool,
        /// Bonereaper v2 market-neutral maker participation clip multiplier.
        #[arg(long, default_value = "0.0")]
        br2_participation_clip_frac: f32,
        /// Bonereaper v2 maximum paired maker entry cost.
        #[arg(long, default_value = "0.99")]
        br2_participation_max_pair_cost: f32,
        /// Bonereaper v2 maximum participation quote count per leg.
        #[arg(long, default_value = "500")]
        br2_participation_max_orders_per_leg: usize,
        /// Bonereaper v2 participation inventory hard cap in shares.
        #[arg(long, default_value = "25.0")]
        br2_participation_max_inventory_delta_shares: f64,
        /// Bonereaper v2 participation inventory repair threshold in shares.
        #[arg(long, default_value = "5.0")]
        br2_participation_repair_inventory_delta_shares: f64,
        /// Bonereaper v2 participation same-leg quote refresh seconds.
        #[arg(long, default_value = "0.50")]
        br2_participation_refresh_secs: f32,
        /// Bonereaper v2 seconds before close where participation maker quotes stop.
        #[arg(long, default_value = "20.0")]
        br2_participation_stop_secs_before_close: f32,
        /// Bonereaper v2 hedge-first arb-anchored base lane toggle.
        #[arg(long, default_value_t = false)]
        br2_hedged_base_enabled: bool,
        /// Bonereaper v2 hedge-first base only fires within this many seconds of window open.
        #[arg(long, default_value = "240.0")]
        br2_hedged_base_max_secs_in: f32,
        /// Bonereaper v2 hedge-first base max combined taker pair cost to lock arb.
        #[arg(long, default_value = "0.98")]
        br2_hedged_base_max_pair_cost: f32,
        /// Bonereaper v2 hedge-first base minimum minority-leg fraction of base book.
        #[arg(long, default_value = "0.20")]
        br2_hedged_base_min_minority_leg_frac: f32,
        /// Bonereaper v2 hedge-first base per-add clip in USDC.
        #[arg(long, default_value = "5.0")]
        br2_hedged_base_clip_usdc: f32,
        /// Bonereaper v2 hedge-first base total budget per market in USDC (0 disables).
        #[arg(long, default_value = "0.0")]
        br2_hedged_base_max_notional_usdc: f32,
        /// Bonereaper v2 cap on directional notional as a fraction of hedged base notional.
        #[arg(long, default_value = "1000000000.0")]
        br2_late_directional_overlay_frac: f32,
        /// Bonereaper v2 minimum composite direction for early/mid/late lanes.
        #[arg(long, default_value = "0.10")]
        br2_min_composite_direction: f32,
        /// Bonereaper v2 early directional clip multiplier.
        #[arg(long, default_value = "0.00")]
        br2_early_clip_frac: f32,
        /// Bonereaper v2 mid-ladder clip multiplier.
        #[arg(long, default_value = "0.00")]
        br2_mid_clip_frac: f32,
        /// Bonereaper v2 late confirmation clip multiplier.
        #[arg(long, default_value = "1.0")]
        br2_late_clip_frac: f32,
        /// Bonereaper v2 maximum late confirmation fires.
        #[arg(long, default_value = "3")]
        br2_late_max_fires: usize,
        /// Bonereaper v2 minimum ML confidence for late confirmation entries.
        #[arg(long, default_value = "0.58")]
        br2_late_confirm_min_model_confidence: f32,
        /// Bonereaper v2 maximum ML risk for late confirmation entries.
        #[arg(long, default_value = "0.80")]
        br2_late_confirm_max_model_risk: f32,
        /// Bonereaper v2 minimum ML predicted-side probability for late confirmation entries.
        #[arg(long, default_value = "0.58")]
        br2_late_confirm_min_model_side_p: f32,
        /// Bonereaper v2 minimum ML probability edge over entry price for late confirmation entries.
        #[arg(long, default_value = "0.02")]
        br2_late_confirm_min_model_edge: f32,
        /// Bonereaper v2 minimum absolute book skew from 0.5 for late confirmation entries.
        #[arg(long, default_value = "0.06")]
        br2_late_confirm_min_book_skew: f32,
        /// Bonereaper v2 maximum continuous whipsaw score for late confirmation entries.
        #[arg(long, default_value = "0.85")]
        br2_late_confirm_max_whipsaw_score: f32,
        /// Bonereaper v2 minimum BTC 180s realized volatility for late confirmation entries.
        #[arg(long, default_value = "0.0")]
        br2_late_confirm_min_realized_vol_180s_bps: f32,
        /// Bonereaper v2 maximum observed market range for late confirmation entries.
        #[arg(long, default_value = "1.0")]
        br2_late_confirm_max_observed_range: f32,
        /// Enable Bonereaper v2 replay-safe recent-regime logistic gate.
        #[arg(long, default_value_t = false)]
        br2_recent_regime_gate_enabled: bool,
        /// Minimum logistic win-probability edge over entry price for recent-regime gate.
        #[arg(long, default_value = "0.08")]
        br2_recent_regime_gate_min_edge: f32,
        /// Apply recent-regime gate to late confirmation entries.
        #[arg(long, default_value_t = true)]
        br2_recent_regime_gate_late_confirm: bool,
        /// Apply recent-regime gate to high-skew entries.
        #[arg(long, default_value_t = true)]
        br2_recent_regime_gate_high_skew: bool,
        /// Apply recent-regime gate to late-favourite entries.
        #[arg(long, default_value_t = true)]
        br2_recent_regime_gate_late_favourite: bool,
        /// Bonereaper v2 high-skew clip multiplier.
        #[arg(long, default_value = "0.60")]
        br2_high_skew_clip_frac: f32,
        /// Bonereaper v2 per-lane directional size multiplier (late favourite). 1.0 = no change.
        #[arg(long, default_value = "1.0")]
        br2_lane_size_late_favourite: f32,
        /// Bonereaper v2 per-lane directional size multiplier (late confirm). 1.0 = no change.
        #[arg(long, default_value = "1.0")]
        br2_lane_size_late_confirm: f32,
        /// Bonereaper v2 per-lane directional size multiplier (high skew). 1.0 = no change.
        #[arg(long, default_value = "1.0")]
        br2_lane_size_high_skew: f32,
        /// Bonereaper v2 regime-conditional lane gate. Ex-ante and inert by
        /// default. When set, amputates the directional lanes (late favourite +
        /// late confirm) toward the floor in whippy regimes; high skew untouched.
        #[arg(long, default_value = "false")]
        br2_regime_gate_enabled: bool,
        /// Trailing window for the regime score: 1, 3 or 7 (days of prior markets).
        #[arg(long, default_value = "3")]
        br2_regime_gate_window: u8,
        /// Trailing-range threshold above which the directional lanes are amputated.
        #[arg(long, default_value = "0.50")]
        br2_regime_gate_threshold: f32,
        /// Soft ramp width below the threshold. 0 = hard step.
        #[arg(long, default_value = "0.0")]
        br2_regime_gate_soft_band: f32,
        /// Directional lane multiplier when fully whippy. 0 = full amputation.
        #[arg(long, default_value = "0.0")]
        br2_regime_gate_lane_floor: f32,
        /// Blend weight in [0,1] for the live ex-ante whipsaw score. 0 = trailing-range only.
        #[arg(long, default_value = "0.0")]
        br2_regime_gate_whipsaw_weight: f32,
        /// Bonereaper v2 maximum high-skew load clips.
        #[arg(long, default_value = "5")]
        br2_high_skew_max_clips: usize,
        /// Bonereaper v2 maximum continuous whipsaw score for high-skew loads.
        #[arg(long, default_value = "0.75")]
        br2_high_skew_max_whipsaw_score: f32,
        /// Bonereaper v2 minimum BTC 180s realized volatility for high-skew loads.
        #[arg(long, default_value = "0.0")]
        br2_high_skew_min_realized_vol_180s_bps: f32,
        /// Bonereaper v2 seconds into market before late-favourite loads begin.
        #[arg(long, default_value = "180.0")]
        br2_late_favourite_start_secs: f32,
        /// Bonereaper v2 late-favourite absolute skew threshold from 0.5.
        #[arg(long, default_value = "0.22")]
        br2_late_favourite_threshold: f32,
        /// Bonereaper v2 minimum ask price for late-favourite loads.
        #[arg(long, default_value = "0.70")]
        br2_late_favourite_min_ask: f32,
        /// Bonereaper v2 maximum ask price for late-favourite loads.
        #[arg(long, default_value = "0.97")]
        br2_late_favourite_max_ask: f32,
        /// Bonereaper v2 late-favourite clip multiplier.
        #[arg(long, default_value = "1.00")]
        br2_late_favourite_clip_frac: f32,
        /// Bonereaper v2 late-favourite clip multiplier once ask is >= high-cert threshold.
        #[arg(long, default_value = "1.00")]
        br2_late_favourite_high_cert_clip_frac: f32,
        /// Bonereaper v2 high-cert edge where late-favourite loads reach full clip size.
        #[arg(long, default_value = "0.04")]
        br2_late_favourite_high_cert_full_clip_edge: f32,
        /// Ask threshold for fragile high-cert late-favourite size taper; disabled at 1.0.
        #[arg(long, default_value = "0.923")]
        br2_late_favourite_fragile_high_cert_ask: f32,
        /// Maximum model edge for fragile high-cert late-favourite size taper.
        #[arg(long, default_value = "0.005")]
        br2_late_favourite_fragile_high_cert_max_edge: f32,
        /// Maximum BTC path efficiency for fragile high-cert late-favourite size taper.
        #[arg(long, default_value = "0.50")]
        br2_late_favourite_fragile_high_cert_max_path_efficiency: f32,
        /// Size multiplier applied to fragile high-cert late-favourite loads.
        #[arg(long, default_value = "0.50")]
        br2_late_favourite_fragile_high_cert_size_frac: f32,
        /// Bonereaper v2 maximum late-favourite load clips.
        #[arg(long, default_value = "12")]
        br2_late_favourite_max_clips: usize,
        /// Bonereaper v2 minimum seconds favourite skew must persist before late-favourite loads.
        #[arg(long, default_value = "0.0")]
        br2_late_favourite_min_sustain_secs: f32,
        /// Bonereaper v2 book depth to sweep for late-favourite loads.
        #[arg(long, default_value = "7")]
        br2_late_favourite_sweep_depth: usize,
        /// Bonereaper v2 minimum ML confidence for late-favourite loads.
        #[arg(long, default_value = "0.68")]
        br2_late_favourite_min_model_confidence: f32,
        /// Bonereaper v2 minimum absolute model direction score for late-favourite loads.
        #[arg(long, default_value = "0.0")]
        br2_late_favourite_min_model_direction_abs: f32,
        /// Bonereaper v2 maximum ML risk for late-favourite loads.
        #[arg(long, default_value = "0.72")]
        br2_late_favourite_max_model_risk: f32,
        /// Bonereaper v2 minimum ML predicted-side probability for late-favourite loads.
        #[arg(long, default_value = "0.62")]
        br2_late_favourite_min_model_side_p: f32,
        /// Bonereaper v2 minimum ML probability edge over entry price for late-favourite loads.
        #[arg(long, default_value = "0.03")]
        br2_late_favourite_min_model_edge: f32,
        /// Bonereaper v2 minimum ML edge over entry price once ask is >= high-cert threshold.
        #[arg(long, default_value = "0.02")]
        br2_late_favourite_high_cert_min_model_edge: f32,
        /// Let high-cert favourite loads use par-discount logic instead of requiring calibrated_p >= entry price.
        #[arg(long, default_value_t = false)]
        br2_late_favourite_high_cert_bypass_model_edge: bool,
        /// Bonereaper v2 maximum continuous whipsaw score for late-favourite loads.
        #[arg(long, default_value = "0.75")]
        br2_late_favourite_max_whipsaw_score: f32,
        /// Bonereaper v2 maximum short-window reversal pressure for late-favourite loads.
        #[arg(long, default_value = "1.0")]
        br2_late_favourite_max_reversal_pressure: f32,
        /// Bonereaper v2 minimum spot path efficiency for late-favourite loads.
        #[arg(long, default_value = "0.0")]
        br2_late_favourite_min_path_efficiency: f32,
        /// Bonereaper v2 minimum BTC 180s realized volatility for late-favourite loads.
        #[arg(long, default_value = "0.0")]
        br2_late_favourite_min_realized_vol_180s_bps: f32,
        /// Bonereaper v2 maximum live-observed YES-mid range before late-favourite loads.
        #[arg(long, default_value = "1.0")]
        br2_late_favourite_max_observed_range: f32,
        /// Bonereaper v2 live-observed YES-mid range where late-favourite size starts throttling.
        #[arg(long, default_value = "0.78")]
        br2_late_favourite_range_soft_throttle: f32,
        /// Bonereaper v2 live-observed YES-mid range where late-favourite size reaches zero.
        #[arg(long, default_value = "0.98")]
        br2_late_favourite_range_hard_throttle: f32,
        /// Extra model edge required at the hard observed-range throttle.
        #[arg(long, default_value = "0.03")]
        br2_late_favourite_range_extra_edge: f32,
        /// Extra model confidence required at the hard observed-range throttle.
        #[arg(long, default_value = "0.08")]
        br2_late_favourite_range_extra_confidence: f32,
        /// Bonereaper v2 maximum fast BTC momentum allowed against the late favourite direction.
        #[arg(long, default_value = "1.0")]
        br2_late_favourite_max_adverse_fast_momentum: f32,
        /// Bonereaper v2 maximum broad BTC momentum allowed against the late favourite direction.
        #[arg(long, default_value = "1.0")]
        br2_late_favourite_max_adverse_broad_momentum: f32,
        /// Bonereaper v2 maximum same-side late-favourite entry pullback from best prior entry price.
        #[arg(long, default_value = "1.0")]
        br2_late_favourite_max_entry_pullback: f32,
        /// Bonereaper v2 maximum same-side late-favourite drawdown from average emitted entry price.
        #[arg(long, default_value = "1.0")]
        br2_late_favourite_max_avg_entry_drawdown: f32,
        /// Bonereaper v2 convex-tail clip multiplier.
        #[arg(long, default_value = "0.10")]
        br2_tail_clip_frac: f32,
        /// Bonereaper v2 maximum convex-tail clips.
        #[arg(long, default_value = "3")]
        br2_tail_max_clips: usize,
        /// Bonereaper v2 book depth to sweep for convex-tail entries.
        #[arg(long, default_value = "3")]
        br2_tail_sweep_depth: usize,
        /// Bonereaper v2 minimum convex-tail ask price.
        #[arg(long, default_value = "0.01")]
        br2_tail_min_ask: f32,
        /// Bonereaper v2 maximum convex-tail ask price.
        #[arg(long, default_value = "0.10")]
        br2_tail_max_ask: f32,
        /// Bonereaper v2 minimum seconds remaining before opening a convex-tail entry.
        #[arg(long, default_value = "10.0")]
        br2_tail_min_seconds_to_close: f32,
        /// Bonereaper v2 minimum favourite mark-to-market edge before buying convex-tail insurance.
        #[arg(long, default_value = "0.0")]
        br2_tail_min_favourite_unrealized_edge: f32,
        /// Bonereaper v2 minimum live-observed YES-mid range before convex-tail entries.
        #[arg(long, default_value = "0.0")]
        br2_tail_min_observed_range: f32,
        /// Bonereaper v2 tail target coverage of favourite loss, disabled at 0.
        #[arg(long, default_value = "0.50")]
        br2_tail_target_favourite_loss_coverage_frac: f32,
        /// Bonereaper v2 higher tail coverage target for high-cert favourite reversal windows.
        #[arg(long, default_value = "0.00")]
        br2_tail_reversal_coverage_frac: f32,
        /// Bonereaper v2 lower bound of seconds remaining for reversal tail boost.
        #[arg(long, default_value = "10.0")]
        br2_tail_reversal_min_seconds_to_close: f32,
        /// Bonereaper v2 upper bound of seconds remaining for reversal tail boost.
        #[arg(long, default_value = "35.0")]
        br2_tail_reversal_max_seconds_to_close: f32,
        /// Bonereaper v2 minimum favourite ask for reversal tail boost.
        #[arg(long, default_value = "0.895")]
        br2_tail_reversal_min_favourite_ask: f32,
        /// Bonereaper v2 minimum absolute skew from 0.5 before tail laddering.
        #[arg(long, default_value = "0.30")]
        br2_tail_extreme_threshold: f32,
        /// Bonereaper v2 minimum additional skew before another tail rung.
        #[arg(long, default_value = "0.02")]
        br2_tail_min_skew_step: f32,
        /// Bonereaper v2 tail budget cap as fraction of favourite spend.
        #[arg(long, default_value = "0.20")]
        br2_tail_budget_favourite_spend_frac: f32,
        /// Bonereaper v2 tail budget cap as fraction of favourite upside.
        #[arg(long, default_value = "0.25")]
        br2_tail_budget_favourite_upside_frac: f32,
        /// Bonereaper v2 boosted tail coverage target in choppy/reversal regimes.
        #[arg(long, default_value = "0.0")]
        br2_tail_regime_boost_coverage_frac: f32,
        /// Bonereaper v2 boosted tail spend cap as fraction of favourite spend.
        #[arg(long, default_value = "0.0")]
        br2_tail_regime_boost_budget_spend_frac: f32,
        /// Bonereaper v2 boosted tail spend cap as fraction of favourite upside.
        #[arg(long, default_value = "0.0")]
        br2_tail_regime_boost_budget_upside_frac: f32,
        /// Bonereaper v2 minimum whipsaw score for boosted tail coverage.
        #[arg(long, default_value = "1.0")]
        br2_tail_regime_boost_min_whipsaw_score: f32,
        /// Bonereaper v2 minimum reversal pressure for boosted tail coverage.
        #[arg(long, default_value = "1.0")]
        br2_tail_regime_boost_min_reversal_pressure: f32,
        /// Bonereaper v2 minimum 180s realized vol for boosted tail coverage.
        #[arg(long, default_value = "1000000000.0")]
        br2_tail_regime_boost_min_realized_vol_180s_bps: f32,
        /// Bonereaper v2 maximum path efficiency for boosted tail coverage.
        #[arg(long, default_value = "-1.0")]
        br2_tail_regime_boost_max_path_efficiency: f32,
        /// Enable the Phase-3 reversal-risk score modulators.
        #[arg(long, default_value_t = false)]
        br2_reversal_score_enabled: bool,
        /// Path to the Phase-2 reversal-score logistic coefficients JSON.
        #[arg(long)]
        br2_reversal_score_coeffs: Option<PathBuf>,
        /// Convex-tail coverage at reversal score 0 (NaN => use base coverage).
        #[arg(long, default_value = "nan")]
        br2_reversal_score_cov_min: f32,
        /// Convex-tail coverage at reversal score 1 (NaN => use base coverage).
        #[arg(long, default_value = "nan")]
        br2_reversal_score_cov_max: f32,
        /// Late-lane size multiplier at reversal score 1 (1.0 => no change).
        #[arg(long, default_value = "1.0")]
        br2_reversal_score_size_floor: f32,
        /// Late-lane size multiplier at reversal score 0 (1.0 => no change).
        #[arg(long, default_value = "1.0")]
        br2_reversal_score_size_ceiling: f32,
        /// Enable the runner-level model gate after strategy emission.
        #[arg(long, default_value_t = true)]
        enforce_model_gate: bool,
        /// Disable the runner-level model gate after strategy emission.
        #[arg(long, default_value_t = false)]
        disable_model_gate: bool,
        /// Runner-level model gate minimum confidence.
        #[arg(long, default_value = "0.68")]
        model_gate_min_confidence: f32,
        /// Runner-level model gate maximum risk.
        #[arg(long, default_value = "0.72")]
        model_gate_max_risk: f32,
        /// Runner-level model gate minimum explicit side edge.
        #[arg(long, default_value = "0.00")]
        model_gate_min_edge: f32,
        /// Canonical model risk weight for BTC spot whipsaw regimes.
        #[arg(long, default_value = "0.16")]
        model_btc_whipsaw_risk_weight: f32,
        /// Canonical model risk weight for BTC path inefficiency.
        #[arg(long, default_value = "0.10")]
        model_btc_path_inefficiency_risk_weight: f32,
        /// Canonical model risk weight for short-term BTC reversal pressure.
        #[arg(long, default_value = "0.12")]
        model_btc_reversal_pressure_risk_weight: f32,
        /// Add explicit asset/timeframe meta features for mixed BTC/ETH or 5m/15m experiments.
        #[arg(long, default_value_t = false)]
        enable_market_context_features: bool,
        /// Local directory mirroring the S3 prefix structure. When set, the
        /// loader reads parquets from local disk instead of S3 (use after
        /// `pm-app prep-cache`).
        #[arg(long)]
        local_cache_dir: Option<PathBuf>,
        /// Split markets into this many chronological forward-chaining folds.
        /// Mutually exclusive with `--fold-size`.
        #[arg(long)]
        walk_forward_folds: Option<usize>,
        /// Split markets into explicit fold windows of this size.
        /// Mutually exclusive with `--walk-forward-folds`.
        #[arg(long)]
        fold_size: Option<usize>,
        /// Purge this many markets around each train/test boundary.
        /// Placeholder for future walk-forward training leakage control.
        #[arg(long, default_value_t = 0)]
        purge_markets: usize,
        /// Do not evaluate fold windows until at least this many prior markets
        /// are available for meta-calibrator training.
        #[arg(long, default_value_t = 0)]
        min_train_markets: usize,
        /// Meta-calibrator training epochs for walk-forward/portfolio training.
        #[arg(long, default_value_t = 24)]
        meta_epochs: usize,
        /// Meta-calibrator learning rate.
        #[arg(long, default_value = "0.04")]
        meta_learning_rate: f32,
        /// Meta-calibrator L2 decay applied on each update.
        #[arg(long, default_value = "0.001")]
        meta_l2: f32,
        /// Absolute clip for meta-calibrator weights and bias.
        #[arg(long, default_value = "1.50")]
        meta_weight_clip: f32,
        /// Maximum market-balanced samples used to fit the meta-calibrator.
        #[arg(long, default_value_t = 120_000)]
        meta_max_fit_samples: usize,
        /// Maximum market-balanced samples used to validate the meta-calibrator.
        #[arg(long, default_value_t = 60_000)]
        meta_max_validation_samples: usize,
        /// Maximum meta-calibrator samples retained from any one market.
        #[arg(long, default_value_t = 64)]
        meta_max_samples_per_market: usize,
        /// Maximum market-balanced OOS samples used in summary diagnostics.
        #[arg(long, default_value_t = 120_000)]
        meta_max_oos_evaluation_samples: usize,
        /// Keep only meta-training samples with base predicted-side probability at least this high.
        #[arg(long, default_value = "0.0")]
        meta_train_min_base_p: f32,
        /// Keep only meta-training samples with early-market penalty at most this high.
        #[arg(long, default_value = "1.0")]
        meta_train_max_early_penalty: f32,
        /// Keep only meta-training samples with `2 * abs(mid - 0.5)` at least this high.
        #[arg(long, default_value = "0.0")]
        meta_train_min_mid_distance: f32,
        /// JSON cache for extracted meta-calibrator training samples.
        #[arg(long)]
        meta_training_samples_cache: Option<PathBuf>,
        /// Load a frozen meta-calibrator snapshot instead of training one.
        #[arg(long)]
        meta_calibrator_snapshot_in: Option<PathBuf>,
        /// Write the trained meta-calibrator snapshot to this path.
        #[arg(long)]
        meta_calibrator_snapshot_out: Option<PathBuf>,
        /// Fail instead of fitting a meta-calibrator when no snapshot is loaded.
        #[arg(long, default_value_t = false)]
        forbid_meta_training: bool,
        /// Disable the ML/meta-calibrator probability adjustment while keeping
        /// the hand-crafted model and strategy gates active.
        #[arg(long, default_value_t = false)]
        disable_meta_calibration: bool,
        /// In portfolio mode, write partial outputs every N evaluated markets.
        /// Set to zero to disable.
        #[arg(long, default_value = "0")]
        portfolio_checkpoint_every_markets: usize,
        /// Portfolio-mode per-decision attribution rows (JSONL). Useful for
        /// offline model research; disabled for parallel non-portfolio runs.
        #[arg(long)]
        decision_log: Option<PathBuf>,
        /// Only log every Nth decision event when writing `--decision-log`.
        #[arg(long, default_value = "1")]
        decision_log_every_n: usize,
        /// Per-market JSONL output.
        #[arg(long)]
        out_markets: Option<PathBuf>,
        /// Summary JSON output.
        #[arg(long)]
        out_summary: Option<PathBuf>,
    },
    /// Summarize a walk-forward `markets.jsonl` result file.
    SummarizeMarkets {
        /// Path to the per-market JSONL emitted by `walk-forward --out-markets`.
        #[arg(long)]
        markets: PathBuf,
        /// Strategy key inside `per_strategy`.
        #[arg(long, default_value = "bonereaper_v2")]
        strategy: String,
        /// Optional JSON output path for the computed summary.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Run a paper-mode replay on one market (historical tape + same execution stack as backtest).
    Paper {
        #[arg(long, default_value = "polymarket")]
        exchange: String,
        #[arg(long, default_value = "book_snapshot_25")]
        channel: String,
        #[arg(long)]
        date: String,
        #[arg(long)]
        asset_id: String,
        /// Slug of the market (e.g. btc-updown-5m-1778587500). Used to parse
        /// the resolution timestamp when --close-ts is omitted.
        #[arg(long)]
        slug: Option<String>,
        /// Resolution Unix epoch seconds. Overrides --slug parsing.
        #[arg(long)]
        close_ts: Option<i64>,
        /// Force the resolution outcome (true = YES won). When omitted, infer
        /// from final yes_mid >= 0.5.
        #[arg(long)]
        resolved_yes: Option<bool>,
        #[arg(long, default_value = "1")]
        market_id: u32,
        #[arg(long, value_enum, default_value = "bonereaper-v2")]
        strategy: StrategyKind,
        #[arg(long, default_value = "100.0")]
        starting_cash: f64,
        #[arg(long, default_value = "5.0")]
        max_clip_usdc: f64,
        #[arg(long, default_value = "0.30")]
        max_drawdown_pct: f64,
        #[arg(long, default_value = "250.0")]
        max_daily_exposure_usdc: f64,
        /// Binance spot symbol to load for momentum + regime signals (e.g.
        /// BTCUSDT). Set to empty string to disable spot.
        #[arg(long, default_value = "BTCUSDT")]
        spot_symbol: String,
        /// If set, write the JSON report to this path.
        #[arg(long)]
        out: Option<PathBuf>,
        /// If set, write per-snapshot portfolio rows (JSONL) to this path.
        #[arg(long)]
        equity_curve: Option<PathBuf>,
        /// Write per-decision attribution rows (JSONL) to this path.
        #[arg(long)]
        decision_log: Option<PathBuf>,
        /// Only log every Nth decision event when writing `--decision-log`.
        #[arg(long, default_value = "1")]
        decision_log_every_n: usize,
        /// Read data from a local cache mirror instead of S3.
        #[arg(long)]
        local_cache_dir: Option<PathBuf>,
    },
    /// Run a live-mode replay on one market (historical tape + same execution stack for parity scaffolding).
    Live {
        #[arg(long, default_value = "polymarket")]
        exchange: String,
        #[arg(long, default_value = "book_snapshot_25")]
        channel: String,
        #[arg(long)]
        date: String,
        #[arg(long)]
        asset_id: String,
        /// Slug of the market (e.g. btc-updown-5m-1778587500). Used to parse
        /// the resolution timestamp when --close-ts is omitted.
        #[arg(long)]
        slug: Option<String>,
        /// Resolution Unix epoch seconds. Overrides --slug parsing.
        #[arg(long)]
        close_ts: Option<i64>,
        /// Force the resolution outcome (true = YES won). When omitted, infer
        /// from final yes_mid >= 0.5.
        #[arg(long)]
        resolved_yes: Option<bool>,
        #[arg(long, default_value = "1")]
        market_id: u32,
        #[arg(long, value_enum, default_value = "bonereaper-v2")]
        strategy: StrategyKind,
        #[arg(long, default_value = "100.0")]
        starting_cash: f64,
        #[arg(long, default_value = "5.0")]
        max_clip_usdc: f64,
        #[arg(long, default_value = "0.30")]
        max_drawdown_pct: f64,
        #[arg(long, default_value = "250.0")]
        max_daily_exposure_usdc: f64,
        /// Binance spot symbol to load for momentum + regime signals (e.g.
        /// BTCUSDT). Set to empty string to disable spot.
        #[arg(long, default_value = "BTCUSDT")]
        spot_symbol: String,
        /// If set, write the JSON report to this path.
        #[arg(long)]
        out: Option<PathBuf>,
        /// If set, write per-snapshot portfolio rows (JSONL) to this path.
        #[arg(long)]
        equity_curve: Option<PathBuf>,
        /// Write per-decision attribution rows (JSONL) to this path.
        #[arg(long)]
        decision_log: Option<PathBuf>,
        /// Only log every Nth decision event when writing `--decision-log`.
        #[arg(long, default_value = "1")]
        decision_log_every_n: usize,
        /// Read data from a local cache mirror instead of S3.
        #[arg(long)]
        local_cache_dir: Option<PathBuf>,
    },
    /// Backtest copying a specific wallet's trades under modelled latency.
    CopyTrade(CopyTradeArgs),
}

#[derive(clap::Args, Debug)]
pub struct CopyTradeArgs {
    #[arg(long)] pub wallet: String,
    #[arg(long, default_value_t = 0)] pub start_ts: i64,
    #[arg(long, default_value_t = 0)] pub end_ts: i64,
    #[arg(long, value_delimiter = ',', default_value = "0,2,5,15")] pub latency_s: Vec<f64>,
    #[arg(long, default_value_t = 100.0)] pub our_bankroll: f64,
    #[arg(long, default_value_t = 5.0)] pub max_clip_usdc: f64,
    #[arg(long, default_value_t = 1000.0)] pub leader_seed_usdc: f64,
    #[arg(long, default_value_t = 16)] pub concurrency: usize,
    #[arg(long, default_value = "https://data-api.polymarket.com")] pub data_base: String,
    #[arg(long, default_value = "https://clob.polymarket.com")] pub clob_base: String,
    #[arg(long, default_value = "https://gamma-api.polymarket.com")] pub gamma_base: String,
    #[arg(long, default_value = "/tmp/copytrade-ledger")] pub out_prefix: String,
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Alpha {
            markets,
            slug_prefix,
            date_start,
            date_end,
            max_markets,
            local_cache_dir,
            replay_event_cache_dir,
            latency_ms,
            latency_sweep,
            edge_thresholds,
            fee_bps,
            notional_usdc,
            decision_dt_ms,
            stop_before_close_s,
            max_clips,
            clip_cooldown_ms,
            exit_after_s,
            skip_calm,
            infer_outcome,
            vol_lookback_s,
            momentum_lookback_s,
            momentum_weight,
            out_json,
            calibrate_split,
            calibrator_out,
            calibrator_in,
            trades_out,
            down_assets,
            tick_cache_dir,
        } => {
            let store = if let Some(ref dir) = local_cache_dir {
                tracing::info!(?dir, "using local cache");
                TelonexStore::try_new_local(dir.clone())?
            } else {
                let cfg = TelonexStoreConfig::from_env()?;
                TelonexStore::try_new(&cfg)?
            };
            let latencies_ms = if latency_sweep {
                vec![0, 50, 150, 300, 500, 1000]
            } else {
                vec![latency_ms]
            };
            alpha::run_alpha(
                &store,
                alpha::AlphaArgs {
                    markets_path: markets,
                    slug_prefix,
                    date_start,
                    date_end,
                    max_markets,
                    replay_event_cache_dir,
                    latencies_ms,
                    edge_thresholds,
                    fee_bps,
                    notional_usdc,
                    decision_dt_ms,
                    stop_before_close_s,
                    max_clips,
                    clip_cooldown_ms,
                    exit_after_s,
                    skip_calm,
                    infer_outcome,
                    vol_lookback_s,
                    momentum_lookback_s,
                    momentum_weight,
                    out_json,
                    calibrate_split,
                    calibrator_out,
                    calibrator_in,
                    trades_out,
                    down_assets,
                    tick_cache_dir,
                },
            )
            .await
        }
        Cmd::InspectS3 {
            exchange,
            channel,
            date,
            asset_id,
            market_id,
            head,
            local_cache_dir,
        } => {
            let channel: Channel = channel
                .parse()
                .map_err(|e: String| anyhow!("bad --channel: {e}"))?;
            inspect_s3(
                exchange,
                channel,
                date,
                asset_id,
                MarketId(market_id),
                head,
                local_cache_dir,
            )
            .await
        }
        Cmd::BacktestS3 {
            exchange,
            channel,
            date,
            asset_id,
            slug,
            close_ts,
            resolved_yes,
            market_id,
            strategy,
            starting_cash,
            max_clip_usdc,
            max_drawdown_pct,
            max_daily_exposure_usdc,
            spot_symbol,
            out,
            equity_curve,
            decision_log,
            decision_log_every_n,
            local_cache_dir,
        } => {
            let channel: Channel = channel
                .parse()
                .map_err(|e: String| anyhow!("bad --channel: {e}"))?;
            let close_ts_s = match (close_ts, slug.as_deref()) {
                (Some(ts), _) => ts,
                (None, Some(s)) => parse_close_ts_from_slug(s)?,
                (None, None) => {
                    return Err(anyhow!(
                        "need either --close-ts or --slug to determine market resolution time"
                    ));
                }
            };
            let limits = PortfolioLimits {
                max_drawdown_pct,
                max_daily_exposure_usdc,
                max_clip_usdc,
                max_per_market_exposure_usdc: 15.0,
            };
            backtest_s3(
                exchange,
                channel,
                date,
                asset_id,
                MarketId(market_id),
                strategy,
                starting_cash,
                limits,
                close_ts_s,
                resolved_yes,
                spot_symbol,
                out,
                equity_curve,
                decision_log,
                decision_log_every_n,
                local_cache_dir,
            )
            .await
        }
        Cmd::QuotesS3 {
            exchange,
            channel,
            date,
            asset_id,
            slug,
            market_id,
            head,
            local_cache_dir,
        } => {
            let channel: Channel = channel
                .parse()
                .map_err(|e: String| anyhow!("bad --channel: {e}"))?;
            quotes_s3(
                exchange,
                channel,
                date,
                asset_id,
                slug,
                MarketId(market_id),
                head,
                local_cache_dir,
            )
            .await
        }
        Cmd::DiscoverDay {
            date,
            slug_prefix,
            max_concurrent,
            availability_cache,
            out,
        } => discover_day(date, slug_prefix, max_concurrent, availability_cache, out).await,
        Cmd::DiscoverLocalCacheDay {
            cache_dir,
            date,
            slug_prefix,
            max_concurrent,
            max_assets,
            availability_cache,
            out,
        } => {
            discover_local_cache_day(
                cache_dir,
                date,
                slug_prefix,
                max_concurrent,
                max_assets,
                availability_cache,
                out,
            )
            .await
        }
        Cmd::DiscoverLocalCacheBookMetadata {
            cache_dir,
            date,
            slug_prefix,
            token_outcome,
            out,
        } => discover_local_cache_book_metadata(cache_dir, date, slug_prefix, token_outcome, out),
        Cmd::DiscoverRange {
            start_date,
            end_date,
            slug_prefix,
            max_concurrent,
            availability_cache,
            out,
        } => {
            discover_range(
                start_date,
                end_date,
                slug_prefix,
                max_concurrent,
                availability_cache,
                out,
            )
            .await
        }
        Cmd::DiscoverMarketsParquet {
            markets_parquet,
            start_date,
            end_date,
            slug_prefix,
            require_book_s3,
            out,
        } => {
            discover_markets_parquet(
                markets_parquet,
                start_date,
                end_date,
                slug_prefix,
                require_book_s3,
                out,
            )
            .await
        }
        Cmd::PrepCache {
            markets,
            cache_dir,
            spot_symbol,
            max_concurrent,
            skip_existing,
        } => {
            prep_cache_cmd(
                markets,
                cache_dir,
                spot_symbol,
                max_concurrent,
                skip_existing,
            )
            .await
        }
        Cmd::EngineBacktest {
            local_cache_dir,
            start_date,
            end_date,
            slug_prefix,
            spot_symbol,
            meta_calibrator_snapshot_in,
            starting_cash,
            max_clip_usdc,
            taker_latency_ms,
            taker_fee_bps,
            maker_rebate_bps,
            replay_sample_ms,
            max_markets,
            strategy,
            signal_min_edge,
            signal_min_confidence,
            signal_max_risk,
            favourite_start_secs,
            favourite_stop_secs_before_close,
            tail_balance_frac,
        } => {
            let strategy_kind = match strategy.as_str() {
                "bonereaper_v2" | "br2" => engine_driver::StrategyKind::BonereaperV2,
                "convex" => engine_driver::StrategyKind::Convex,
                other => anyhow::bail!("unknown --strategy {other} (use bonereaper_v2|convex)"),
            };
            let report = engine_driver::run_engine_backtest(engine_driver::EngineBacktestCfg {
                cache_dir: local_cache_dir,
                start_date,
                end_date,
                slug_prefix,
                spot_symbol,
                snapshot_path: meta_calibrator_snapshot_in,
                starting_cash,
                max_clip_usdc,
                taker_latency_ms,
                taker_fee_bps,
                maker_rebate_bps,
                replay_sample_ms,
                max_markets,
                strategy: strategy_kind,
                signal_min_edge,
                signal_min_confidence,
                signal_max_risk,
                favourite_start_secs,
                favourite_stop_secs_before_close,
                tail_balance_frac,
            })
            .await?;
            let pnl = report.final_equity_usd - report.starting_cash_usd;
            let pct = if report.starting_cash_usd != 0.0 {
                (report.final_equity_usd / report.starting_cash_usd - 1.0) * 100.0
            } else {
                0.0
            };
            println!(
                "engine backtest: markets_total={} markets_traded={} orders={} fills={} \
                 start_cash=${:.2} final_equity=${:.2} pnl=${:.2} ({pct:+.2}%)",
                report.markets_total,
                report.markets_traded,
                report.orders_submitted,
                report.fills,
                report.starting_cash_usd,
                report.final_equity_usd,
                pnl,
            );
            Ok(())
        }
        Cmd::WalkForward {
            markets,
            profile,
            back_to_explore_policy_scales_jsonl,
            skip_markets,
            max_markets,
            starting_cash,
            kelly_fraction,
            max_clip_usdc,
            max_order_clip_multiplier,
            max_per_market_exposure_usdc,
            max_per_market_exposure_frac,
            spot_symbol,
            strategies,
            allow_legacy_strategies,
            max_concurrent_fetches,
            replay_sample_ms,
            taker_latency_ms,
            replay_event_cache_dir,
            disable_pm_trades,
            use_outcome_label,
            portfolio_mode,
            volatility_regime_threshold,
            clip_fraction_of_equity,
            clip_drawdown_soft_pct,
            clip_drawdown_hard_pct,
            clip_drawdown_min_multiplier,
            clip_session_drawdown_soft_pct,
            clip_session_drawdown_hard_pct,
            clip_session_drawdown_min_multiplier,
            daily_loss_cap_pct,
            br2_disable_internal_model_gates,
            br2_participation_clip_frac,
            br2_participation_max_pair_cost,
            br2_participation_max_orders_per_leg,
            br2_participation_max_inventory_delta_shares,
            br2_participation_repair_inventory_delta_shares,
            br2_participation_refresh_secs,
            br2_participation_stop_secs_before_close,
            br2_hedged_base_enabled,
            br2_hedged_base_max_secs_in,
            br2_hedged_base_max_pair_cost,
            br2_hedged_base_min_minority_leg_frac,
            br2_hedged_base_clip_usdc,
            br2_hedged_base_max_notional_usdc,
            br2_late_directional_overlay_frac,
            br2_min_composite_direction,
            br2_early_clip_frac,
            br2_mid_clip_frac,
            br2_late_clip_frac,
            br2_late_max_fires,
            br2_late_confirm_min_model_confidence,
            br2_late_confirm_max_model_risk,
            br2_late_confirm_min_model_side_p,
            br2_late_confirm_min_model_edge,
            br2_late_confirm_min_book_skew,
            br2_late_confirm_max_whipsaw_score,
            br2_late_confirm_min_realized_vol_180s_bps,
            br2_late_confirm_max_observed_range,
            br2_recent_regime_gate_enabled,
            br2_recent_regime_gate_min_edge,
            br2_recent_regime_gate_late_confirm,
            br2_recent_regime_gate_high_skew,
            br2_recent_regime_gate_late_favourite,
            br2_high_skew_clip_frac,
            br2_lane_size_late_favourite,
            br2_lane_size_late_confirm,
            br2_lane_size_high_skew,
            br2_regime_gate_enabled,
            br2_regime_gate_window,
            br2_regime_gate_threshold,
            br2_regime_gate_soft_band,
            br2_regime_gate_lane_floor,
            br2_regime_gate_whipsaw_weight,
            br2_high_skew_max_clips,
            br2_high_skew_max_whipsaw_score,
            br2_high_skew_min_realized_vol_180s_bps,
            br2_late_favourite_start_secs,
            br2_late_favourite_threshold,
            br2_late_favourite_min_ask,
            br2_late_favourite_max_ask,
            br2_late_favourite_clip_frac,
            br2_late_favourite_high_cert_clip_frac,
            br2_late_favourite_high_cert_full_clip_edge,
            br2_late_favourite_fragile_high_cert_ask,
            br2_late_favourite_fragile_high_cert_max_edge,
            br2_late_favourite_fragile_high_cert_max_path_efficiency,
            br2_late_favourite_fragile_high_cert_size_frac,
            br2_late_favourite_max_clips,
            br2_late_favourite_min_sustain_secs,
            br2_late_favourite_sweep_depth,
            br2_late_favourite_min_model_confidence,
            br2_late_favourite_min_model_direction_abs,
            br2_late_favourite_max_model_risk,
            br2_late_favourite_min_model_side_p,
            br2_late_favourite_min_model_edge,
            br2_late_favourite_high_cert_min_model_edge,
            br2_late_favourite_high_cert_bypass_model_edge,
            br2_late_favourite_max_whipsaw_score,
            br2_late_favourite_max_reversal_pressure,
            br2_late_favourite_min_path_efficiency,
            br2_late_favourite_min_realized_vol_180s_bps,
            br2_late_favourite_max_observed_range,
            br2_late_favourite_range_soft_throttle,
            br2_late_favourite_range_hard_throttle,
            br2_late_favourite_range_extra_edge,
            br2_late_favourite_range_extra_confidence,
            br2_late_favourite_max_adverse_fast_momentum,
            br2_late_favourite_max_adverse_broad_momentum,
            br2_late_favourite_max_entry_pullback,
            br2_late_favourite_max_avg_entry_drawdown,
            br2_tail_clip_frac,
            br2_tail_max_clips,
            br2_tail_sweep_depth,
            br2_tail_min_ask,
            br2_tail_max_ask,
            br2_tail_min_seconds_to_close,
            br2_tail_min_favourite_unrealized_edge,
            br2_tail_min_observed_range,
            br2_tail_target_favourite_loss_coverage_frac,
            br2_tail_reversal_coverage_frac,
            br2_tail_reversal_min_seconds_to_close,
            br2_tail_reversal_max_seconds_to_close,
            br2_tail_reversal_min_favourite_ask,
            br2_tail_extreme_threshold,
            br2_tail_min_skew_step,
            br2_tail_budget_favourite_spend_frac,
            br2_tail_budget_favourite_upside_frac,
            br2_tail_regime_boost_coverage_frac,
            br2_tail_regime_boost_budget_spend_frac,
            br2_tail_regime_boost_budget_upside_frac,
            br2_tail_regime_boost_min_whipsaw_score,
            br2_tail_regime_boost_min_reversal_pressure,
            br2_tail_regime_boost_min_realized_vol_180s_bps,
            br2_tail_regime_boost_max_path_efficiency,
            br2_reversal_score_enabled,
            br2_reversal_score_coeffs,
            br2_reversal_score_cov_min,
            br2_reversal_score_cov_max,
            br2_reversal_score_size_floor,
            br2_reversal_score_size_ceiling,
            enforce_model_gate,
            disable_model_gate,
            model_gate_min_confidence,
            model_gate_max_risk,
            model_gate_min_edge,
            model_btc_whipsaw_risk_weight,
            model_btc_path_inefficiency_risk_weight,
            model_btc_reversal_pressure_risk_weight,
            enable_market_context_features,
            walk_forward_folds,
            fold_size,
            purge_markets,
            min_train_markets,
            meta_epochs,
            meta_learning_rate,
            meta_l2,
            meta_weight_clip,
            meta_max_fit_samples,
            meta_max_validation_samples,
            meta_max_samples_per_market,
            meta_max_oos_evaluation_samples,
            meta_train_min_base_p,
            meta_train_max_early_penalty,
            meta_train_min_mid_distance,
            meta_training_samples_cache,
            meta_calibrator_snapshot_in,
            meta_calibrator_snapshot_out,
            forbid_meta_training,
            disable_meta_calibration,
            portfolio_checkpoint_every_markets,
            decision_log,
            decision_log_every_n,
            local_cache_dir,
            out_markets,
            out_summary,
        } => {
            walk_forward(
                markets,
                profile,
                back_to_explore_policy_scales_jsonl,
                skip_markets,
                max_markets,
                starting_cash,
                kelly_fraction,
                max_clip_usdc,
                max_order_clip_multiplier,
                max_per_market_exposure_usdc,
                max_per_market_exposure_frac,
                spot_symbol,
                strategies,
                allow_legacy_strategies,
                max_concurrent_fetches,
                replay_sample_ms,
                taker_latency_ms,
                replay_event_cache_dir,
                !disable_pm_trades,
                use_outcome_label,
                portfolio_mode,
                volatility_regime_threshold,
                clip_fraction_of_equity,
                clip_drawdown_soft_pct,
                clip_drawdown_hard_pct,
                clip_drawdown_min_multiplier,
                clip_session_drawdown_soft_pct,
                clip_session_drawdown_hard_pct,
                clip_session_drawdown_min_multiplier,
                daily_loss_cap_pct,
                br2_disable_internal_model_gates,
                br2_participation_clip_frac,
                br2_participation_max_pair_cost,
                br2_participation_max_orders_per_leg,
                br2_participation_max_inventory_delta_shares,
                br2_participation_repair_inventory_delta_shares,
                br2_participation_refresh_secs,
                br2_participation_stop_secs_before_close,
                br2_hedged_base_enabled,
                br2_hedged_base_max_secs_in,
                br2_hedged_base_max_pair_cost,
                br2_hedged_base_min_minority_leg_frac,
                br2_hedged_base_clip_usdc,
                br2_hedged_base_max_notional_usdc,
                br2_late_directional_overlay_frac,
                br2_min_composite_direction,
                br2_early_clip_frac,
                br2_mid_clip_frac,
                br2_late_clip_frac,
                br2_late_max_fires,
                br2_late_confirm_min_model_confidence,
                br2_late_confirm_max_model_risk,
                br2_late_confirm_min_model_side_p,
                br2_late_confirm_min_model_edge,
                br2_late_confirm_min_book_skew,
                br2_late_confirm_max_whipsaw_score,
                br2_late_confirm_min_realized_vol_180s_bps,
                br2_late_confirm_max_observed_range,
                br2_recent_regime_gate_enabled,
                br2_recent_regime_gate_min_edge,
                br2_recent_regime_gate_late_confirm,
                br2_recent_regime_gate_high_skew,
                br2_recent_regime_gate_late_favourite,
                br2_high_skew_clip_frac,
                br2_lane_size_late_favourite,
                br2_lane_size_late_confirm,
                br2_lane_size_high_skew,
                br2_regime_gate_enabled,
                br2_regime_gate_window,
                br2_regime_gate_threshold,
                br2_regime_gate_soft_band,
                br2_regime_gate_lane_floor,
                br2_regime_gate_whipsaw_weight,
                br2_high_skew_max_clips,
                br2_high_skew_max_whipsaw_score,
                br2_high_skew_min_realized_vol_180s_bps,
                br2_late_favourite_start_secs,
                br2_late_favourite_threshold,
                br2_late_favourite_min_ask,
                br2_late_favourite_max_ask,
                br2_late_favourite_clip_frac,
                br2_late_favourite_high_cert_clip_frac,
                br2_late_favourite_high_cert_full_clip_edge,
                br2_late_favourite_fragile_high_cert_ask,
                br2_late_favourite_fragile_high_cert_max_edge,
                br2_late_favourite_fragile_high_cert_max_path_efficiency,
                br2_late_favourite_fragile_high_cert_size_frac,
                br2_late_favourite_max_clips,
                br2_late_favourite_min_sustain_secs,
                br2_late_favourite_sweep_depth,
                br2_late_favourite_min_model_confidence,
                br2_late_favourite_min_model_direction_abs,
                br2_late_favourite_max_model_risk,
                br2_late_favourite_min_model_side_p,
                br2_late_favourite_min_model_edge,
                br2_late_favourite_high_cert_min_model_edge,
                br2_late_favourite_high_cert_bypass_model_edge,
                br2_late_favourite_max_whipsaw_score,
                br2_late_favourite_max_reversal_pressure,
                br2_late_favourite_min_path_efficiency,
                br2_late_favourite_min_realized_vol_180s_bps,
                br2_late_favourite_max_observed_range,
                br2_late_favourite_range_soft_throttle,
                br2_late_favourite_range_hard_throttle,
                br2_late_favourite_range_extra_edge,
                br2_late_favourite_range_extra_confidence,
                br2_late_favourite_max_adverse_fast_momentum,
                br2_late_favourite_max_adverse_broad_momentum,
                br2_late_favourite_max_entry_pullback,
                br2_late_favourite_max_avg_entry_drawdown,
                br2_tail_clip_frac,
                br2_tail_max_clips,
                br2_tail_sweep_depth,
                br2_tail_min_ask,
                br2_tail_max_ask,
                br2_tail_min_seconds_to_close,
                br2_tail_min_favourite_unrealized_edge,
                br2_tail_min_observed_range,
                br2_tail_target_favourite_loss_coverage_frac,
                br2_tail_reversal_coverage_frac,
                br2_tail_reversal_min_seconds_to_close,
                br2_tail_reversal_max_seconds_to_close,
                br2_tail_reversal_min_favourite_ask,
                br2_tail_extreme_threshold,
                br2_tail_min_skew_step,
                br2_tail_budget_favourite_spend_frac,
                br2_tail_budget_favourite_upside_frac,
                br2_tail_regime_boost_coverage_frac,
                br2_tail_regime_boost_budget_spend_frac,
                br2_tail_regime_boost_budget_upside_frac,
                br2_tail_regime_boost_min_whipsaw_score,
                br2_tail_regime_boost_min_reversal_pressure,
                br2_tail_regime_boost_min_realized_vol_180s_bps,
                br2_tail_regime_boost_max_path_efficiency,
                br2_reversal_score_enabled,
                br2_reversal_score_coeffs,
                br2_reversal_score_cov_min,
                br2_reversal_score_cov_max,
                br2_reversal_score_size_floor,
                br2_reversal_score_size_ceiling,
                enforce_model_gate && !disable_model_gate,
                model_gate_min_confidence,
                model_gate_max_risk,
                model_gate_min_edge,
                model_btc_whipsaw_risk_weight,
                model_btc_path_inefficiency_risk_weight,
                model_btc_reversal_pressure_risk_weight,
                enable_market_context_features,
                walk_forward_folds,
                fold_size,
                purge_markets,
                min_train_markets,
                meta_epochs,
                meta_learning_rate,
                meta_l2,
                meta_weight_clip,
                meta_max_fit_samples,
                meta_max_validation_samples,
                meta_max_samples_per_market,
                meta_max_oos_evaluation_samples,
                meta_train_min_base_p,
                meta_train_max_early_penalty,
                meta_train_min_mid_distance,
                meta_training_samples_cache,
                meta_calibrator_snapshot_in,
                meta_calibrator_snapshot_out,
                forbid_meta_training,
                disable_meta_calibration,
                portfolio_checkpoint_every_markets,
                decision_log,
                decision_log_every_n,
                local_cache_dir,
                out_markets,
                out_summary,
            )
            .await
        }
        Cmd::SummarizeMarkets {
            markets,
            strategy,
            out,
        } => {
            let summary = summarize_markets_jsonl(&markets, &strategy)?;
            print_result_summary(&summary);
            if let Some(path) = out {
                write_result_summary_json(&path, &summary)?;
                tracing::info!(?path, "wrote result summary");
            }
            Ok(())
        }
        Cmd::Paper {
            exchange,
            channel,
            date,
            asset_id,
            slug,
            close_ts,
            resolved_yes,
            market_id,
            strategy,
            starting_cash,
            max_clip_usdc,
            max_drawdown_pct,
            max_daily_exposure_usdc,
            spot_symbol,
            out,
            equity_curve,
            decision_log,
            decision_log_every_n,
            local_cache_dir,
        } => {
            let channel: Channel = channel
                .parse()
                .map_err(|e: String| anyhow!("bad --channel: {e}"))?;
            let close_ts_s = match (close_ts, slug.as_deref()) {
                (Some(ts), _) => ts,
                (None, Some(s)) => parse_close_ts_from_slug(s)?,
                (None, None) => {
                    return Err(anyhow!(
                        "need either --close-ts or --slug to determine market resolution time"
                    ));
                }
            };
            let limits = PortfolioLimits {
                max_drawdown_pct,
                max_daily_exposure_usdc,
                max_clip_usdc,
                max_per_market_exposure_usdc: 15.0,
            };
            run_market_backtest(
                exchange,
                channel,
                date,
                asset_id,
                MarketId(market_id),
                strategy,
                starting_cash,
                limits,
                close_ts_s,
                resolved_yes,
                spot_symbol,
                out,
                equity_curve,
                decision_log,
                decision_log_every_n,
                local_cache_dir,
                MarketRunMode::Paper,
            )
            .await
        }
        Cmd::Live {
            exchange,
            channel,
            date,
            asset_id,
            slug,
            close_ts,
            resolved_yes,
            market_id,
            strategy,
            starting_cash,
            max_clip_usdc,
            max_drawdown_pct,
            max_daily_exposure_usdc,
            spot_symbol,
            out,
            equity_curve,
            decision_log,
            decision_log_every_n,
            local_cache_dir,
        } => {
            let channel: Channel = channel
                .parse()
                .map_err(|e: String| anyhow!("bad --channel: {e}"))?;
            let close_ts_s = match (close_ts, slug.as_deref()) {
                (Some(ts), _) => ts,
                (None, Some(s)) => parse_close_ts_from_slug(s)?,
                (None, None) => {
                    return Err(anyhow!(
                        "need either --close-ts or --slug to determine market resolution time"
                    ));
                }
            };
            let limits = PortfolioLimits {
                max_drawdown_pct,
                max_daily_exposure_usdc,
                max_clip_usdc,
                max_per_market_exposure_usdc: 15.0,
            };
            run_market_backtest(
                exchange,
                channel,
                date,
                asset_id,
                MarketId(market_id),
                strategy,
                starting_cash,
                limits,
                close_ts_s,
                resolved_yes,
                spot_symbol,
                out,
                equity_curve,
                decision_log,
                decision_log_every_n,
                local_cache_dir,
                MarketRunMode::Live,
            )
            .await
        }
        Cmd::CopyTrade(a) => {
            use pm_copytrade::sources::activity::HttpFillSource;
            use pm_copytrade::sources::prices::HttpPriceSource;
            use pm_copytrade::sources::resolution::HttpResolutionSource;
            let client = reqwest::Client::builder()
                .user_agent("pm-copytrade/0.1")
                .build()?;
            let end_ts = if a.end_ts == 0 { chrono::Utc::now().timestamp() } else { a.end_ts };
            let start_ts = if a.start_ts == 0 { end_ts - 60 * 24 * 3600 } else { a.start_ts };
            let fills_src = HttpFillSource { client: client.clone(), base: a.data_base.clone(), bucket_seconds: 3600 };
            let price_src = HttpPriceSource::new(client.clone(), a.data_base.clone(), a.clob_base.clone(), 90);
            let res_src = HttpResolutionSource::new(client.clone(), a.gamma_base.clone());
            let cfg = pm_copytrade::RunConfig {
                wallet: a.wallet.clone(), start_ts, end_ts, latencies_s: a.latency_s.clone(),
                our_bankroll: a.our_bankroll, max_clip_usdc: a.max_clip_usdc,
                leader_seed_usdc: a.leader_seed_usdc, concurrency: a.concurrency,
            };
            let (report, ledgers) = pm_copytrade::run_historical(&fills_src, &price_src, &res_src, &cfg).await?;
            println!("fills: {} total, {} in common set across all latencies", report.fills, report.common_fills);
            for run in &report.runs {
                let p = std::path::PathBuf::from(format!("{}-L{}.jsonl", a.out_prefix, run.latency_s));
                pm_copytrade::summary::write_ledger_jsonl(&p, &ledgers[&((run.latency_s * 1000.0).round() as i64)])?;
                let pf_str: String = run.priced_from.iter()
                    .map(|(k, v)| format!("{k}:{v}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("latency {:>4}s | trades {:>5} | win {:>5.1}% | ROI {:>6.2}% | maxDD {:>5.1}% | endEq {:.2} | open {} | priced[{}]",
                    run.latency_s, run.summary.trades, run.summary.win_rate * 100.0,
                    run.summary.roi * 100.0, run.summary.max_drawdown * 100.0, run.final_equity, run.open_unresolved, pf_str);
            }
            pm_copytrade::summary::write_summary_json(&std::path::PathBuf::from(format!("{}-summary.json", a.out_prefix)), &report)?;
            Ok(())
        }
    }
}

async fn discover_day(
    date: String,
    slug_prefix: String,
    max_concurrent: usize,
    availability_cache: Option<PathBuf>,
    out: PathBuf,
) -> Result<()> {
    let cfg = TelonexStoreConfig::from_env()?;
    let store = TelonexStore::try_new(&cfg)?;
    let markets = discovery::discover_markets(
        &store,
        &date,
        &slug_prefix,
        max_concurrent,
        availability_cache.as_deref(),
    )
    .await?;
    let mut f = std::fs::File::create(&out)?;
    for m in &markets {
        writeln!(f, "{}", serde_json::to_string(m)?)?;
    }
    tracing::info!(
        date = %date,
        markets = markets.len(),
        out = ?out,
        "discovery complete"
    );
    println!(
        "discovered {} markets for {} -> {}",
        markets.len(),
        date,
        out.display()
    );
    Ok(())
}

async fn discover_local_cache_day(
    cache_dir: PathBuf,
    date: String,
    slug_prefix: String,
    max_concurrent: usize,
    max_assets: usize,
    availability_cache: Option<PathBuf>,
    out: PathBuf,
) -> Result<()> {
    let markets = discovery::discover_markets_from_local_cache(
        &cache_dir,
        &date,
        &slug_prefix,
        max_concurrent,
        max_assets,
        availability_cache.as_deref(),
    )
    .await?;
    let mut f = std::fs::File::create(&out)?;
    for m in &markets {
        writeln!(f, "{}", serde_json::to_string(m)?)?;
    }
    tracing::info!(
        date = %date,
        cache_dir = ?cache_dir,
        markets = markets.len(),
        out = ?out,
        "local cache discovery complete"
    );
    println!(
        "discovered {} cached markets for {} -> {}",
        markets.len(),
        date,
        out.display()
    );
    Ok(())
}

fn discover_local_cache_book_metadata(
    cache_dir: PathBuf,
    date: String,
    slug_prefix: String,
    token_outcome: String,
    out: PathBuf,
) -> Result<()> {
    let markets = discovery::discover_markets_from_local_book_metadata(
        &cache_dir,
        &date,
        &slug_prefix,
        &token_outcome,
    )?;
    let mut f = std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
    for m in &markets {
        writeln!(f, "{}", serde_json::to_string(m)?)?;
    }
    tracing::info!(
        date = %date,
        cache_dir = ?cache_dir,
        token_outcome = %token_outcome,
        markets = markets.len(),
        out = ?out,
        "local cache book metadata discovery complete"
    );
    println!(
        "discovered {} cached book-metadata markets for {} -> {}",
        markets.len(),
        date,
        out.display()
    );
    println!("note: output has outcome=Unknown; run walk-forward without --use-outcome-label");
    Ok(())
}

async fn discover_range(
    start_date: String,
    end_date: String,
    slug_prefix: String,
    max_concurrent: usize,
    availability_cache: Option<PathBuf>,
    out: PathBuf,
) -> Result<()> {
    let start = NaiveDate::parse_from_str(&start_date, "%Y-%m-%d")
        .with_context(|| format!("parse --start-date {start_date}"))?;
    let end = NaiveDate::parse_from_str(&end_date, "%Y-%m-%d")
        .with_context(|| format!("parse --end-date {end_date}"))?;
    if end < start {
        return Err(anyhow!("--end-date must be >= --start-date"));
    }

    let cfg = TelonexStoreConfig::from_env()?;
    let store = TelonexStore::try_new(&cfg)?;
    let mut all = Vec::new();
    let mut day = start;
    while day <= end {
        let date = day.format("%Y-%m-%d").to_string();
        let markets = discovery::discover_markets(
            &store,
            &date,
            &slug_prefix,
            max_concurrent,
            availability_cache.as_deref(),
        )
        .await
        .with_context(|| format!("discover markets for {date}"))?;
        tracing::info!(
            date,
            markets = markets.len(),
            "range discovery day complete"
        );
        all.extend(markets);
        day = day
            .succ_opt()
            .ok_or_else(|| anyhow!("date overflow after {date}"))?;
    }
    all.sort_by_key(|m| m.close_ts);
    all.dedup_by(|a, b| a.asset_id == b.asset_id);

    let mut f = std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
    for m in &all {
        writeln!(f, "{}", serde_json::to_string(m)?)?;
    }
    tracing::info!(
        start = %start_date,
        end = %end_date,
        markets = all.len(),
        out = %out.display(),
        "range discovery complete"
    );
    println!(
        "discovered {} markets for {}..{} -> {}",
        all.len(),
        start_date,
        end_date,
        out.display()
    );
    Ok(())
}

async fn discover_markets_parquet(
    markets_parquet: PathBuf,
    start_date: String,
    end_date: String,
    slug_prefix: String,
    require_book_s3: bool,
    out: PathBuf,
) -> Result<()> {
    let start = NaiveDate::parse_from_str(&start_date, "%Y-%m-%d")
        .with_context(|| format!("parse --start-date {start_date}"))?;
    let end = NaiveDate::parse_from_str(&end_date, "%Y-%m-%d")
        .with_context(|| format!("parse --end-date {end_date}"))?;
    if end < start {
        return Err(anyhow!("--end-date must be >= --start-date"));
    }

    let file = File::open(&markets_parquet)
        .with_context(|| format!("open markets parquet {}", markets_parquet.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("read parquet metadata {}", markets_parquet.display()))?;
    let mut reader = builder
        .with_batch_size(8192)
        .build()
        .context("build markets parquet reader")?;

    let mut all = Vec::new();
    for batch in &mut reader {
        let batch = batch.context("read markets parquet batch")?;
        append_market_rows_from_parquet(&batch, &slug_prefix, start, end, &mut all)?;
    }

    all.sort_by_key(|m| (m.close_ts, m.asset_id.clone()));
    all.dedup_by(|a, b| a.slug == b.slug);

    if require_book_s3 {
        let before = all.len();
        let cfg = TelonexStoreConfig::from_env()?;
        let store = TelonexStore::try_new(&cfg)?;
        let available = available_book_assets_by_date(&store, start, end).await?;
        all.retain(|m| {
            available
                .get(&m.date)
                .is_some_and(|assets| assets.contains(&m.asset_id))
        });
        tracing::info!(
            before,
            after = all.len(),
            "filtered parquet markets to cached book_snapshot_25 assets"
        );
    }

    let mut f = std::fs::File::create(&out).with_context(|| format!("create {}", out.display()))?;
    for m in &all {
        writeln!(f, "{}", serde_json::to_string(m)?)?;
    }
    println!(
        "discovered {} markets from {} for {}..{} -> {}",
        all.len(),
        markets_parquet.display(),
        start_date,
        end_date,
        out.display()
    );
    Ok(())
}

async fn available_book_assets_by_date(
    store: &TelonexStore,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<HashMap<String, HashSet<String>>> {
    let mut out = HashMap::new();
    let mut day = start;
    while day <= end {
        let date = day.format("%Y-%m-%d").to_string();
        let assets = discovery::list_asset_ids_for_day(store, &date)
            .await
            .with_context(|| format!("list cached book assets for {date}"))?
            .into_iter()
            .collect::<HashSet<_>>();
        tracing::info!(date, assets = assets.len(), "cached book assets listed");
        out.insert(date.clone(), assets);
        day = day
            .succ_opt()
            .ok_or_else(|| anyhow!("date overflow after {date}"))?;
    }
    Ok(out)
}

fn append_market_rows_from_parquet(
    batch: &RecordBatch,
    slug_prefix: &str,
    start: NaiveDate,
    end: NaiveDate,
    out: &mut Vec<discovery::MarketHandle>,
) -> Result<()> {
    let slug = required_string_col(batch, "slug")?;
    let status = required_string_col(batch, "status")?;
    let result_id = required_string_col(batch, "result_id")?;
    let outcome_0 = required_string_col(batch, "outcome_0")?;
    let outcome_1 = required_string_col(batch, "outcome_1")?;
    let asset_id_0 = required_string_col(batch, "asset_id_0")?;
    let asset_id_1 = required_string_col(batch, "asset_id_1")?;
    let end_date_us = required_i64_col(batch, "end_date_us")?;

    for row in 0..batch.num_rows() {
        let Some(slug_value) = string_value(slug, row) else {
            continue;
        };
        if !slug_matches_prefixes(slug_value, slug_prefix) {
            continue;
        }
        if string_value(status, row) != Some("resolved") {
            continue;
        }
        let Some(start_ts) = discovery::parse_close_ts(slug_value) else {
            continue;
        };
        let Some(start_dt) = DateTime::from_timestamp(start_ts, 0) else {
            continue;
        };
        let date = start_dt.date_naive();
        if date < start || date > end {
            continue;
        }
        let Some((selected_idx, asset_id)) = canonical_up_asset_for_row(
            string_value(outcome_0, row),
            string_value(asset_id_0, row),
            string_value(outcome_1, row),
            string_value(asset_id_1, row),
        ) else {
            continue;
        };
        if asset_id.is_empty() {
            continue;
        }
        let outcome = match string_value(result_id, row) {
            Some("0") if selected_idx == 0 => "Up",
            Some("1") if selected_idx == 1 => "Up",
            Some("0" | "1") => "Down",
            _ => continue,
        };
        let close_ts = if end_date_us.is_valid(row) {
            end_date_us.value(row).div_euclid(1_000_000)
        } else {
            start_ts.saturating_add(300)
        };
        out.push(discovery::MarketHandle {
            asset_id: asset_id.to_string(),
            slug: slug_value.to_string(),
            close_ts,
            outcome: outcome.to_string(),
            date: date.to_string(),
        });
    }
    Ok(())
}

fn slug_matches_prefixes(slug: &str, slug_prefixes: &str) -> bool {
    slug_prefixes
        .split(',')
        .map(str::trim)
        .filter(|prefix| !prefix.is_empty())
        .any(|prefix| slug.starts_with(prefix))
}

fn canonical_up_asset_for_row<'a>(
    outcome_0: Option<&str>,
    asset_id_0: Option<&'a str>,
    outcome_1: Option<&str>,
    asset_id_1: Option<&'a str>,
) -> Option<(u8, &'a str)> {
    if outcome_is_up(outcome_0) {
        asset_id_0.map(|asset| (0, asset))
    } else if outcome_is_up(outcome_1) {
        asset_id_1.map(|asset| (1, asset))
    } else {
        None
    }
}

fn outcome_is_up(outcome: Option<&str>) -> bool {
    outcome.is_some_and(|value| {
        value.eq_ignore_ascii_case("up")
            || value.eq_ignore_ascii_case("yes")
            || value.eq_ignore_ascii_case("above")
    })
}

fn required_string_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a StringArray> {
    let idx = batch
        .schema()
        .fields()
        .iter()
        .position(|field| field.name() == name)
        .ok_or_else(|| anyhow!("markets parquet missing column {name}"))?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow!("markets parquet column {name} is not string"))
}

fn required_i64_col<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Int64Array> {
    let idx = batch
        .schema()
        .fields()
        .iter()
        .position(|field| field.name() == name)
        .ok_or_else(|| anyhow!("markets parquet missing column {name}"))?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow!("markets parquet column {name} is not int64"))
}

fn string_value(array: &StringArray, row: usize) -> Option<&str> {
    array.is_valid(row).then(|| array.value(row))
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
async fn walk_forward(
    markets_path: PathBuf,
    profile: Option<PathBuf>,
    back_to_explore_policy_scales_jsonl: Option<PathBuf>,
    skip_markets: usize,
    max_markets: usize,
    starting_cash: f64,
    kelly_fraction: f64,
    max_clip_usdc: f64,
    max_order_clip_multiplier: f64,
    max_per_market_exposure_usdc: f64,
    max_per_market_exposure_frac: Option<f64>,
    spot_symbol: String,
    strategies_csv: String,
    allow_legacy_strategies: bool,
    max_concurrent_fetches: usize,
    replay_sample_ms: u64,
    taker_latency_ms: u64,
    replay_event_cache_dir: Option<PathBuf>,
    load_pm_trades: bool,
    use_outcome_label: bool,
    portfolio_mode: bool,
    volatility_regime_threshold: f64,
    clip_fraction_of_equity: Option<f64>,
    clip_drawdown_soft_pct: f64,
    clip_drawdown_hard_pct: f64,
    clip_drawdown_min_multiplier: f64,
    clip_session_drawdown_soft_pct: f64,
    clip_session_drawdown_hard_pct: f64,
    clip_session_drawdown_min_multiplier: f64,
    daily_loss_cap_pct: f64,
    br2_disable_internal_model_gates: bool,
    br2_participation_clip_frac: f32,
    br2_participation_max_pair_cost: f32,
    br2_participation_max_orders_per_leg: usize,
    br2_participation_max_inventory_delta_shares: f64,
    br2_participation_repair_inventory_delta_shares: f64,
    br2_participation_refresh_secs: f32,
    br2_participation_stop_secs_before_close: f32,
    br2_hedged_base_enabled: bool,
    br2_hedged_base_max_secs_in: f32,
    br2_hedged_base_max_pair_cost: f32,
    br2_hedged_base_min_minority_leg_frac: f32,
    br2_hedged_base_clip_usdc: f32,
    br2_hedged_base_max_notional_usdc: f32,
    br2_late_directional_overlay_frac: f32,
    br2_min_composite_direction: f32,
    br2_early_clip_frac: f32,
    br2_mid_clip_frac: f32,
    br2_late_clip_frac: f32,
    br2_late_max_fires: usize,
    br2_late_confirm_min_model_confidence: f32,
    br2_late_confirm_max_model_risk: f32,
    br2_late_confirm_min_model_side_p: f32,
    br2_late_confirm_min_model_edge: f32,
    br2_late_confirm_min_book_skew: f32,
    br2_late_confirm_max_whipsaw_score: f32,
    br2_late_confirm_min_realized_vol_180s_bps: f32,
    br2_late_confirm_max_observed_range: f32,
    br2_recent_regime_gate_enabled: bool,
    br2_recent_regime_gate_min_edge: f32,
    br2_recent_regime_gate_late_confirm: bool,
    br2_recent_regime_gate_high_skew: bool,
    br2_recent_regime_gate_late_favourite: bool,
    br2_high_skew_clip_frac: f32,
    br2_lane_size_late_favourite: f32,
    br2_lane_size_late_confirm: f32,
    br2_lane_size_high_skew: f32,
    br2_regime_gate_enabled: bool,
    br2_regime_gate_window: u8,
    br2_regime_gate_threshold: f32,
    br2_regime_gate_soft_band: f32,
    br2_regime_gate_lane_floor: f32,
    br2_regime_gate_whipsaw_weight: f32,
    br2_high_skew_max_clips: usize,
    br2_high_skew_max_whipsaw_score: f32,
    br2_high_skew_min_realized_vol_180s_bps: f32,
    br2_late_favourite_start_secs: f32,
    br2_late_favourite_threshold: f32,
    br2_late_favourite_min_ask: f32,
    br2_late_favourite_max_ask: f32,
    br2_late_favourite_clip_frac: f32,
    br2_late_favourite_high_cert_clip_frac: f32,
    br2_late_favourite_high_cert_full_clip_edge: f32,
    br2_late_favourite_fragile_high_cert_ask: f32,
    br2_late_favourite_fragile_high_cert_max_edge: f32,
    br2_late_favourite_fragile_high_cert_max_path_efficiency: f32,
    br2_late_favourite_fragile_high_cert_size_frac: f32,
    br2_late_favourite_max_clips: usize,
    br2_late_favourite_min_sustain_secs: f32,
    br2_late_favourite_sweep_depth: usize,
    br2_late_favourite_min_model_confidence: f32,
    br2_late_favourite_min_model_direction_abs: f32,
    br2_late_favourite_max_model_risk: f32,
    br2_late_favourite_min_model_side_p: f32,
    br2_late_favourite_min_model_edge: f32,
    br2_late_favourite_high_cert_min_model_edge: f32,
    br2_late_favourite_high_cert_bypass_model_edge: bool,
    br2_late_favourite_max_whipsaw_score: f32,
    br2_late_favourite_max_reversal_pressure: f32,
    br2_late_favourite_min_path_efficiency: f32,
    br2_late_favourite_min_realized_vol_180s_bps: f32,
    br2_late_favourite_max_observed_range: f32,
    br2_late_favourite_range_soft_throttle: f32,
    br2_late_favourite_range_hard_throttle: f32,
    br2_late_favourite_range_extra_edge: f32,
    br2_late_favourite_range_extra_confidence: f32,
    br2_late_favourite_max_adverse_fast_momentum: f32,
    br2_late_favourite_max_adverse_broad_momentum: f32,
    br2_late_favourite_max_entry_pullback: f32,
    br2_late_favourite_max_avg_entry_drawdown: f32,
    br2_tail_clip_frac: f32,
    br2_tail_max_clips: usize,
    br2_tail_sweep_depth: usize,
    br2_tail_min_ask: f32,
    br2_tail_max_ask: f32,
    br2_tail_min_seconds_to_close: f32,
    br2_tail_min_favourite_unrealized_edge: f32,
    br2_tail_min_observed_range: f32,
    br2_tail_target_favourite_loss_coverage_frac: f32,
    br2_tail_reversal_coverage_frac: f32,
    br2_tail_reversal_min_seconds_to_close: f32,
    br2_tail_reversal_max_seconds_to_close: f32,
    br2_tail_reversal_min_favourite_ask: f32,
    br2_tail_extreme_threshold: f32,
    br2_tail_min_skew_step: f32,
    br2_tail_budget_favourite_spend_frac: f32,
    br2_tail_budget_favourite_upside_frac: f32,
    br2_tail_regime_boost_coverage_frac: f32,
    br2_tail_regime_boost_budget_spend_frac: f32,
    br2_tail_regime_boost_budget_upside_frac: f32,
    br2_tail_regime_boost_min_whipsaw_score: f32,
    br2_tail_regime_boost_min_reversal_pressure: f32,
    br2_tail_regime_boost_min_realized_vol_180s_bps: f32,
    br2_tail_regime_boost_max_path_efficiency: f32,
    br2_reversal_score_enabled: bool,
    br2_reversal_score_coeffs: Option<PathBuf>,
    br2_reversal_score_cov_min: f32,
    br2_reversal_score_cov_max: f32,
    br2_reversal_score_size_floor: f32,
    br2_reversal_score_size_ceiling: f32,
    enforce_model_gate: bool,
    model_gate_min_confidence: f32,
    model_gate_max_risk: f32,
    model_gate_min_edge: f32,
    model_btc_whipsaw_risk_weight: f32,
    model_btc_path_inefficiency_risk_weight: f32,
    model_btc_reversal_pressure_risk_weight: f32,
    enable_market_context_features: bool,
    walk_forward_folds: Option<usize>,
    fold_size: Option<usize>,
    purge_markets: usize,
    min_train_markets: usize,
    meta_epochs: usize,
    meta_learning_rate: f32,
    meta_l2: f32,
    meta_weight_clip: f32,
    meta_max_fit_samples: usize,
    meta_max_validation_samples: usize,
    meta_max_samples_per_market: usize,
    meta_max_oos_evaluation_samples: usize,
    meta_train_min_base_p: f32,
    meta_train_max_early_penalty: f32,
    meta_train_min_mid_distance: f32,
    meta_training_samples_cache: Option<PathBuf>,
    meta_calibrator_snapshot_in: Option<PathBuf>,
    meta_calibrator_snapshot_out: Option<PathBuf>,
    forbid_meta_training: bool,
    disable_meta_calibration: bool,
    portfolio_checkpoint_every_markets: usize,
    decision_log: Option<PathBuf>,
    decision_log_every_n: usize,
    local_cache_dir: Option<PathBuf>,
    out_markets: Option<PathBuf>,
    out_summary: Option<PathBuf>,
) -> Result<()> {
    let file = std::fs::File::open(&markets_path)
        .with_context(|| format!("open markets file {}", markets_path.display()))?;
    let mut markets: Vec<discovery::MarketHandle> = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        markets.push(serde_json::from_str(&line)?);
    }
    if markets.is_empty() {
        return Err(anyhow!("no markets in {}", markets_path.display()));
    }
    // Guard against accidentally using an output manifest from a prior run dir as input.
    // Input manifests belong under data/manifests/; data/runs/*/ is for results (markets.jsonl, logs, summaries).
    // (The check is here so it also catches after any early filtering, but we warn on the original path.)
    if markets_path.to_string_lossy().contains("/runs/") {
        tracing::warn!(
            path = %markets_path.display(),
            "input --markets path lives under a data/runs/ experiment dir. \
             This often indicates a copy-paste error or stale path. \
             Prefer manifests in data/manifests/ (or a dedicated data/manifests/<experiment>/) \
             so runs for different strategies (e.g. lively vs back_to_explore) do not mix inputs."
        );
    }
    if skip_markets > 0 || max_markets > 0 {
        markets.sort_by_key(|m| m.close_ts);
        if skip_markets >= markets.len() {
            return Err(anyhow!(
                "--skip-markets={} leaves no markets from {}",
                skip_markets,
                markets_path.display()
            ));
        }
        if skip_markets > 0 {
            markets.drain(0..skip_markets);
        }
        if max_markets > 0 {
            markets.truncate(max_markets);
        }
        tracing::info!(
            skip_markets,
            max_markets,
            markets = markets.len(),
            "sliced market list"
        );
    }
    if walk_forward_folds.is_some() && fold_size.is_some() {
        return Err(anyhow!(
            "cannot set both --walk-forward-folds and --fold-size"
        ));
    }
    if walk_forward_folds == Some(0) {
        return Err(anyhow!("--walk-forward-folds must be >= 1"));
    }
    if fold_size == Some(0) {
        return Err(anyhow!("--fold-size must be >= 1"));
    }
    if decision_log.is_some() && !portfolio_mode {
        return Err(anyhow!(
            "--decision-log is currently supported only with --portfolio-mode"
        ));
    }
    if use_outcome_label {
        crate::walkforward::validate_outcome_labels(&markets)?;
    }

    let strategies = parse_strategies(&strategies_csv, allow_legacy_strategies)?;

    // Load optional strategy profile (currently supports bonereaper_v2 and back_to_explore).
    let selected_profile = if let Some(p) = &profile {
        match StrategyProfileFile::load(p) {
            Ok(profile_file) => {
                if profile_file.warn_if_inactive(&strategies) {
                    tracing::warn!(
                        path = %p.display(),
                        available_strategies = ?strategies.iter().map(|s| s.name()).collect::<Vec<_>>(),
                        selected_profile = %profile_file
                            .strategy_name()
                            .unwrap_or(""),
                        "profile strategy is not part of --strategies and will be ignored"
                    );
                    None
                } else {
                    match profile_file.selected_strategy_profile(&strategies) {
                        Ok(Some(profile)) => {
                            let strategy_name = profile_file.strategy_name().unwrap_or("unknown");
                            tracing::info!(path = %p.display(), strategy = strategy_name, "loaded strategy profile");
                            Some((strategy_name.to_string(), profile))
                        }
                        Ok(None) => {
                            tracing::warn!(path = %p.display(), "profile strategy not active in selected strategy set");
                            None
                        }
                        Err(e) => {
                            tracing::warn!(path = %p.display(), error = %e, "failed to resolve strategy profile, continuing without it");
                            None
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "failed to load profile, continuing without it");
                None
            }
        }
    } else {
        None
    };

    let store = if let Some(ref dir) = local_cache_dir {
        tracing::info!(?dir, "using local cache");
        TelonexStore::try_new_local(dir.clone())?
    } else {
        let cfg = TelonexStoreConfig::from_env()?;
        TelonexStore::try_new(&cfg)?
    };

    let mut wf_cfg = WalkForwardConfig {
        starting_cash_usdc: starting_cash,
        kelly_fraction,
        max_clip_usdc,
        max_order_clip_multiplier,
        max_per_market_exposure_usdc,
        max_per_market_exposure_frac,
        spot_symbol,
        strategies,
        max_concurrent_fetches,
        replay_sample_ms,
        taker_latency_ms,
        replay_event_cache_dir,
        load_pm_trades,
        use_outcome_label,
        maker_rebate_bps: 10.0,
        taker_fee_bps: 0.0,
        portfolio_mode,
        clip_fraction_of_equity,
        clip_drawdown_soft_pct,
        clip_drawdown_hard_pct,
        clip_drawdown_min_multiplier,
        clip_session_drawdown_soft_pct,
        clip_session_drawdown_hard_pct,
        clip_session_drawdown_min_multiplier,
        daily_loss_cap_pct,
        br2_disable_internal_model_gates,
        br2_participation_clip_frac,
        br2_participation_max_pair_cost,
        br2_participation_max_orders_per_leg,
        br2_participation_max_inventory_delta_shares,
        br2_participation_repair_inventory_delta_shares,
        br2_participation_refresh_secs,
        br2_participation_stop_secs_before_close,
        br2_hedged_base_enabled,
        br2_hedged_base_max_secs_in,
        br2_hedged_base_max_pair_cost,
        br2_hedged_base_min_minority_leg_frac,
        br2_hedged_base_clip_usdc,
        br2_hedged_base_max_notional_usdc,
        br2_late_directional_overlay_frac,
        br2_min_composite_direction,
        br2_early_clip_frac,
        br2_mid_clip_frac,
        br2_late_clip_frac,
        br2_late_max_fires,
        br2_late_confirm_min_model_confidence,
        br2_late_confirm_max_model_risk,
        br2_late_confirm_min_model_side_p,
        br2_late_confirm_min_model_edge,
        br2_late_confirm_min_book_skew,
        br2_late_confirm_max_whipsaw_score,
        br2_late_confirm_min_realized_vol_180s_bps,
        br2_late_confirm_max_observed_range,
        br2_recent_regime_gate_enabled,
        br2_recent_regime_gate_min_edge,
        br2_recent_regime_gate_late_confirm,
        br2_recent_regime_gate_high_skew,
        br2_recent_regime_gate_late_favourite,
        br2_high_skew_clip_frac,
        br2_lane_size_late_favourite,
        br2_lane_size_late_confirm,
        br2_lane_size_high_skew,
        br2_regime_gate_enabled,
        br2_regime_gate_window,
        br2_regime_gate_threshold,
        br2_regime_gate_soft_band,
        br2_regime_gate_lane_floor,
        br2_regime_gate_whipsaw_weight,
        br2_high_skew_max_clips,
        br2_high_skew_max_whipsaw_score,
        br2_high_skew_min_realized_vol_180s_bps,
        br2_late_favourite_start_secs,
        br2_late_favourite_threshold,
        br2_late_favourite_min_ask,
        br2_late_favourite_max_ask,
        br2_late_favourite_clip_frac,
        br2_late_favourite_high_cert_clip_frac,
        br2_late_favourite_high_cert_full_clip_edge,
        br2_late_favourite_fragile_high_cert_ask,
        br2_late_favourite_fragile_high_cert_max_edge,
        br2_late_favourite_fragile_high_cert_max_path_efficiency,
        br2_late_favourite_fragile_high_cert_size_frac,
        br2_late_favourite_max_clips,
        br2_late_favourite_min_sustain_secs,
        br2_late_favourite_sweep_depth,
        br2_late_favourite_min_model_confidence,
        br2_late_favourite_min_model_direction_abs,
        br2_late_favourite_max_model_risk,
        br2_late_favourite_min_model_side_p,
        br2_late_favourite_min_model_edge,
        br2_late_favourite_high_cert_min_model_edge,
        br2_late_favourite_high_cert_bypass_model_edge,
        br2_late_favourite_max_whipsaw_score,
        br2_late_favourite_max_reversal_pressure,
        br2_late_favourite_min_path_efficiency,
        br2_late_favourite_min_realized_vol_180s_bps,
        br2_late_favourite_max_observed_range,
        br2_late_favourite_range_soft_throttle,
        br2_late_favourite_range_hard_throttle,
        br2_late_favourite_range_extra_edge,
        br2_late_favourite_range_extra_confidence,
        br2_late_favourite_max_adverse_fast_momentum,
        br2_late_favourite_max_adverse_broad_momentum,
        br2_late_favourite_max_entry_pullback,
        br2_late_favourite_max_avg_entry_drawdown,
        br2_tail_clip_frac,
        br2_tail_max_clips,
        br2_tail_sweep_depth,
        br2_tail_min_ask,
        br2_tail_max_ask,
        br2_tail_min_seconds_to_close,
        br2_tail_min_favourite_unrealized_edge,
        br2_tail_min_observed_range,
        br2_tail_target_favourite_loss_coverage_frac,
        br2_tail_reversal_coverage_frac,
        br2_tail_reversal_min_seconds_to_close,
        br2_tail_reversal_max_seconds_to_close,
        br2_tail_reversal_min_favourite_ask,
        br2_tail_extreme_threshold,
        br2_tail_min_skew_step,
        br2_tail_budget_favourite_spend_frac,
        br2_tail_budget_favourite_upside_frac,
        br2_tail_regime_boost_coverage_frac,
        br2_tail_regime_boost_budget_spend_frac,
        br2_tail_regime_boost_budget_upside_frac,
        br2_tail_regime_boost_min_whipsaw_score,
        br2_tail_regime_boost_min_reversal_pressure,
        br2_tail_regime_boost_min_realized_vol_180s_bps,
        br2_tail_regime_boost_max_path_efficiency,
        br2_reversal_score_enabled,
        br2_reversal_score_coeffs_path: br2_reversal_score_coeffs,
        br2_reversal_score_cov_min,
        br2_reversal_score_cov_max,
        br2_reversal_score_size_floor,
        br2_reversal_score_size_ceiling,
        enforce_model_gate,
        model_gate_min_confidence,
        model_gate_max_risk,
        model_gate_min_edge,
        model_btc_whipsaw_risk_weight,
        model_btc_path_inefficiency_risk_weight,
        model_btc_reversal_pressure_risk_weight,
        enable_market_context_features,
        volatility_regime_threshold,
        walk_forward_folds,
        fold_size,
        purge_markets,
        min_train_markets,
        meta_training_config: MetaTrainingConfig {
            epochs: meta_epochs,
            learning_rate: meta_learning_rate,
            l2: meta_l2,
            weight_clip: meta_weight_clip,
            reset_before_fit: true,
        },
        meta_max_fit_samples,
        meta_max_validation_samples,
        meta_max_samples_per_market,
        meta_max_oos_evaluation_samples,
        meta_train_min_base_p,
        meta_train_max_early_penalty,
        meta_train_min_mid_distance,
        meta_training_samples_cache,
        meta_calibrator_snapshot_in,
        meta_calibrator_snapshot_out,
        forbid_meta_training,
        enable_meta_calibration: !disable_meta_calibration,
        portfolio_checkpoint_every_markets,
        decision_log_jsonl: decision_log,
        decision_log_every_n,
        checkpoint_markets_out: out_markets.clone(),
        checkpoint_summary_out: out_summary.clone(),
        back_to_explore_policy_scales_jsonl,
        ..WalkForwardConfig::default()
    };

    // Apply profile values last. Profile files are the canonical way to run
    // named variants; keep ad hoc CLI sweeps profile-free or create a profile.
    let active_strats: Vec<&str> = wf_cfg.strategies.iter().map(|s| s.name()).collect();
    let profile_log = selected_profile.as_ref().map(|(strategy, profile)| {
        profile.apply_to_walkforward_config(&mut wf_cfg);
        serde_json::json!({
            "strategy": strategy,
            "strategy_profile": profile.strategy_name(),
        })
    });

    let profile_log = profile_log.or_else(|| {
        profile.as_ref().map(|path| {
            serde_json::json!({
                "path": path
            })
        })
    });

    let effective_config = serde_json::json!({
        "profile": profile_log,
        "strategies": active_strats,
        "starting_cash_usdc": wf_cfg.starting_cash_usdc,
        "replay_sample_ms": wf_cfg.replay_sample_ms,
        "taker_latency_ms": wf_cfg.taker_latency_ms,
        "clip_fraction_of_equity": wf_cfg.clip_fraction_of_equity,
        "max_clip_usdc": wf_cfg.max_clip_usdc,
        "max_order_clip_multiplier": wf_cfg.max_order_clip_multiplier,
        "max_per_market_exposure_usdc": wf_cfg.max_per_market_exposure_usdc,
        "max_per_market_exposure_frac": wf_cfg.max_per_market_exposure_frac,
        "clip_drawdown_soft_pct": wf_cfg.clip_drawdown_soft_pct,
        "clip_drawdown_hard_pct": wf_cfg.clip_drawdown_hard_pct,
        "daily_loss_cap_pct": wf_cfg.daily_loss_cap_pct,
        "back_to_explore_policy_scales_jsonl": wf_cfg
            .back_to_explore_policy_scales_jsonl
            .as_ref()
            .map(|path| path.to_string_lossy()),
        "forbid_meta_training": wf_cfg.forbid_meta_training,
    });
    tracing::info!(
        strategies = ?active_strats,
        portfolio_mode = wf_cfg.portfolio_mode,
        clip_fraction_of_equity = ?wf_cfg.clip_fraction_of_equity,
        daily_loss_cap_pct = wf_cfg.daily_loss_cap_pct,
        clip_drawdown_hard_pct = wf_cfg.clip_drawdown_hard_pct,
        replay_sample_ms = wf_cfg.replay_sample_ms,
        "effective walk-forward profile"
    );

    tracing::info!(markets = markets.len(), "starting walk-forward");
    let started = Instant::now();
    let (results, summary) = run_walkforward(&store, &markets, &wf_cfg).await?;
    let elapsed = started.elapsed().as_secs_f64();
    tracing::info!(elapsed_s = elapsed, "walk-forward complete");

    print_summary(&summary);

    if let Some(p) = out_markets {
        write_market_results_jsonl_atomic(&p, &results)?;
        tracing::info!(?p, "wrote per-market results");
    }
    if let Some(p) = out_summary {
        write_summary_json_atomic(&p, &summary)?;
        tracing::info!(?p, "wrote summary");

        let git_sha = std::env::var("PM_SOURCE_GIT_SHA")
            .ok()
            .filter(|sha| !sha.trim().is_empty())
            .or_else(|| {
                std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
            });
        let manifest_path = p.with_file_name("run_manifest.json");
        let manifest = serde_json::json!({
            "profile_path": profile.as_ref().map(|p| p.to_string_lossy()),
            "git_sha": git_sha,
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "command": std::env::args().collect::<Vec<_>>(),
            "effective_config": effective_config,
        });
        if let Ok(mut f) = std::fs::File::create(&manifest_path) {
            let _ = writeln!(
                f,
                "{}",
                serde_json::to_string_pretty(&manifest).unwrap_or_default()
            );
            tracing::info!(?manifest_path, "wrote run_manifest.json");
        }
    }
    Ok(())
}

fn parse_strategies(csv: &str, allow_legacy: bool) -> Result<Vec<StratId>> {
    let mut out = Vec::new();
    let mut legacy: Vec<&str> = Vec::new();
    for token in csv.split(',').map(str::trim) {
        let id = StratId::from_name(token).ok_or_else(|| {
            anyhow!(
                "unknown strategy: {token}. supported strategies: {}",
                StratId::all_names().join(", ")
            )
        })?;
        if !allow_legacy && !id.is_active() {
            legacy.push(token);
            continue;
        }
        out.push(id);
    }

    if !allow_legacy && !legacy.is_empty() {
        let mut active = StratId::active_names();
        let mut archived = StratId::archived_names();
        active.sort_unstable();
        legacy.sort_unstable();
        archived.sort_unstable();
        return Err(anyhow!(
            "legacy strategies disabled in this run: {}. Set --allow-legacy-strategies and rerun, or pass only active strategies: {}. Archived strategies: {}",
            legacy.join(", "),
            active.join(", "),
            archived.join(", ")
        ));
    }

    if out.is_empty() {
        return Err(anyhow!(
            "no strategies specified; either use active strategy IDs or pass --allow-legacy-strategies for archived ones"
        ));
    }

    Ok(out)
}

/// Parses `btc-updown-5m-1778587500` -> 1778587500.
fn parse_close_ts_from_slug(slug: &str) -> Result<i64> {
    slug.rsplit('-')
        .next()
        .and_then(|t| t.parse::<i64>().ok())
        .ok_or_else(|| anyhow!("could not parse resolution timestamp from slug: {slug}"))
}

async fn fetch_tape(
    exchange: &str,
    channel: Channel,
    date: &str,
    asset_id: &str,
    market_id: MarketId,
    local_cache_dir: Option<&PathBuf>,
) -> Result<(
    TelonexStore,
    Vec<pm_types::ReplayEvent>,
    pm_telonex_loader::LoadStats,
)> {
    let store = if let Some(cache_dir) = local_cache_dir {
        TelonexStore::try_new_local(cache_dir.clone())?
    } else {
        let cfg = TelonexStoreConfig::from_env()?;
        tracing::info!(bucket = %cfg.bucket, region = %cfg.region, "connecting to S3");
        TelonexStore::try_new(&cfg)?
    };

    let resolve_started = Instant::now();
    let path = store
        .resolve_asset_day(exchange, channel, date, asset_id)
        .await?;
    tracing::info!(
        ?path,
        took_ms = resolve_started.elapsed().as_millis() as u64,
        "resolved parquet"
    );

    let load_started = Instant::now();
    let (events, stats) = load_book_snapshot_async(store.store(), path.clone(), market_id).await?;
    if stats.out_of_order_rows > 0 {
        tracing::warn!(
            out_of_order = stats.out_of_order_rows,
            "rewound-timestamps in source tape; events re-sorted for determinism"
        );
    }
    tracing::info!(
        load_ms = load_started.elapsed().as_millis() as u64,
        rows = stats.rows_emitted,
        "tape loaded"
    );
    Ok((store, events, stats))
}

async fn inspect_s3(
    exchange: String,
    channel: Channel,
    date: String,
    asset_id: String,
    market_id: MarketId,
    head: usize,
    local_cache_dir: Option<PathBuf>,
) -> Result<()> {
    let (store, events, stats) = fetch_tape(
        &exchange,
        channel,
        &date,
        &asset_id,
        market_id,
        local_cache_dir.as_ref(),
    )
    .await?;

    println!(
        "== s3://{}/raw/telonex/exchange={}/channel={}/date={}/asset_id={} ==",
        store.bucket, exchange, channel, date, asset_id
    );
    println!("batches         : {}", stats.batches);
    println!("rows_total      : {}", stats.rows_total);
    println!("rows_emitted    : {}", stats.rows_emitted);
    println!("rows_null_top   : {}", stats.rows_null_top);
    println!("rows_reordered  : {}", stats.out_of_order_rows);
    if let (Some(f), Some(l)) = (stats.first_ts_ns, stats.last_ts_ns) {
        let fdt = DateTime::<Utc>::from_timestamp_nanos(f);
        let ldt = DateTime::<Utc>::from_timestamp_nanos(l);
        println!(
            "ts_range_utc    : {} -> {}",
            fdt.to_rfc3339(),
            ldt.to_rfc3339()
        );
        let dur_s = (l - f) as f64 / 1e9;
        println!("duration_seconds: {:.1}", dur_s);
    }

    if events.is_empty() {
        println!("no events emitted");
        return Ok(());
    }

    let mut min_spread = f32::INFINITY;
    let mut max_spread = f32::NEG_INFINITY;
    let mut sum_spread = 0.0f64;
    let mut crossed = 0usize;
    for e in &events {
        if e.yes_bid > 0.0 && e.yes_ask > 0.0 {
            let sp = e.yes_ask - e.yes_bid;
            if sp < min_spread {
                min_spread = sp;
            }
            if sp > max_spread {
                max_spread = sp;
            }
            sum_spread += sp as f64;
            if sp < 0.0 {
                crossed += 1;
            }
        }
    }
    let avg_spread = sum_spread / events.len() as f64;
    println!(
        "spread (yes)    : min={:.4}  avg={:.4}  max={:.4}  crossed_rows={}",
        min_spread, avg_spread, max_spread, crossed
    );
    println!(
        "yes_mid first/last: {:.4} -> {:.4}",
        events.first().unwrap().yes_mid,
        events.last().unwrap().yes_mid
    );

    println!("\nfirst {head} events:");
    for e in events.iter().take(head) {
        let dt = DateTime::<Utc>::from_timestamp_nanos(e.ts_ns);
        println!(
            "  {} mid={:.4} bid={:.4}x{:>7.1} ask={:.4}x{:>7.1}",
            dt.format("%H:%M:%S%.3f"),
            e.yes_mid,
            e.yes_bid,
            e.bids[0].size,
            e.yes_ask,
            e.asks[0].size
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn backtest_s3(
    exchange: String,
    channel: Channel,
    date: String,
    asset_id: String,
    market_id: MarketId,
    strategy: StrategyKind,
    starting_cash: f64,
    limits: PortfolioLimits,
    close_ts_seconds: i64,
    resolved_yes: Option<bool>,
    spot_symbol: String,
    out: Option<PathBuf>,
    equity_curve: Option<PathBuf>,
    decision_log: Option<PathBuf>,
    decision_log_every_n: usize,
    local_cache_dir: Option<PathBuf>,
) -> Result<()> {
    run_market_backtest(
        exchange,
        channel,
        date,
        asset_id,
        market_id,
        strategy,
        starting_cash,
        limits,
        close_ts_seconds,
        resolved_yes,
        spot_symbol,
        out,
        equity_curve,
        decision_log,
        decision_log_every_n,
        local_cache_dir,
        MarketRunMode::Backtest,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_market_backtest(
    exchange: String,
    channel: Channel,
    date: String,
    asset_id: String,
    market_id: MarketId,
    strategy: StrategyKind,
    starting_cash: f64,
    limits: PortfolioLimits,
    close_ts_seconds: i64,
    resolved_yes: Option<bool>,
    spot_symbol: String,
    out: Option<PathBuf>,
    equity_curve: Option<PathBuf>,
    decision_log: Option<PathBuf>,
    decision_log_every_n: usize,
    local_cache_dir: Option<PathBuf>,
    mode: MarketRunMode,
) -> Result<()> {
    let (store, events, _stats) = fetch_tape(
        &exchange,
        channel,
        &date,
        &asset_id,
        market_id,
        local_cache_dir.as_ref(),
    )
    .await?;

    let spot_history = if spot_symbol.is_empty() {
        SpotHistory::default()
    } else {
        load_spot_history(&store, &spot_symbol, &date).await?
    };

    let market_close_ns = close_ts_seconds.saturating_mul(1_000_000_000);
    let market_run_mode = mode.as_str();
    let max_clip_usdc = limits.max_clip_usdc;
    let cfg = RunnerConfig {
        current_btc_net_shares: 0.0,
        current_eth_net_shares: 0.0,
        starting_cash_usdc: starting_cash,
        market_open_ns: market_close_ns.saturating_sub(300_000_000_000),
        market_close_ns,
        resolved_yes,
        portfolio_limits: limits,
        equity_curve_jsonl: equity_curve,
        snapshot_every_n: 200,
        maker_rebate_bps: 10.0,
        taker_fee_bps: 0.0,
        max_inventory_imbalance_shares: 1.5,
        taker_slippage_bps: 15.0,
        taker_latency_ms: 0,
        decision_log_jsonl: decision_log,
        decision_log_parquet: None,
        strategy_name: market_run_mode.to_string(),
        shared_model_state: None,
        update_model_state_on_resolution: true,
        meta_calibrator_snapshot: None,
        enable_meta_calibration: true,
        model_market_context: pm_model::ModelMarketContext::default(),
        prior_market_range_1d: 0.0,
        prior_market_range_3d: 0.0,
        prior_market_range_7d: 0.0,
        model_btc_whipsaw_risk_weight: 0.16,
        model_btc_path_inefficiency_risk_weight: 0.10,
        model_btc_reversal_pressure_risk_weight: 0.12,
        decision_log_every_n,
        enforce_model_gate: true,
        model_gate_min_confidence: 0.68,
        model_gate_max_risk: 0.72,
        model_gate_min_edge: 0.00,
        daily_start_cash_usdc: starting_cash,
        daily_loss_cap_pct: 1.0,
        current_daily_loss_pct: 0.0,
    };
    let trade_history = match resolve_pm_trades_day(&store, &date, &asset_id).await {
        Ok(path) => match load_pm_trades_async(store.store(), path).await {
            Ok((trades, stats)) => {
                tracing::info!(
                    rows = stats.rows_emitted,
                    buys = stats.buy_count,
                    sells = stats.sell_count,
                    "pm trades loaded"
                );
                TradeHistory::new(trades)
            }
            Err(e) => {
                tracing::warn!(error = %e, "pm trades load failed, defaulting to empty trade history");
                TradeHistory::default()
            }
        },
        Err(_) => TradeHistory::default(),
    };

    let started = Instant::now();
    let report = match strategy {
        StrategyKind::PairedMm => {
            let mut s = PairedMmDense::new(PairedMmDenseConfig {
                clip_shares: max_clip_usdc / 0.5_f64.max(0.01),
                ..PairedMmDenseConfig::default()
            });
            run_backtest(&events, &spot_history, &trade_history, &mut s, &cfg)?
        }
        StrategyKind::BonereaperV2 => {
            let mut s = BonereaperV2::new(BonereaperV2Config {
                bankroll_usdc: starting_cash,
                max_clip_usdc,
                ..BonereaperV2Config::default()
            });
            run_backtest(&events, &spot_history, &trade_history, &mut s, &cfg)?
        }
    };
    tracing::info!(
        elapsed_ms = started.elapsed().as_millis() as u64,
        events = report.events_processed,
        mode = market_run_mode,
        ?strategy,
        "market run done"
    );

    pretty_print(&report);

    if let Some(path) = out {
        let json = serde_json::to_string_pretty(&report)?;
        std::fs::write(&path, json)?;
        tracing::info!(?path, "wrote report");
    }
    Ok(())
}

/// Build a Nautilus-conformant symbol from a Polymarket slug. The dotted
/// venue suffix is added inside `polymarket_instrument_id`.
fn slug_to_nautilus_symbol(slug: &str) -> String {
    slug.to_uppercase()
}

async fn load_spot_history(store: &TelonexStore, symbol: &str, date: &str) -> Result<SpotHistory> {
    let load_started = Instant::now();
    let path = resolve_binance_day(store, "agg_trades", symbol, date).await?;
    let (ticks, stats) = load_binance_agg_trades_async(store.store(), path).await?;
    tracing::info!(
        symbol = %symbol,
        date = %date,
        ticks = stats.rows_emitted,
        load_ms = load_started.elapsed().as_millis() as u64,
        "spot history loaded"
    );
    Ok(SpotHistory::new(ticks))
}

async fn quotes_s3(
    exchange: String,
    channel: Channel,
    date: String,
    asset_id: String,
    slug: String,
    market_id: MarketId,
    head: usize,
    local_cache_dir: Option<PathBuf>,
) -> Result<()> {
    let (_store, events, stats) = fetch_tape(
        &exchange,
        channel,
        &date,
        &asset_id,
        market_id,
        local_cache_dir.as_ref(),
    )
    .await?;
    let symbol = slug_to_nautilus_symbol(&slug);
    let iid = polymarket_instrument_id(&symbol);

    println!("== nautilus QuoteTick conversion ==");
    println!("symbol        : {symbol}");
    println!("instrument_id : {iid}");
    println!("rows_emitted  : {}", stats.rows_emitted);

    let convert_start = Instant::now();
    let mut converted = 0usize;
    println!("\nfirst {head} QuoteTicks:");
    for (idx, e) in events.iter().enumerate() {
        let q = to_quote_tick(e, iid);
        converted += 1;
        if idx < head {
            let dt = DateTime::<Utc>::from_timestamp_nanos(u64::from(q.ts_event) as i64);
            println!(
                "  {} bid={}x{} ask={}x{}",
                dt.format("%H:%M:%S%.3f"),
                q.bid_price,
                q.bid_size,
                q.ask_price,
                q.ask_size
            );
        }
    }
    let convert_ms = convert_start.elapsed().as_millis() as u64;
    println!("converted     : {converted} QuoteTicks in {convert_ms}ms");

    Ok(())
}

async fn prep_cache_cmd(
    markets_path: PathBuf,
    cache_dir: PathBuf,
    spot_symbol: String,
    max_concurrent: usize,
    skip_existing: bool,
) -> Result<()> {
    let file = std::fs::File::open(&markets_path)
        .with_context(|| format!("open markets file {}", markets_path.display()))?;
    let mut markets: Vec<discovery::MarketHandle> = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        markets.push(serde_json::from_str(&line)?);
    }
    if markets.is_empty() {
        return Err(anyhow!("no markets in {}", markets_path.display()));
    }
    let cfg = TelonexStoreConfig::from_env()?;
    let store = TelonexStore::try_new(&cfg)?;
    let prep_cfg = prep_cache::PrepCacheConfig {
        cache_dir,
        spot_symbol,
        max_concurrent,
        skip_existing,
    };
    prep_cache::run_prep_cache(&store, &markets, &prep_cfg).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parquet_discovery_selects_up_asset_from_either_outcome_slot() {
        assert_eq!(
            canonical_up_asset_for_row(Some("Up"), Some("asset0"), Some("Down"), Some("asset1")),
            Some((0, "asset0"))
        );
        assert_eq!(
            canonical_up_asset_for_row(Some("Down"), Some("asset0"), Some("Up"), Some("asset1")),
            Some((1, "asset1"))
        );
    }

    #[test]
    fn parquet_discovery_rejects_rows_without_canonical_up_side() {
        assert_eq!(
            canonical_up_asset_for_row(Some("No"), Some("asset0"), Some("Down"), Some("asset1")),
            None
        );
    }

    #[test]
    fn parquet_discovery_slug_prefix_accepts_comma_separated_families() {
        let prefixes = "btc-updown-5m-, eth-updown-5m-,btc-updown-15m-";
        assert!(slug_matches_prefixes("btc-updown-5m-1778370900", prefixes));
        assert!(slug_matches_prefixes("eth-updown-5m-1778370900", prefixes));
        assert!(slug_matches_prefixes("btc-updown-15m-1778370300", prefixes));
        assert!(!slug_matches_prefixes("sol-updown-5m-1778370900", prefixes));
    }

    #[test]
    fn parse_strategies_defaults_to_active_set_only() {
        let parsed = parse_strategies("back_to_explore,paired_mm", false).unwrap();
        assert_eq!(parsed, vec![StratId::BackToExplore, StratId::PairedMm]);
    }

    #[test]
    fn parse_strategies_rejects_unknown_names() {
        assert!(parse_strategies("reactive_directional", false).is_err());
        assert!(parse_strategies("reactive_directional", true).is_err());
    }
}
