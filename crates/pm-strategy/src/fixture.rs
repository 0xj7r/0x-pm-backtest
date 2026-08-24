//! Deterministic fixture strategy for the golden replay harness.
//!
//! This is test plumbing, not a trading strategy: it exists so the pinned-tape
//! golden gate has something that actually submits orders and takes fills, and
//! so the engine's order/fill/settlement path stays covered after the real
//! strategies are retired. It is reachable from the CLI only behind
//! `--allow-fixture`.
//!
//! The rule is intentionally crude and stateless apart from a one-shot latch:
//! once per market, after a short warmup, buy the cheap side when the YES mid
//! sits outside a fixed band, then hold to resolution. No spot, no trades, no
//! model, no config surface beyond compile-time constants.

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

/// Events to ignore at the open, so the entry does not key off a single
/// half-formed first book snapshot.
const WARMUP_EVENTS: u64 = 16;
/// YES is the cheap side at or below this mid.
const CHEAP_YES_MID: f32 = 0.35;
/// NO is the cheap side at or above this mid.
const CHEAP_NO_MID: f32 = 0.65;
const CLIP_SHARES: f64 = 25.0;
const MAX_DEPTH: usize = 3;
const ENTRY_TAG: &str = "fixture_threshold_entry";

#[derive(Debug, Default)]
pub struct ThresholdFadeStrategy {
    entered: bool,
}

impl ThresholdFadeStrategy {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Strategy for ThresholdFadeStrategy {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        _spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        if self.entered || ctx.events_seen < WARMUP_EVENTS {
            return StrategyOutput::hold();
        }
        let mid = event.yes_mid;
        if !mid.is_finite() || mid <= 0.0 || mid >= 1.0 {
            return StrategyOutput::hold();
        }
        let side = if mid <= CHEAP_YES_MID {
            Side::BuyYes
        } else if mid >= CHEAP_NO_MID {
            Side::BuyNo
        } else {
            return StrategyOutput::hold();
        };
        self.entered = true;
        StrategyOutput::one(OrderRequest {
            side,
            shares: CLIP_SHARES,
            max_depth: MAX_DEPTH,
            limit_price: None,
            tag: ENTRY_TAG,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{MarketId, ReplayEvent};

    fn event_at(mid: f32) -> ReplayEvent {
        ReplayEvent {
            ts_ns: 0,
            market_id: MarketId(0),
            yes_mid: mid,
            yes_bid: mid - 0.01,
            yes_ask: mid + 0.01,
            volume: 0.0,
            bids: Default::default(),
            asks: Default::default(),
            spot_price: 0.0,
            flags: Default::default(),
        }
    }

    fn ctx_after(events_seen: u64) -> Ctx {
        Ctx {
            events_seen,
            ..Ctx::default()
        }
    }

    fn run(mid: f32, events_seen: u64) -> StrategyOutput {
        let mut s = ThresholdFadeStrategy::new();
        s.on_event(
            &event_at(mid),
            &ctx_after(events_seen),
            &SpotHistory::default(),
            &TradeHistory::default(),
        )
    }

    #[test]
    fn holds_through_warmup_even_at_an_extreme_mid() {
        assert!(run(0.10, WARMUP_EVENTS - 1).orders.is_empty());
    }

    #[test]
    fn buys_yes_when_yes_is_cheap() {
        let out = run(0.20, WARMUP_EVENTS);
        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyYes);
        assert_eq!(out.orders[0].shares, CLIP_SHARES);
        assert_eq!(out.orders[0].tag, ENTRY_TAG);
    }

    #[test]
    fn buys_no_when_no_is_cheap() {
        let out = run(0.80, WARMUP_EVENTS);
        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyNo);
    }

    #[test]
    fn holds_inside_the_band() {
        assert!(run(0.50, WARMUP_EVENTS).orders.is_empty());
    }

    #[test]
    fn holds_on_a_degenerate_mid() {
        assert!(run(0.0, WARMUP_EVENTS).orders.is_empty());
        assert!(run(1.0, WARMUP_EVENTS).orders.is_empty());
        assert!(run(f32::NAN, WARMUP_EVENTS).orders.is_empty());
    }

    #[test]
    fn enters_at_most_once_per_market() {
        let mut s = ThresholdFadeStrategy::new();
        let ctx = ctx_after(WARMUP_EVENTS);
        let first = s.on_event(
            &event_at(0.20),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        let second = s.on_event(
            &event_at(0.20),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert_eq!(first.orders.len(), 1);
        assert!(second.orders.is_empty());
    }
}
