use crate::event::{EngineEvent, Ts};
use crate::seams::{CancelAck, Clock, Exchange, Feed, FillReport, OrderId, OrderIntent, SubmitAck};
use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

/// Feed that replays a fixed, pre-sorted vector of events.
pub struct ScriptedFeed {
    events: VecDeque<EngineEvent>,
    clock: Rc<Cell<Ts>>,
}

impl ScriptedFeed {
    pub fn new(mut events: Vec<EngineEvent>, clock: Rc<Cell<Ts>>) -> Self {
        events.sort_by_key(|e| e.ts());
        Self { events: events.into(), clock }
    }
}

impl Feed for ScriptedFeed {
    fn next(&mut self) -> Option<EngineEvent> {
        let ev = self.events.pop_front()?;
        self.clock.set(ev.ts()); // advance sim time to the event we hand out
        Some(ev)
    }
}

/// Clock backed by a shared cell the feed advances.
pub struct SimClock {
    pub ts: Rc<Cell<Ts>>,
}
impl Clock for SimClock {
    fn now(&self) -> Ts {
        self.ts.get()
    }
}

/// Exchange that fills every submitted order instantly at a scripted price.
/// Phase 1 only needs deterministic fills to exercise the loop; the real
/// both-book fill model is Phase 2.
pub struct InstantExchange {
    next_fill: Vec<FillReport>,
    pub submitted: Vec<OrderIntent>,
    pub fill_price: f32,
    pub fee_usd: f64,
}

impl InstantExchange {
    pub fn new(fill_price: f32, fee_usd: f64) -> Self {
        Self { next_fill: Vec::new(), submitted: Vec::new(), fill_price, fee_usd }
    }
}

impl Exchange for InstantExchange {
    fn submit(&mut self, order: OrderIntent, now: Ts) -> SubmitAck {
        self.submitted.push(order);
        self.next_fill.push(FillReport {
            order: order.id,
            market: order.market,
            side: order.side,
            shares: order.shares,
            price: self.fill_price,
            fee_usd: self.fee_usd,
            liquidity: crate::seams::FillLiquidity::Taker,
            ts: now,
        });
        SubmitAck::Accepted
    }
    fn cancel(&mut self, _id: OrderId, _now: Ts) -> CancelAck {
        CancelAck::Unknown
    }
    fn poll_fills(&mut self, _now: Ts) -> Vec<FillReport> {
        std::mem::take(&mut self.next_fill)
    }
}
