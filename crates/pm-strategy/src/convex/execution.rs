use crate::OrderRequest;
use crate::convex::position::TargetIncrement;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Posture {
    /// Post-only limit at the reference price (capture rebate / better entry).
    Maker,
    /// Sweep the opposing book (guaranteed fill, pays the spread).
    Taker,
    /// Maker while there is time to wait; taker inside `taker_switch_secs` of close.
    Adaptive,
}

pub struct ExecutionPolicy {
    posture: Posture,
    /// Seconds-to-close under which Adaptive escalates to taker.
    taker_switch_secs: f32,
}

impl ExecutionPolicy {
    pub fn new(posture: Posture) -> Self {
        Self { posture, taker_switch_secs: 5.0 }
    }

    pub fn orders(&self, target: &TargetIncrement, secs_to_close: f32) -> Vec<OrderRequest> {
        let take = match self.posture {
            Posture::Taker => true,
            Posture::Maker => false,
            Posture::Adaptive => secs_to_close <= self.taker_switch_secs,
        };
        target
            .legs
            .iter()
            .map(|leg| OrderRequest {
                side: leg.side,
                shares: leg.shares,
                max_depth: leg.max_depth,
                limit_price: if take { None } else { Some(leg.price_ref) },
                tag: "convex_book",
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Side;
    use crate::convex::position::{TargetIncrement, TargetLeg};

    fn target() -> TargetIncrement {
        TargetIncrement { legs: vec![TargetLeg { side: Side::BuyYes, shares: 10.0, max_depth: 3, price_ref: 0.80 }] }
    }

    #[test]
    fn taker_emits_market_orders() {
        let p = ExecutionPolicy::new(Posture::Taker);
        let orders = p.orders(&target(), 100.0);
        assert_eq!(orders.len(), 1);
        assert!(orders[0].limit_price.is_none(), "taker => no limit price (sweep)");
    }

    #[test]
    fn maker_emits_post_only_limit_at_price_ref() {
        let p = ExecutionPolicy::new(Posture::Maker);
        let orders = p.orders(&target(), 100.0);
        assert_eq!(orders[0].limit_price, Some(0.80), "maker => limit at price_ref");
    }

    #[test]
    fn adaptive_is_maker_with_time_taker_near_close() {
        let p = ExecutionPolicy::new(Posture::Adaptive);
        assert!(p.orders(&target(), 100.0)[0].limit_price.is_some(), "adaptive far from close => maker");
        assert!(p.orders(&target(), 3.0)[0].limit_price.is_none(), "adaptive near close => taker");
    }
}
