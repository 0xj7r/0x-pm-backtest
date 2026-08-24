//! Strategy trait + signal stack + reference strategies.
//!
//! The Nautilus-native runtime integration lives in `pm-app`; this crate stays
//! Nautilus-free so signal math can be unit-tested in milliseconds without
//! pulling the engine.

#![forbid(unsafe_code)]

pub mod exo_fade;
pub mod fixture;
pub mod regime;
#[path = "archive/trivial.rs"]
pub mod trivial;

use pm_model::ModelOutput;
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

/// Per-event context handed to a strategy alongside the tape.
///
/// Every field here is read by at least one strategy. Fields that only the
/// runner wrote and nothing consumed have been removed: a field nobody reads
/// is a claim about the contract that the code does not back up, and it costs
/// a plumbing site in every construction path. Adding one back is cheap (a
/// field plus one line in the runner's `Ctx` literal), and should happen in
/// the same change as the strategy that reads it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ctx {
    pub events_seen: u64,
    pub cash_usdc: f64,
    /// Observed market volatility so far as `max(yes_mid) - min(yes_mid)`.
    /// This is live-safe: it only includes ticks already seen by the runner.
    pub market_yes_range_so_far: f32,
    /// Live-safe spot regime snapshot at this event. These distinguish clean
    /// directional expansion from chop with the same observed market range.
    pub regime_path_efficiency: f32,
    pub regime_reversal_pressure: f32,
    pub regime_sign_flip_rate: f32,
    pub regime_realized_vol_180s_bps: f32,
    /// Market resolution time in ns since epoch (UTC). Strategies use this
    /// to compute time-to-close and gate early/mid/late behaviour.
    pub market_close_ns: i64,
    /// Real NO-leg top of book ask (from the opposing ladder, NOT synthetic
    /// 1-yes). 0.0 when no NO book is available.
    pub no_ask: f32,
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

pub use exo_fade::{ExoFadeConfig, ExoFadeGateStats, ExoFadeStrategy};
pub use fixture::ThresholdFadeStrategy;
pub use trivial::NoopStrategy;
