use crate::event::{EngineEvent, Ts};
use crate::seams::Feed;
use std::cell::Cell;
use std::rc::Rc;

/// A `Feed` backed by a pre-loaded `Vec<EngineEvent>`, consumed in order.
///
/// Used by the backtest driver (not gated behind `testkit`) so it is available
/// in release builds. Tests that need a controllable feed also use this.
///
/// When constructed with [`SliceFeed::with_clock`], each `next()` advances the
/// shared clock cell to the handed-out event's `ts` (the same pattern as
/// `testkit::ScriptedFeed`) so a `SimClock` reflects sim time for fill latency
/// and `poll_fills`.
pub struct SliceFeed {
    events: Vec<EngineEvent>,
    cursor: usize,
    clock: Option<Rc<Cell<Ts>>>,
}

impl SliceFeed {
    pub fn new(events: Vec<EngineEvent>) -> Self {
        Self { events, cursor: 0, clock: None }
    }

    /// Like [`SliceFeed::new`] but advances `clock` to each event's `ts` as it is
    /// emitted, driving sim time for a [`crate::seams::SimClock`] sharing the cell.
    pub fn with_clock(events: Vec<EngineEvent>, clock: Rc<Cell<Ts>>) -> Self {
        Self { events, cursor: 0, clock: Some(clock) }
    }

    pub fn is_empty(&self) -> bool {
        self.cursor >= self.events.len()
    }

    pub fn remaining(&self) -> usize {
        self.events.len().saturating_sub(self.cursor)
    }
}

impl Feed for SliceFeed {
    fn next(&mut self) -> Option<EngineEvent> {
        if self.cursor < self.events.len() {
            let ev = self.events[self.cursor];
            self.cursor += 1;
            if let Some(clock) = &self.clock {
                clock.set(ev.ts());
            }
            Some(ev)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{MarketId, NoBook, ReplayEvent, ReplayFlags};

    fn make_event(ts: i64) -> EngineEvent {
        EngineEvent::Market {
            replay: ReplayEvent {
                ts_ns: ts,
                market_id: MarketId(0),
                yes_mid: 0.5,
                yes_bid: 0.49,
                yes_ask: 0.51,
                volume: 0.0,
                bids: Default::default(),
                asks: Default::default(),
                spot_price: 0.0,
                flags: ReplayFlags::BOOK_UPDATE,
            },
            no_book: NoBook::default(),
        }
    }

    #[test]
    fn slice_feed_emits_events_in_order_then_none() {
        let events = vec![make_event(100), make_event(200), make_event(300)];
        let mut feed = SliceFeed::new(events);

        let e1 = feed.next().unwrap();
        assert_eq!(e1.ts(), 100);
        let e2 = feed.next().unwrap();
        assert_eq!(e2.ts(), 200);
        let e3 = feed.next().unwrap();
        assert_eq!(e3.ts(), 300);
        assert!(feed.next().is_none());
        assert!(feed.next().is_none());
    }

    #[test]
    fn slice_feed_empty_vec_returns_none_immediately() {
        let mut feed = SliceFeed::new(vec![]);
        assert!(feed.next().is_none());
        assert!(feed.is_empty());
        assert_eq!(feed.remaining(), 0);
    }
}
