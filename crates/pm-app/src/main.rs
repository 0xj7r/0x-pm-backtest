#![recursion_limit = "256"]

use anyhow::{Context, Result, anyhow};
use arrow::array::{Array, Int64Array, StringArray};
use arrow::record_batch::RecordBatch;
use chrono::{DateTime, NaiveDate, Utc};
use clap::{Parser, Subcommand};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use pm_model::MetaTrainingConfig;
use pm_telonex_loader::{
    Channel, TelonexStore, TelonexStoreConfig,
    load_book_snapshot_async,
};
use pm_types::MarketId;
use std::fs::File;
use std::path::PathBuf;
use std::time::Instant;

mod alpha;
mod discovery;
use pm_shadow as shadow;
mod prep_cache;

use pm_backtest::config::{SpotSource, StrikeSource, WalkForwardConfig};
use pm_backtest::engine::{StratId, run_walkforward};
use pm_backtest::scorecard::{
    print_result_summary, print_summary, summarize_markets_jsonl, write_market_results_jsonl_atomic,
    write_result_summary_json, write_summary_json_atomic,
};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};

#[derive(Parser, Debug)]
#[command(
    name = "pm-app",
    version,
    about = "Polymarket backtest engine (pure Rust)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// LOG-ONLY live twin: streams Binance spot + Polymarket books, runs a
    /// `pm_strategy::Strategy` at the configured cadence and logs
    /// WOULD_ENTER/QUOTE_PROBE/WOULD_EXIT/SUMMARY as JSONL. Places NO orders.
    ///
    /// The flags here are engine concerns only. Entry thresholds, gates and
    /// sizing belong to the strategy's own config, and the only strategy that
    /// ships is `noop`, so today's stream is entry-free by construction.
    Shadow {
        /// Market family slug prefix.
        #[arg(long, default_value = "btc-updown-5m-")]
        slug_prefix: String,
        /// Strategy driving the twin. Only `noop` is accepted: the live twin
        /// runs deployable strategies, and there are none yet.
        #[arg(long, default_value = "noop")]
        strategy: String,
        /// Trailing realized-vol window in seconds.
        #[arg(long, default_value = "3600")]
        vol_lookback_s: u32,
        /// Mark-to-book exit horizon after entry, seconds (0 = hold to
        /// redemption; the engine measures settlement instead of an exit).
        #[arg(long, default_value = "0")]
        exit_after_s: u32,
        /// Quote-existence probe delay after entry, milliseconds.
        #[arg(long, default_value = "150")]
        latency_probe_ms: u64,
        /// Directory for JSONL shadow logs (created if missing).
        #[arg(long)]
        out_dir: PathBuf,
        /// Weight on the basis-adjusted perp last in the effective-spot
        /// blend (0 = spot-only, disables the futures feed).
        #[arg(long, default_value = "0.75")]
        perp_price_weight: f64,
        /// Vol estimator: "realized" (rolling) or "ewma".
        #[arg(long, default_value = "realized")]
        vol_estimator: String,
        /// EWMA half-life seconds (only when --vol-estimator ewma).
        #[arg(long, default_value = "600.0")]
        ewma_halflife_s: f64,
        /// Decision evaluation cadence in ms (default 1000 = harness-matched;
        /// 100 = fast mode). Does not change decision logic, only when it runs.
        #[arg(long, default_value = "1000")]
        decide_interval_ms: u64,
        /// Evaluate decisions on the next poll tick after any input event,
        /// floored by 20ms spacing; decide-interval-ms becomes the fallback
        /// heartbeat.
        #[arg(long)]
        decide_on_event: bool,
    },
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
        /// Polymarket taker fee curve rate: fee = rate * p * (1-p) per share
        /// on every aggressive fill (0 disables; crypto markets = 0.07).
        #[arg(long, default_value = "0.0")]
        fee_curve_rate: f64,
        /// Fee-aware exit: at the exit instant, sell only when net proceeds
        /// beat the belief's hold-to-resolution EV; otherwise hold.
        #[arg(long)]
        fee_aware_exit: bool,
        /// Variance-aversion premium for --fee-aware-exit: sell only if
        /// exit_net >= hold_ev + margin * shares.
        #[arg(long, default_value = "0.0")]
        fee_exit_margin: f64,
        #[arg(long, default_value = "50.0")]
        notional_usdc: f64,
        /// Kelly-style per-entry sizing (reliability-discounted edge +
        /// variance equalization); default is flat clips.
        #[arg(long)]
        kelly_sizing: bool,
        /// Thesis gate: skip when chosen-side belief is below this (0 = off).
        #[arg(long, default_value = "0.0")]
        min_p_side: f64,
        /// Thesis gate: skip when chosen-side belief exceeds this (1.0 = off).
        #[arg(long, default_value = "1.0")]
        max_p_side: f64,
        /// Thesis gate: skip when entry ask is below this (0 = off).
        #[arg(long, default_value = "0.0")]
        min_entry_ask: f64,
        /// Thesis gate: skip when entry ask exceeds this (1.0 = off).
        #[arg(long, default_value = "1.0")]
        max_entry_ask: f64,
        /// No entries until this many seconds after market open (0 = off).
        #[arg(long, default_value = "0")]
        min_secs_from_open: u32,
        /// Decision-quality gate: min seconds the belief held its side (0 = off).
        #[arg(long, default_value = "0.0")]
        min_belief_dwell_s: f64,
        /// Vol-responsive sizing reference (bps): clip = notional *
        /// clamp(sigma_bar_bps/ref, lo, hi). 0 = off (flat).
        #[arg(long, default_value = "0.0")]
        vol_sizing_ref_bps: f64,
        #[arg(long, default_value = "0.5")]
        vol_sizing_lo: f64,
        #[arg(long, default_value = "2.0")]
        vol_sizing_hi: f64,
        /// Basis-momentum tilt multipliers (agree/disagree); 1.0/1.0 = off.
        #[arg(long, default_value = "1.0")]
        basis_mom_agree: f64,
        #[arg(long, default_value = "1.0")]
        basis_mom_disagree: f64,
        /// Capture stress: fraction of displayed depth available to us.
        #[arg(long, default_value = "1.0")]
        depth_capture_frac: f64,
        /// Race stress: always lose the touch; fills start one level deeper.
        #[arg(long)]
        skip_touch_level: bool,
        #[arg(long, default_value = "1000")]
        decision_dt_ms: u64,
        #[arg(long, default_value = "10")]
        stop_before_close_s: u32,
        /// Entry window: permit entries only when time-to-close <= this many
        /// seconds (0 = disabled). stop_before_close_s stays the inner bound.
        #[arg(long, default_value = "0")]
        enter_within_close_s: u32,
        /// Max laddered clip entries per market.
        #[arg(long, default_value = "1")]
        max_clips: u32,
        /// Skip entries when realized vol (bps/bar) exceeds this (0 = off).
        #[arg(long, default_value = "0.0")]
        max_entry_sigma_bps: f64,
        /// Vol floor: skip when sigma_bar_bps is below this (shadow-final: 3.0).
        #[arg(long, default_value = "0.0")]
        min_entry_sigma_bps: f64,
        /// Skip UTC-Saturday entries (shadow-final behaviour).
        #[arg(long)]
        skip_saturday: bool,
        /// Post-entry selldown stop (hold mode only): sell taker when the
        /// entry side's ask prints at-or-below fill - eps. Negative = off.
        #[arg(long, default_value = "-1.0", allow_hyphen_values = true)]
        selldown_stop_eps: f64,
        /// Pre-entry stability gate: enter only if over the trailing S
        /// seconds the entry side's ask never printed below (current ask -
        /// --entry-stability-eps). 0 = off (parity).
        #[arg(long, default_value = "0")]
        entry_stability_s: u32,
        /// Tolerance for the stability gate's trailing-min comparison.
        #[arg(long, default_value = "0.005")]
        entry_stability_eps: f64,
        /// Entry fills walk depth only while each level retains this much
        /// edge vs the belief (0 = unconditional; live limit = belief-floor).
        #[arg(long, default_value = "0.0")]
        min_marginal_edge: f64,
        /// Minimum ms between clip entries.
        #[arg(long, default_value = "5000")]
        clip_cooldown_ms: u64,
        /// Event-based re-entry: after an entry, block further entries until
        /// both sides' edges drop below this (the dislocation closed); the
        /// next threshold crossing is then a fresh event (0 = disabled).
        #[arg(long, default_value = "0.0")]
        rearm_edge: f64,
        /// Maker entry study: rest a bid at (side ask - this offset) instead
        /// of taking the ask; fills only when the side ask later trades
        /// at-or-below the level before the stop_before_close deadline (zero
        /// fee, hold to resolution, one resting order per market). Negative
        /// disables (taker parity).
        #[arg(long, default_value = "-1.0", allow_hyphen_values = true)]
        maker_entry_offset: f64,
        /// Exit at the book N seconds after fill (0 = hold to resolution).
        #[arg(long, default_value = "0")]
        exit_after_s: u32,
        /// Passive exit study: rest an ask at the side mid at the exit
        /// horizon instead of crossing the spread. P&L uses the exact
        /// conditional fill; the optimistic always-fills bound is recorded
        /// per trade in --trades-out.
        #[arg(long)]
        exit_at_mid: bool,
        /// Hybrid passive exit: rest at the side mid at the exit horizon and
        /// convert to a spread-crossing exit after this many seconds unfilled
        /// (0 = disabled = champion crossing exit).
        #[arg(long, default_value = "0")]
        passive_exit_timeout_s: u32,
        /// Pair completion: buy the opposite token when its ask locks at
        /// least this margin against leg 1's cost (0 disables).
        #[arg(long, default_value = "0")]
        pair_completion_margin: f64,
        /// Pair-lock loss hedge (research knob): with a held net position on
        /// side A, buy the opposite side for exactly the net exposed shares
        /// once avg_cost_A + opposite ask <= 1 - margin. One hedge per
        /// market; exempt from entry gates. 0 disables (parity).
        #[arg(long, default_value = "0.0")]
        pair_lock_margin: f64,
        /// Cut-loser stop (research knob): with a held net position on side
        /// A at avg cost c, sell the net exposed shares at the bid once the
        /// side's bid drops below this fraction of c. Once per market; first
        /// trigger vs the pair lock wins. 0 disables (parity).
        #[arg(long, default_value = "0.0")]
        cut_loser_p: f64,
        /// Skip entries in calm_low_vol regime.
        #[arg(long)]
        skip_calm: bool,
        /// Take entries ONLY in calm_low_vol windows (calm-regime
        /// strategy exploration).
        #[arg(long)]
        only_calm: bool,
        /// Directional mode: enter only when the book already agrees with
        /// the belief (default is the fade: enter on disagreement).
        #[arg(long)]
        aligned_mode: bool,
        /// Aligned mode: minimum side mid for book agreement.
        #[arg(long, default_value = "0.55")]
        align_min_mid: f64,
        /// Buy the opposite tail as a convexity hedge when its ask <= this
        /// (0 disables).
        #[arg(long, default_value = "0")]
        tail_max_price: f64,
        /// Tail hedge notional as a fraction of the clip.
        #[arg(long, default_value = "0.25")]
        tail_frac: f64,
        /// Skip when decision-time regime is expanded_mixed.
        #[arg(long)]
        skip_expanded_mixed: bool,
        /// Skip entries when decision-time regime is expanded_high_flip.
        #[arg(long)]
        skip_expanded_high_flip: bool,
        /// Skip :00 favourites where model >> book (p_side>open-fav-p-min, ask<open-fav-ask-max).
        #[arg(long)]
        skip_open_fav_gap: bool,
        #[arg(long, default_value = "0.90")]
        open_fav_p_min: f64,
        #[arg(long, default_value = "0.60")]
        open_fav_ask_max: f64,
        #[arg(long, default_value = "5")]
        open_fav_secs: u32,
        /// Pause entries after this many consecutive resolved losses (0 = off).
        #[arg(long, default_value = "0")]
        pause_after_consec_losses: u32,
        /// On rearm clips, skip when entry ask exceeds this (0 = off).
        #[arg(long, default_value = "0.0")]
        max_rearm_entry_ask: f64,
        /// Skip when spot return over this lookback (seconds) disagrees with entry side (0 = off).
        #[arg(long, default_value = "0")]
        skip_spot_misalign_s: u32,
        /// Skip when 60/300/600/900s spot all disagree with entry side.
        #[arg(long)]
        skip_spot_against_all: bool,
        /// Infer missing outcome labels from the final tape mid.
        #[arg(long)]
        infer_outcome: bool,
        #[arg(long, default_value = "1800")]
        vol_lookback_s: u32,
        /// Vol estimator feeding the fair value:
        /// realized | ewma | blend | seasonal | jump_robust.
        #[arg(long, default_value = "realized")]
        vol_estimator: String,
        /// EWMA half-life in seconds (--vol-estimator ewma only).
        #[arg(long, default_value = "1200")]
        ewma_halflife_s: f64,
        /// Fast realized window in seconds (--vol-estimator blend only).
        #[arg(long, default_value = "300")]
        vol_fast_window_s: u32,
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
        /// Trained continuation model JSON (scripts/dir_train.py); gates and
        /// prices Aligned entries.
        #[arg(long)]
        dir_model: Option<PathBuf>,
        /// Dump directional continuation samples from the training pass.
        #[arg(long)]
        dir_samples_out: Option<PathBuf>,
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
        /// Official open prints JSONL (slug -> open_price) overriding the
        /// Binance-open strike proxy.
        #[arg(long)]
        strikes: Option<PathBuf>,
        /// Load the perp complex for this symbol (e.g. BTCUSDT) into the
        /// belief's ExoState.
        #[arg(long)]
        perp_symbol: Option<String>,
        /// Cache root for perp parquets (default: data/cache).
        #[arg(long)]
        perp_cache_dir: Option<PathBuf>,
        /// Cross-asset reference spot symbol (e.g. BTCUSDT for ETH markets).
        #[arg(long)]
        xasset_symbol: Option<String>,
        /// Weight on the reference asset's trailing 60s return as an extra
        /// drift term (0 disables).
        #[arg(long, default_value = "0")]
        xasset_weight: f64,
        /// Weight on the basis-adjusted perp last in the effective-spot
        /// blend (0 disables; requires --perp-symbol).
        #[arg(long, default_value = "0")]
        perp_price_weight: f64,
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
    /// Run a walk-forward backtest over many markets.
    WalkForward {
        /// JSONL of `MarketHandle` rows from `discover-day`.
        #[arg(long)]
        markets: PathBuf,
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
        /// Binance USD-M futures symbol. NOTE: the walk-forward perp path is
        /// currently INERT. No strategy consumes perp data, so nothing reads
        /// what this would load. The loaders are retained as plumbing for a
        /// future perp-weighted belief; setting this today changes nothing.
        #[arg(long)]
        perp_symbol: Option<String>,
        /// Cache root for perp parquets (default: --local-cache-dir or data/cache).
        #[arg(long)]
        perp_cache_dir: Option<PathBuf>,
        /// Comma-separated active strategy IDs.
        ///
        /// There are no deployable strategies. `noop` (the default) emits no
        /// orders and exercises the loader and accounting path; `fixture` is
        /// test-only plumbing and needs `--allow-fixture`.
        #[arg(long, default_value = "noop")]
        strategies: String,
        /// Permit `--strategies fixture`, the deterministic test-only strategy
        /// that anchors the golden replay gate. It is not a deployable
        /// strategy; without this flag the id is rejected.
        #[arg(long, default_value_t = false)]
        allow_fixture: bool,
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
        /// Permit runs below the truthful taker-latency floor (750ms). The run
        /// proceeds but is watermarked `"FANTASY"` in the summary and the
        /// `FANTASY-` prefix is applied to output filenames.
        #[arg(long, default_value_t = false)]
        fantasy: bool,
        /// Taker fee curve rate for `rate * p * (1-p)` per share, charged on
        /// every taker fill (Polymarket's crypto taker fee shape). Default
        /// 0.07 is the validated venue rate; a rate below it requires
        /// --fantasy and watermarks the run.
        #[arg(long, default_value = "0.07")]
        fee_curve_rate: f64,
        /// Run the walk-forward N times at deterministically seeded perturbed
        /// taker latencies and report the P&L spread (p10/p50/p90) instead of a
        /// point estimate. 0 (default) runs once. Runs are serial: a
        /// 288-market day at N=5 is ~5x the single-run wall time.
        #[arg(long, default_value_t = 0)]
        jitter: usize,
        /// Half-width (ms) of the uniform latency jitter band around
        /// --taker-latency-ms. Each draw is clamped up to the 750ms truthful
        /// floor unless --fantasy is granted.
        #[arg(long, default_value_t = 250)]
        jitter_latency_spread_ms: u64,
        /// Seed for the jitter PRNG (same seed reproduces the same latencies).
        #[arg(long, default_value_t = 42)]
        jitter_seed: u64,
        /// Optional multi-window validation label (e.g. `feb2026`). Recorded in
        /// the summary; the validation status is derived from it plus
        /// --validated-set-complete.
        #[arg(long)]
        window_label: Option<String>,
        /// Assert that the full canonical validated window set ran. Only the
        /// multi-window driver script sets this; a single run can never claim
        /// validated status by itself.
        #[arg(long, default_value_t = false)]
        validated_set_complete: bool,
        /// Bankroll (USD) for the sizing-realism block. When set, the summary
        /// carries a haircut P&L (winners x0.82, losers full), the smallest
        /// clip the sizing implies, and the 5-share floor/ruin flag.
        #[arg(long)]
        bankroll: Option<f64>,
        /// Source of the strike (price-to-beat) used for outcome resolution.
        /// `binance_proxy` (default) uses the Binance open price; `official`
        /// uses the Polymarket resolution price, which lives on a different
        /// basis and is rejected with a Binance spot tape unless
        /// `--allow-mixed-basis` is also set.
        #[arg(long, default_value = "binance_proxy")]
        strike_source: String,
        /// Permit a Binance spot tape with an Official strike. The run
        /// proceeds but is watermarked `"MIXED-BASIS"` in the summary.
        #[arg(long, default_value_t = false)]
        allow_mixed_basis: bool,
        /// Force per-era breakdown and label-vs-model disagreement reporting
        /// even when every market carries an outcome label.
        #[arg(long, default_value_t = false)]
        era_diagnostics: bool,
    },
    /// Summarize a walk-forward `markets.jsonl` result file.
    SummarizeMarkets {
        /// Path to the per-market JSONL emitted by `walk-forward --out-markets`.
        #[arg(long)]
        markets: PathBuf,
        /// Strategy key inside `per_strategy`.
        #[arg(long, default_value = "noop")]
        strategy: String,
        /// Optional JSON output path for the computed summary.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// The ONLY Cmd::Shadow -> ShadowArgs mapping. main() and the clap/shell
/// parity tests share it, so a parsed CLI in tests is exactly what a real
/// invocation would run.
fn shadow_args_from_cmd(cmd: Cmd) -> Option<shadow::ShadowArgs> {
    let Cmd::Shadow {
        slug_prefix,
        strategy,
        vol_lookback_s,
        exit_after_s,
        latency_probe_ms,
        out_dir,
        perp_price_weight,
        vol_estimator,
        ewma_halflife_s,
        decide_interval_ms,
        decide_on_event,
    } = cmd
    else {
        return None;
    };
    Some(shadow::ShadowArgs {
        slug_prefix,
        strategy,
        vol_lookback_s,
        exit_after_s,
        latency_probe_ms,
        out_dir,
        perp_price_weight,
        vol_estimator,
        ewma_halflife_s,
        decide_interval_ms,
        decide_on_event,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    match cli.cmd {
        cmd @ Cmd::Shadow { .. } => {
            let args = shadow_args_from_cmd(cmd).expect("Cmd::Shadow variant");
            shadow::run_shadow(args).await
        }
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
            fee_curve_rate,
            fee_aware_exit,
            fee_exit_margin,
            notional_usdc,
            kelly_sizing,
            min_p_side,
            max_p_side,
            min_entry_ask,
            max_entry_ask,
            min_secs_from_open,
            min_belief_dwell_s,
            vol_sizing_ref_bps,
            vol_sizing_lo,
            vol_sizing_hi,
            basis_mom_agree,
            basis_mom_disagree,
            depth_capture_frac,
            skip_touch_level,
            decision_dt_ms,
            stop_before_close_s,
            enter_within_close_s,
            max_entry_sigma_bps,
            min_entry_sigma_bps,
            skip_saturday,
            selldown_stop_eps,
            entry_stability_s,
            entry_stability_eps,
            min_marginal_edge,
            max_clips,
            clip_cooldown_ms,
            rearm_edge,
            maker_entry_offset,
            exit_after_s,
            exit_at_mid,
            passive_exit_timeout_s,
            pair_completion_margin,
            pair_lock_margin,
            cut_loser_p,
            skip_calm,
            only_calm,
            aligned_mode,
            align_min_mid,
            tail_max_price,
            tail_frac,
            skip_expanded_mixed,
            skip_expanded_high_flip,
            skip_open_fav_gap,
            open_fav_p_min,
            open_fav_ask_max,
            open_fav_secs,
            pause_after_consec_losses,
            max_rearm_entry_ask,
            skip_spot_misalign_s,
            skip_spot_against_all,
            infer_outcome,
            vol_lookback_s,
            vol_estimator,
            ewma_halflife_s,
            vol_fast_window_s,
            momentum_lookback_s,
            momentum_weight,
            out_json,
            calibrate_split,
            calibrator_out,
            calibrator_in,
            dir_model,
            dir_samples_out,
            trades_out,
            down_assets,
            tick_cache_dir,
            strikes,
            perp_symbol,
            perp_cache_dir,
            xasset_symbol,
            xasset_weight,
            perp_price_weight,
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
                    fee_curve_rate,
                    fee_aware_exit,
                    fee_exit_margin,
                    notional_usdc,
                    kelly_sizing,
                    min_p_side,
                    max_p_side,
                    min_entry_ask,
                    max_entry_ask,
                    min_secs_from_open,
                    min_belief_dwell_s,
                    vol_sizing_ref_bps,
                    vol_sizing_lo,
                    vol_sizing_hi,
                    basis_mom_agree,
                    basis_mom_disagree,
                    depth_capture_frac,
                    skip_touch_level,
                    decision_dt_ms,
                    stop_before_close_s,
                    enter_within_close_s,
                    max_entry_sigma_bps,
                    min_entry_sigma_bps,
                    skip_saturday,
                    selldown_stop_eps,
                    entry_stability_s,
                    stability_eps: entry_stability_eps,
                    min_marginal_edge,
                    max_clips,
                    clip_cooldown_ms,
                    rearm_edge,
                    maker_entry_offset,
                    exit_after_s,
                    exit_at_mid,
                    passive_exit_timeout_s,
                    pair_completion_margin,
                    pair_lock_margin,
                    cut_loser_p,
                    skip_calm,
                    only_calm,
                    aligned_mode,
                    align_min_mid,
                    tail_max_price,
                    tail_frac,
                    skip_expanded_mixed,
                    skip_expanded_high_flip,
                    skip_open_fav_gap,
                    open_fav_p_min,
                    open_fav_ask_max,
                    open_fav_secs,
                    pause_after_consec_losses,
                    max_rearm_entry_ask,
                    skip_spot_misalign_s,
                    skip_spot_against_all,
                    infer_outcome,
                    vol_lookback_s,
                    vol_estimator,
                    ewma_halflife_s,
                    vol_fast_window_s,
                    momentum_lookback_s,
                    momentum_weight,
                    out_json,
                    calibrate_split,
                    calibrator_out,
                    calibrator_in,
                    dir_model,
                    dir_samples_out,
                    trades_out,
                    down_assets,
                    tick_cache_dir,
                    strikes,
                    perp_symbol,
                    perp_cache_dir,
                    xasset_symbol,
                    xasset_weight,
                    perp_price_weight,
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
        Cmd::WalkForward {
            markets,
            skip_markets,
            max_markets,
            starting_cash,
            kelly_fraction,
            max_clip_usdc,
            max_order_clip_multiplier,
            max_per_market_exposure_usdc,
            max_per_market_exposure_frac,
            spot_symbol,
            perp_symbol,
            perp_cache_dir,
            strategies,
            allow_fixture,
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
            fantasy,
            fee_curve_rate,
            jitter,
            jitter_latency_spread_ms,
            jitter_seed,
            window_label,
            validated_set_complete,
            bankroll,
            strike_source,
            allow_mixed_basis,
            era_diagnostics,
        } => {
            walk_forward(
                markets,
                skip_markets,
                max_markets,
                starting_cash,
                kelly_fraction,
                max_clip_usdc,
                max_order_clip_multiplier,
                max_per_market_exposure_usdc,
            max_per_market_exposure_frac,
            spot_symbol,
            perp_symbol,
            perp_cache_dir,
            strategies,
            allow_fixture,
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
                fantasy,
                fee_curve_rate,
                jitter,
                jitter_latency_spread_ms,
                jitter_seed,
                window_label,
                validated_set_complete,
                bankroll,
                strike_source,
                allow_mixed_basis,
                era_diagnostics,
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
    skip_markets: usize,
    max_markets: usize,
    starting_cash: f64,
    kelly_fraction: f64,
    max_clip_usdc: f64,
    max_order_clip_multiplier: f64,
    max_per_market_exposure_usdc: f64,
    max_per_market_exposure_frac: Option<f64>,
    spot_symbol: String,
    perp_symbol: Option<String>,
    perp_cache_dir: Option<PathBuf>,
    strategies_csv: String,
    allow_fixture: bool,
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
    fantasy: bool,
    fee_curve_rate: f64,
    jitter: usize,
    jitter_latency_spread_ms: u64,
    jitter_seed: u64,
    window_label: Option<String>,
    validated_set_complete: bool,
    bankroll: Option<f64>,
    strike_source: String,
    allow_mixed_basis: bool,
    era_diagnostics: bool,
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
             so runs for different strategies do not mix inputs."
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
        pm_backtest::accounting::validate_outcome_labels(&markets)?;
    }

    // Truthful-latency floor, era-aware: reject runs modeling less total
    // latency than the truthful floor or the era's venue taker delay, unless
    // --fantasy grants a watermarked override. The venue-delay history is not
    // monotonic (0ms, then 250ms, then 50ms), so validate against whichever
    // market close in the run carries the LARGEST venue delay.
    let strictest_close = markets
        .iter()
        .map(|m| m.close_ts)
        .max_by_key(|ts| pm_backtest::settlement::venue_taker_delay_ms(*ts))
        .unwrap_or(0);
    let latency_watermark = pm_backtest::validate::validate_latency_for_era(
        taker_latency_ms,
        fantasy,
        strictest_close,
    )?;
    // Taker fee curve rate floor: a rate below the canonical venue rate
    // (0.07) is cheaper-than-real and rejected unless --fantasy watermarks it.
    let fee_rate_watermark =
        pm_backtest::validate::validate_fee_rate(fee_curve_rate, fantasy)?;
    // Mixed price-basis refusal: a Binance spot tape against an Official strike
    // compares prices on different bases (see the basis doctrine in
    // docs/PROD.md) and is rejected unless --allow-mixed-basis watermarks it.
    let strike_source = match strike_source.as_str() {
        "binance_proxy" => StrikeSource::BinanceProxy,
        "official" => StrikeSource::Official,
        other => {
            return Err(anyhow!(
                "unknown --strike-source {other} (expected binance_proxy or official)"
            ));
        }
    };
    let basis_watermark =
        pm_backtest::config::validate_basis(SpotSource::Binance, strike_source, allow_mixed_basis)?;
    // Dedup: --fantasy alone can make both the latency and fee-rate
    // validators return "FANTASY" independently; collapse consecutive
    // duplicates so the watermark reads "FANTASY" once, not "FANTASY,FANTASY".
    let mut watermark_parts: Vec<String> = [latency_watermark, fee_rate_watermark, basis_watermark]
        .into_iter()
        .flatten()
        .collect();
    watermark_parts.dedup();
    let watermark = if watermark_parts.is_empty() {
        None
    } else {
        Some(watermark_parts.join(","))
    };
    // Fantasy runs watermark their output filenames so they are unmistakable.
    let out_markets = out_markets.map(|p| apply_fantasy_prefix(&p, fantasy));
    let out_summary = out_summary.map(|p| apply_fantasy_prefix(&p, fantasy));

    let strategies = parse_strategies(&strategies_csv, allow_fixture)?;

    let store = if let Some(ref dir) = local_cache_dir {
        tracing::info!(?dir, "using local cache");
        TelonexStore::try_new_local(dir.clone())?
    } else {
        let cfg = TelonexStoreConfig::from_env()?;
        TelonexStore::try_new(&cfg)?
    };

    let wf_cfg = WalkForwardConfig {
        starting_cash_usdc: starting_cash,
        kelly_fraction,
        max_clip_usdc,
        max_order_clip_multiplier,
        max_per_market_exposure_usdc,
        max_per_market_exposure_frac,
        spot_symbol,
        perp_symbol,
        perp_cache_dir: perp_cache_dir.or(local_cache_dir.clone()),
        strategies,
        max_concurrent_fetches,
        replay_sample_ms,
        taker_latency_ms,
        fantasy,
        jitter,
        jitter_latency_spread_ms,
        jitter_seed,
        spot_source: SpotSource::Binance,
        strike_source,
        allow_mixed_basis,
        era_diagnostics,
        window_label: window_label.clone(),
        replay_event_cache_dir,
        load_pm_trades,
        use_outcome_label,
        // Fee/rebate accounting is unconditional: fills.rs always applies
        // `taker_fee_bps` and `taker_fee_curve_rate` on taker fills and
        // `maker_rebate_bps` on maker fills. There is no fees_enabled/off-
        // switch bypassing the application code; the values below are the
        // only fee knobs (audit: no skip path found).
        maker_rebate_bps: 10.0,
        taker_fee_bps: 0.0,
        taker_fee_curve_rate: fee_curve_rate,
        portfolio_mode,
        clip_fraction_of_equity,
        clip_drawdown_soft_pct,
        clip_drawdown_hard_pct,
        clip_drawdown_min_multiplier,
        clip_session_drawdown_soft_pct,
        clip_session_drawdown_hard_pct,
        clip_session_drawdown_min_multiplier,
        daily_loss_cap_pct,
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
        ..WalkForwardConfig::default()
    };

    let active_strats: Vec<&str> = wf_cfg.strategies.iter().map(|s| s.name()).collect();
    let effective_config = serde_json::json!({
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
        "forbid_meta_training": wf_cfg.forbid_meta_training,
    });
    tracing::info!(
        strategies = ?active_strats,
        portfolio_mode = wf_cfg.portfolio_mode,
        clip_fraction_of_equity = ?wf_cfg.clip_fraction_of_equity,
        daily_loss_cap_pct = wf_cfg.daily_loss_cap_pct,
        clip_drawdown_hard_pct = wf_cfg.clip_drawdown_hard_pct,
        replay_sample_ms = wf_cfg.replay_sample_ms,
        "effective walk-forward config"
    );

    tracing::info!(markets = markets.len(), "starting walk-forward");
    let started = Instant::now();
    let (results, mut summary) = run_walkforward(&store, &markets, &wf_cfg).await?;
    let elapsed = started.elapsed().as_secs_f64();
    tracing::info!(elapsed_s = elapsed, "walk-forward complete");

    // Stamp the fantasy watermark onto the final summary (truthful runs: None,
    // omitted from JSON via skip_serializing_if). Matches the value returned
    // by validate_latency so the on-disk watermark is authoritative.
    summary.watermark = watermark.clone();

    // Multi-window validation labeling and sizing-realism block. Validation is
    // UNVALIDATED unless the window label is canonical AND the caller asserted
    // the full set ran; a single run can never claim validated status alone.
    summary.window_label = window_label.clone();
    summary.validation = pm_backtest::scorecard::validation_label(
        window_label.as_deref(),
        validated_set_complete,
    );
    if let Some(bankroll_usd) = bankroll {
        let clip_fraction = wf_cfg.clip_fraction_of_equity.unwrap_or(0.01);
        summary.sizing = Some(pm_backtest::scorecard::sizing_realism(
            &results,
            &wf_cfg.strategies,
            bankroll_usd,
            clip_fraction,
        ));
    }

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

/// Prefix the FILENAME (not the directory) of an output path with `FANTASY-`
/// when fantasy mode is active, so watermarked runs are unmistakable on disk.
fn apply_fantasy_prefix(path: &std::path::Path, fantasy: bool) -> std::path::PathBuf {
    if !fantasy {
        return path.to_path_buf();
    }
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if file_name.is_empty() {
        return path.to_path_buf();
    }
    let prefixed = format!("FANTASY-{file_name}");
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(prefixed),
        _ => std::path::PathBuf::from(prefixed),
    }
}

fn parse_strategies(csv: &str, allow_fixture: bool) -> Result<Vec<StratId>> {
    let mut out = Vec::new();
    for token in csv.split(',').map(str::trim) {
        let id = StratId::from_name(token).ok_or_else(|| {
            anyhow!(
                "unknown strategy: {token}. supported strategies: {}",
                StratId::all_names().join(", ")
            )
        })?;
        if id.is_fixture() && !allow_fixture {
            return Err(anyhow!(
                "strategy {token} is test-only plumbing for the golden replay gate and is not deployable; pass --allow-fixture to run it"
            ));
        }
        out.push(id);
    }

    if out.is_empty() {
        return Err(anyhow!("no strategies specified"));
    }

    Ok(out)
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
    fn parse_strategies_rejects_unknown_names() {
        assert!(parse_strategies("reactive_directional", false).is_err());
    }

    #[test]
    fn parse_strategies_rejects_the_fixture_without_the_allow_flag() {
        let err = parse_strategies("fixture", false).unwrap_err().to_string();
        assert!(err.contains("--allow-fixture"), "unexpected error: {err}");
        assert!(parse_strategies("noop,fixture", false).is_err());
    }

    #[test]
    fn parse_strategies_accepts_the_fixture_behind_the_allow_flag() {
        assert_eq!(
            parse_strategies("fixture", true).unwrap(),
            vec![StratId::Fixture]
        );
    }

    #[test]
    fn parse_strategies_allow_fixture_does_not_widen_the_deployable_set() {
        assert!(parse_strategies("reactive_directional", true).is_err());
    }

    // CONFIG PARITY GATE (deep-review F1). The clap layer and the shell flag
    // file are both potential shadow configs; these tests pin BOTH to the Rust
    // canon (`default_shadow_args`). If one of these fails, fix the clap
    // default or the flags file, never the test.

    fn parse_shadow_args(argv: &[String]) -> shadow::ShadowArgs {
        // The Cmd enum is large enough that clap parsing overflows the
        // default 2MiB test-thread stack in debug builds; parse on a
        // dedicated thread with the main-thread-sized stack instead.
        let argv = argv.to_vec();
        std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let cli = Cli::try_parse_from(&argv)
                    .unwrap_or_else(|e| panic!("clap rejected {argv:?}: {e}"));
                shadow_args_from_cmd(cli.cmd).expect("shadow subcommand")
            })
            .expect("spawn parse thread")
            .join()
            .expect("parse thread panicked")
    }

    #[test]
    fn shadow_clap_defaults_equal_engine_canon() {
        // Minimal invocation: only the required arg. Every default the CLI
        // fills in must equal the library canon, field for field, with NO
        // whitelist. A default that must differ is a bug in the default.
        let argv: Vec<String> = ["pm-app", "shadow", "--out-dir", "shadow-final"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let args = parse_shadow_args(&argv);
        let canon = shadow::default_shadow_args(PathBuf::from("shadow-final"));
        assert_eq!(
            args, canon,
            "shadow clap defaults drifted from default_shadow_args; \
             align the clap default, do not whitelist"
        );
    }

    #[test]
    fn shadow_cli_no_longer_accepts_strategy_parameters() {
        // The fade's gate flags are gone with the strategy. A launcher still
        // passing one must fail loudly rather than be silently ignored.
        for dead in ["--edge-threshold=0.12", "--min-entry-ask=0.45", "--skip-saturday"] {
            let argv: Vec<String> = ["pm-app", "shadow", "--out-dir", "o", dead]
                .iter()
                .map(|s| s.to_string())
                .collect();
            let parsed = std::thread::Builder::new()
                .stack_size(16 * 1024 * 1024)
                .spawn(move || Cli::try_parse_from(&argv).is_ok())
                .expect("spawn parse thread")
                .join()
                .expect("parse thread panicked");
            assert!(!parsed, "{dead} must be rejected, not ignored");
        }
    }

    // Shell SSOT parsing: extract the exec argv from a foreground launcher
    // plus its sourced flags array, substituting ${VAR:-default} with the
    // default. Panics loudly on any construct it does not understand, so a
    // creative launcher edit fails the gate rather than slipping past it.

    fn read_ops_script(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/ops")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Replace every `${NAME:-default}` with `default`; leave other text.
    fn expand_param_defaults(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(start) = rest.find("${") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find('}') else {
                panic!("unterminated ${{ in shell fragment: {s}");
            };
            let inner = &after[..end];
            match inner.split_once(":-") {
                Some((_, default)) => out.push_str(default),
                None => panic!("plain ${{{inner}}} has no :-default; teach the parity test"),
            }
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        out
    }

    /// Scan `NAME="value"` assignments (defaults resolved) into a map.
    fn shell_assignments(script: &str) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        for line in script.lines() {
            let line = line.trim();
            let Some((name, value)) = line.split_once('=') else {
                continue;
            };
            if name.is_empty()
                || !name.chars().all(|c| c.is_ascii_uppercase() || c == '_')
                || value.starts_with("\"$(")
                || value.starts_with("$(")
                || value.starts_with('(')
            {
                continue;
            }
            let value = value.trim_matches('"');
            vars.insert(name.to_string(), expand_param_defaults(value));
        }
        vars
    }

    /// Tokens of a `NAME=( ... )` flags array, comments stripped.
    fn shell_flags_array(script: &str, array_name: &str) -> Vec<String> {
        let open = format!("{array_name}=(");
        let start = script
            .find(&open)
            .unwrap_or_else(|| panic!("flags array {array_name} not found"));
        let body = &script[start + open.len()..];
        let end = body.find("\n)").expect("flags array not closed");
        let mut out = Vec::new();
        for line in body[..end].lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            for tok in line.split_whitespace() {
                out.push(tok.trim_matches('"').to_string());
            }
        }
        assert!(!out.is_empty(), "flags array {array_name} parsed empty");
        out
    }

    /// The full argv a foreground launcher would exec: base flags from the
    /// `exec "$BIN" shadow \` block with env defaults substituted, plus the
    /// sourced flags array spliced where `"${ARRAY[@]}"` appears.
    fn launcher_argv(
        foreground: &str,
        flags_file: &str,
        array_name: &str,
    ) -> Vec<String> {
        let fg = read_ops_script(foreground);
        let flags = shell_flags_array(&read_ops_script(flags_file), array_name);
        let vars = shell_assignments(&fg);

        let mut block = String::new();
        let mut in_exec = false;
        for line in fg.lines() {
            let t = line.trim();
            if !in_exec {
                if !t.starts_with("exec ") {
                    continue;
                }
                in_exec = true;
            }
            let cont = t.ends_with('\\');
            block.push_str(t.trim_end_matches('\\'));
            block.push(' ');
            if !cont {
                break;
            }
        }
        assert!(in_exec, "{foreground}: no exec block found");

        let raw: Vec<&str> = block.split_whitespace().collect();
        assert_eq!(raw[0], "exec", "{foreground}: exec block malformed");
        assert_eq!(
            raw[2], "shadow",
            "{foreground}: launcher no longer runs the shadow subcommand"
        );
        let splice_marker = format!("${{{array_name}[@]}}");
        let mut argv = vec!["pm-app".to_string(), "shadow".to_string()];
        for tok in &raw[3..] {
            let tok = tok.trim_matches('"');
            if tok == splice_marker {
                argv.extend(flags.iter().cloned());
                continue;
            }
            let expanded = expand_param_defaults(tok);
            if let Some(name) = expanded.strip_prefix('$') {
                let value = vars.get(name).unwrap_or_else(|| {
                    panic!("{foreground}: unresolved shell var ${name} in exec block")
                });
                argv.push(value.clone());
            } else {
                argv.push(expanded);
            }
        }
        argv
    }

    #[test]
    fn shadow_launcher_flags_equal_engine_canon() {
        let argv = launcher_argv(
            "shadow_final_foreground.sh",
            "shadow_flags.sh",
            "SHADOW_FLAGS",
        );
        let args = parse_shadow_args(&argv);
        let expected = shadow::default_shadow_args(args.out_dir.clone());
        assert_eq!(
            args, expected,
            "shadow_final_foreground.sh + shadow_flags.sh no longer render \
             default_shadow_args; update the flags file AND the Rust canon \
             together"
        );
    }
}
