//! Trivial plumbing-test strategies.

use crate::{Ctx, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory};

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
