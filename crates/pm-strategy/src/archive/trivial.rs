//! Trivial plumbing-test strategies.

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory};

pub struct BuyYesAtOpen {
    pub clip_shares: f64,
    fired: bool,
}

impl BuyYesAtOpen {
    pub fn new(clip_shares: f64) -> Self {
        Self {
            clip_shares,
            fired: false,
        }
    }
}

impl Strategy for BuyYesAtOpen {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        _ctx: &Ctx,
        _spot: &SpotHistory,
        _trades: &pm_types::TradeHistory,
    ) -> StrategyOutput {
        if self.fired || event.yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }
        self.fired = true;
        StrategyOutput::one(OrderRequest {
            side: Side::BuyYes,
            shares: self.clip_shares,
            max_depth: 1,
            limit_price: None,
            tag: "buy_yes_at_open",
        })
    }
}

pub struct NoopStrategy;
impl Strategy for NoopStrategy {
    fn on_event(
        &mut self,
        _event: &ReplayEvent,
        _ctx: &Ctx,
        _spot: &SpotHistory,
        _trades: &pm_types::TradeHistory,
    ) -> StrategyOutput {
        StrategyOutput::hold()
    }
}
