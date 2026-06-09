//! Clean multi-market convex-book strategy: Signal -> PositionManager -> ExecutionPolicy.
pub mod signal;
pub mod position;
pub mod execution;

use crate::{Ctx, Side, Strategy, StrategyOutput};
use pm_model::ModelOutput;
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

use signal::{evaluate, SignalGate, Conviction};
use position::{BothBookPrices, PositionConfig, PositionManager};
use execution::{ExecutionPolicy, Posture};

#[derive(Debug, Clone)]
pub struct ConvexBookConfig {
    pub signal: SignalGate,
    pub position: PositionConfig,
    pub posture: Posture,
}
impl Default for ConvexBookConfig {
    fn default() -> Self {
        Self { signal: SignalGate::default(), position: PositionConfig::default(), posture: Posture::Adaptive }
    }
}

/// Per-market convex-book strategy. The engine clones one per market, so the
/// PositionManager's inventory state is naturally isolated.
#[derive(Clone)]
pub struct ConvexBookStrategy {
    signal: SignalGate,
    position: PositionManager,
    execution_posture: Posture,
    /// Last confirmed conviction; retained so the tail checker can run on
    /// ticks where the signal gate fails but inventory is already loaded.
    last_conv: Option<Conviction>,
}

impl ConvexBookStrategy {
    pub fn new(cfg: ConvexBookConfig) -> Self {
        Self {
            signal: cfg.signal,
            position: PositionManager::new(cfg.position),
            execution_posture: cfg.posture,
            last_conv: None,
        }
    }
}

impl Strategy for ConvexBookStrategy {
    fn on_event(&mut self, event: &ReplayEvent, ctx: &Ctx, _spot: &SpotHistory, _trades: &TradeHistory) -> StrategyOutput {
        let secs_to_close = ((ctx.market_close_ns - event.ts_ns) as f64 / 1e9) as f32;
        if secs_to_close < 0.0 {
            return StrategyOutput::hold();
        }
        let prices = BothBookPrices {
            yes_ask: event.yes_ask, yes_bid: event.yes_bid,
            no_ask: ctx.no_ask, no_bid: ctx.no_bid,
        };
        let favourite = if event.yes_mid >= 0.5 { Side::BuyYes } else { Side::BuyNo };
        let fav_ask = prices.ask(favourite);
        let fresh = evaluate(ctx, event.yes_mid, fav_ask, &self.signal);
        if let Some(c) = fresh {
            self.last_conv = Some(c);
        }
        let Some(conv) = self.last_conv else {
            return StrategyOutput::hold();
        };
        let target = self.position.plan(&conv, &prices, secs_to_close);
        if target.legs.is_empty() {
            return StrategyOutput::hold();
        }
        let policy = ExecutionPolicy::new(self.execution_posture);
        StrategyOutput { orders: policy.orders(&target, secs_to_close) }
    }

    fn on_event_scored(&mut self, event: &ReplayEvent, ctx: &Ctx, spot: &SpotHistory, trades: &TradeHistory)
        -> (StrategyOutput, Option<ModelOutput>) {
        (self.on_event(event, ctx, spot, trades), ctx.model_output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Ctx, Side, Strategy};
    use pm_model::ModelOutput;
    use pm_types::{MarketId, ReplayEvent, ReplayFlags, SpotHistory, TradeHistory};

    fn event_at(secs_to_close: f32, yes_ask: f32, yes_bid: f32) -> (ReplayEvent, Ctx) {
        let close_ns = 1_000_000_000_000i64;
        let ts_ns = close_ns - (secs_to_close as i64) * 1_000_000_000;
        let ev = ReplayEvent {
            ts_ns, market_id: MarketId(0),
            yes_mid: (yes_ask + yes_bid) / 2.0, yes_bid, yes_ask,
            volume: 0.0, bids: Default::default(), asks: Default::default(),
            spot_price: 0.0, flags: ReplayFlags::BOOK_UPDATE,
        };
        let ctx = Ctx {
            market_close_ns: close_ns,
            no_ask: 1.0 - yes_bid, no_bid: 1.0 - yes_ask, no_mid: 1.0 - (yes_ask + yes_bid) / 2.0,
            model_output: Some(ModelOutput {
                direction_score: 0.5, confidence_score: 0.75, calibrated_p: 0.86, risk_score: 0.3,
            }),
            ..Ctx::default()
        };
        (ev, ctx)
    }

    #[test]
    fn loads_favourite_then_tail_over_market_life() {
        let mut s = ConvexBookStrategy::new(ConvexBookConfig::default());
        let spot = SpotHistory::default();
        let trades = TradeHistory::default();

        // Early: no favourite load yet (secs_in < start).
        let (e0, c0) = event_at(200.0, 0.80, 0.79);
        assert!(s.on_event(&e0, &c0, &spot, &trades).orders.is_empty(), "too early");

        // Late: favourite YES loads.
        let (e1, c1) = event_at(100.0, 0.80, 0.79);
        let out1 = s.on_event(&e1, &c1, &spot, &trades);
        assert!(out1.orders.iter().any(|o| o.side == Side::BuyYes), "favourite loads late");

        // Later + extreme skew + cheap NO: tail (BuyNo) appears.
        let (e2, c2) = event_at(60.0, 0.93, 0.92);
        let out2 = s.on_event(&e2, &c2, &spot, &trades);
        assert!(out2.orders.iter().any(|o| o.side == Side::BuyNo), "cheap convex tail appears");
    }
}
