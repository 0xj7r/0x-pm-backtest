//! Strategy trait + signal stack + reference strategies.
//!
//! The Nautilus-native runtime integration lives in `pm-app`; this crate stays
//! Nautilus-free so signal math can be unit-tested in milliseconds without
//! pulling the engine.

#![forbid(unsafe_code)]

pub mod back_to_explore;
pub mod convex;
pub mod bonereaper_v2;
pub mod paired_mm;
pub mod regime;
pub mod signals;
#[path = "archive/spot_momentum.rs"]
pub mod spot_momentum;
#[path = "archive/trivial.rs"]
pub mod trivial;

use pm_model::{ModelAttribution, ModelOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    BuyYes,
    SellYes,
    BuyNo,
    SellNo,
}

#[derive(Debug, Clone, Copy)]
pub struct OrderRequest {
    pub side: Side,
    pub shares: f64,
    /// Maximum book levels a taker order may sweep. Maker orders ignore this.
    pub max_depth: usize,
    /// Optional limit price (in YES terms; for NO orders, the runner inverts).
    /// `None` means market order against the opposite top of book.
    pub limit_price: Option<f32>,
    /// Tag for attribution (kept tiny — &'static so strategies don't allocate).
    pub tag: &'static str,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Ctx {
    pub events_seen: u64,
    pub yes_shares: f64,
    pub no_shares: f64,
    pub cash_usdc: f64,
    /// Observed market volatility so far as `max(yes_mid) - min(yes_mid)`.
    /// This is live-safe: it only includes ticks already seen by the runner.
    pub market_yes_range_so_far: f32,
    /// Live-safe spot regime snapshot at this event. These distinguish clean
    /// directional expansion from chop with the same observed market range.
    pub regime_whipsaw_score: f32,
    pub regime_path_efficiency: f32,
    pub regime_reversal_pressure: f32,
    pub regime_sign_flip_rate: f32,
    pub regime_realized_vol_180s_bps: f32,
    /// Mean full-market YES-mid range over already closed prior BTC 5m markets.
    /// These fields are live-safe in portfolio replay because they never include
    /// the current market.
    pub prior_market_range_1d: f32,
    pub prior_market_range_3d: f32,
    pub prior_market_range_7d: f32,
    /// Canonical 4-score model output for this event, produced by the shared
    /// model state before the strategy hook. Strategies can use this for
    /// ML-gated lanes while the runner still owns attribution and parity.
    pub model_output: Option<ModelOutput>,
    /// Replay-safe feature attribution from the same canonical model evaluation.
    /// Specialist strategies consume this to match offline decision-log training
    /// without recomputing a parallel feature stack.
    pub model_attribution: Option<ModelAttribution>,
    /// Market resolution time in ns since epoch (UTC). Strategies use this
    /// to compute time-to-close and gate early/mid/late behaviour.
    pub market_close_ns: i64,

    /// Real NO-leg top of book (from the opposing ladder, NOT synthetic 1-yes).
    /// 0.0 when no NO book is available (Phase-1 tests / pre-first-NO).
    pub no_bid: f32,
    pub no_ask: f32,
    pub no_mid: f32,

    // === Cross-market ladder exposure (for BackToExplore and similar ladder strategies) ===
    // These are populated in portfolio replay so strategies can see net exposure
    // across all currently open windows for the same asset.
    pub btc_net_exposure_shares: f64,
    pub eth_net_exposure_shares: f64,

    /// Daily loss cap info (populated in portfolio mode for strategies that
    /// want to avoid adding risk on bad days, while still allowing repair/pair
    /// hedges -- better than blunt runner stop for two-sided strats).
    pub daily_start_cash_usdc: f64,
    pub daily_loss_cap_pct: f64,

    /// Current realized daily loss (0.0 at open or non-portfolio). 0.05 means 5% down from
    /// that day's starting equity. BackToExplore etc use this to adapt: boost pair/repair
    /// (two-sided), cut size_mult, lower target_net on bad days. This is the "better than
    /// hard clip=0" approach: signal-driven risk response inside the strat.
    pub current_daily_loss_pct: f64,
}

#[derive(Debug, Default, Clone)]
pub struct StrategyOutput {
    pub orders: Vec<OrderRequest>,
}

impl StrategyOutput {
    pub fn hold() -> Self {
        Self::default()
    }
    pub fn one(req: OrderRequest) -> Self {
        Self { orders: vec![req] }
    }
}

/// Per-event strategy hook. `spot` is the underlying spot tape (e.g. BTC/USD
/// from Binance). `trades` is the per-market Polymarket trade tape (aggressor
/// flow). Either may be empty if not loaded; strategies should ignore unused
/// inputs.
pub trait Strategy {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        trades: &TradeHistory,
    ) -> StrategyOutput;

    /// Optional per-event model diagnostics. Return model scores when available
    /// for attribution and offline analysis.
    fn on_event_scored(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        trades: &TradeHistory,
    ) -> (StrategyOutput, Option<ModelOutput>) {
        (self.on_event(event, ctx, spot, trades), None)
    }

    fn on_market_resolved(&mut self, _market_mid: f32, _resolved_yes: bool) {}
}

pub use back_to_explore::{BackToExploreConfig, BackToExploreTaker};
pub use convex::{ConvexBookConfig, ConvexBookStrategy};
pub use convex::position::PositionConfig;
pub use bonereaper_v2::{BonereaperV2, BonereaperV2Config};
pub use paired_mm::{PairedMmDense, PairedMmDenseConfig};
pub use trivial::NoopStrategy;
