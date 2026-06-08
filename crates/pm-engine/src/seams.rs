use crate::event::{EngineEvent, Ts};
use pm_strategy::Side;
use pm_types::MarketId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OrderId(pub u64);

/// An order accepted by the risk gate, handed to the Exchange.
#[derive(Debug, Clone, Copy)]
pub struct OrderIntent {
    pub id: OrderId,
    pub market: MarketId,
    pub side: Side,
    pub shares: f64,
    pub max_depth: usize,
    /// `None` = taker (sweep opposing book); `Some(p)` = maker limit (YES terms).
    pub limit_price: Option<f32>,
    pub tag: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillLiquidity {
    Maker,
    Taker,
}

#[derive(Debug, Clone, Copy)]
pub struct FillReport {
    pub order: OrderId,
    pub market: MarketId,
    pub side: Side,
    pub shares: f64,
    /// Executed price in YES terms (NO fills are reported as their NO price).
    pub price: f32,
    pub fee_usd: f64,
    pub liquidity: FillLiquidity,
    pub ts: Ts,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SubmitAck {
    Accepted,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CancelAck {
    Cancelled,
    Unknown,
}

/// Produces the ordered market-event stream. Synchronous pull keeps the engine
/// deterministic; the live driver's impl blocks on a merged channel.
pub trait Feed {
    fn next(&mut self) -> Option<EngineEvent>;
}

/// Turns accepted orders into fills. sim: compute from book/tape; live: CLOB.
pub trait Exchange {
    fn submit(&mut self, order: OrderIntent, now: Ts) -> SubmitAck;
    fn cancel(&mut self, id: OrderId, now: Ts) -> CancelAck;
    /// Fills realized since the previous poll.
    fn poll_fills(&mut self, now: Ts) -> Vec<FillReport>;
}

/// Time source. sim: last event ts; live: wall clock.
pub trait Clock {
    fn now(&self) -> Ts;
}
