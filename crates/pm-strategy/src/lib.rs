//! The `Strategy` trait and the two strategies that implement it.
//!
//! Both are plumbing: `NoopStrategy` emits nothing, and `ThresholdFadeStrategy`
//! is the deterministic test-only fixture that anchors the golden replay gate.
//! There is deliberately no deployable strategy here. This crate stays free of
//! engine and runtime dependencies so strategy logic can be unit-tested in
//! milliseconds.

#![forbid(unsafe_code)]

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
/// These are the runner-derived facts a strategy cannot recover from
/// `ReplayEvent` alone: how far into the market it is, what it holds in cash,
/// how the market and the underlying have moved so far, and when the market
/// resolves. Only `events_seen` currently has a reader, because the only
/// strategies left are the noop and the golden fixture; the rest survive
/// deliberately, as the contract a future strategy is written against. The
/// write-only rule that pruned this struct was applied while a real strategy
/// was still here to define the surface; re-running it now would delete the
/// contract itself rather than dead plumbing.
///
/// Adding a field back is cheap (the field plus one line in the runner's `Ctx`
/// literal) and should land in the same change as the strategy that reads it.
/// Notably absent: the strategy's own `yes_shares`/`no_shares` position, which
/// was removed as write-only and will likely be the first thing an
/// inventory-managing strategy needs back.
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

    /// Optional per-event model diagnostics: return model scores to override the
    /// runner's canonical evaluation for this event.
    ///
    /// No shipped strategy overrides this. It survives the strategy reset because
    /// it is the seam the engine's own model-gate and decision-log tests use to
    /// drive a controlled `ModelOutput` through the gate
    /// (`pm_backtest::fills` tests). Removing it would delete that coverage with
    /// nothing to replace it, in exchange for one dependency edge.
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

pub use fixture::ThresholdFadeStrategy;
pub use trivial::NoopStrategy;
