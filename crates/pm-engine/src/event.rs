use pm_types::{MarketId, NoBook, ReplayEvent, TradeTick};

/// Single time currency: nanoseconds since the Unix epoch.
pub type Ts = i64;

/// Underlying asset of a market. Drives the correlated-exposure cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Btc,
    Eth,
    Sol,
    Xrp,
}

/// The one event currency both drivers produce, in timestamp order.
///
/// `Market` wraps the existing `ReplayEvent` (which already carries `market_id`,
/// the YES book, `spot_price`, and event-kind `flags`) plus the real NO ladder.
/// The strategy sees only the `ReplayEvent`; the engine keeps `no_book` for fills.
///
/// `Trade` carries a real on-chain trade print for one market. The engine routes
/// it to `Exchange::on_trade` (drives trade-tape maker fills). Trades and book
/// updates share the single ts-ordered stream so maker-fill timing and
/// cross-market capital sequencing stay faithful.
#[derive(Debug, Clone, Copy)]
pub enum EngineEvent {
    Market { replay: ReplayEvent, no_book: NoBook },
    Trade { market: MarketId, tick: TradeTick },
}

impl EngineEvent {
    pub fn ts(&self) -> Ts {
        match self {
            EngineEvent::Market { replay, .. } => replay.ts_ns,
            EngineEvent::Trade { tick, .. } => tick.ts_ns,
        }
    }
    pub fn market_id(&self) -> MarketId {
        match self {
            EngineEvent::Market { replay, .. } => replay.market_id,
            EngineEvent::Trade { market, .. } => *market,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{MarketId, ReplayEvent, ReplayFlags};

    fn replay_at(ts: i64) -> ReplayEvent {
        ReplayEvent {
            ts_ns: ts,
            market_id: MarketId(0),
            yes_mid: 0.5,
            yes_bid: 0.49,
            yes_ask: 0.51,
            volume: 0.0,
            bids: Default::default(),
            asks: Default::default(),
            spot_price: 0.0,
            flags: ReplayFlags::empty(),
        }
    }

    #[test]
    fn engine_event_exposes_ts() {
        let ev = EngineEvent::Market { replay: replay_at(123), no_book: NoBook::default() };
        assert_eq!(ev.ts(), 123);
    }
}
