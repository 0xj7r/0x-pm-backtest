use crate::market::MarketId;

pub const TAPE_DEPTH: usize = 5;

/// One level of an order book side.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[repr(C)]
pub struct BookLevel {
    pub price: f32,
    pub size: f32,
}

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
    pub struct ReplayFlags: u8 {
        const TRADE        = 0b0000_0001;
        const BOOK_UPDATE  = 0b0000_0010;
        const SPOT_UPDATE  = 0b0000_0100;
        const MARKET_OPEN  = 0b0000_1000;
        const MARKET_CLOSE = 0b0001_0000;
    }
}

/// Replay tape event. Packed for cache locality during deterministic replay.
///
/// Single fixed depth (5 levels) keeps SIMD/vector access cheap. Bigger depth
/// rebuilds the book offline before tape generation.
///
/// Now serializable — enables fast on-disk ReplayEvent caching for repeated
/// large AWS/S3 runs (see --replay-event-cache-dir in pm-app).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[repr(C)]
pub struct ReplayEvent {
    pub ts_ns: i64,
    pub market_id: MarketId,
    pub yes_mid: f32,
    pub yes_bid: f32,
    pub yes_ask: f32,
    pub volume: f32,
    pub bids: [BookLevel; TAPE_DEPTH],
    pub asks: [BookLevel; TAPE_DEPTH],
    pub spot_price: f32,
    pub flags: ReplayFlags,
}

const _: () = {
    assert!(std::mem::size_of::<ReplayEvent>() <= 128);
};

/// Real NO-side ladder, carried alongside a `ReplayEvent` for fill simulation.
/// Kept separate so the strategy-facing `ReplayEvent` stays YES-centric and the
/// sim Exchange prices the NO leg from real depth instead of `1 - yes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[repr(C)]
pub struct NoBook {
    pub bids: [BookLevel; TAPE_DEPTH],
    pub asks: [BookLevel; TAPE_DEPTH],
}

#[cfg(test)]
mod no_book_tests {
    use super::*;

    #[test]
    fn no_book_defaults_to_empty_levels() {
        let nb = NoBook::default();
        assert_eq!(nb.bids[0], BookLevel::default());
        assert_eq!(nb.asks[0], BookLevel::default());
    }

    #[test]
    fn no_book_roundtrips_serde() {
        let mut nb = NoBook::default();
        nb.asks[0] = BookLevel { price: 0.42, size: 100.0 };
        let json = serde_json::to_string(&nb).unwrap();
        let back: NoBook = serde_json::from_str(&json).unwrap();
        assert_eq!(back.asks[0].price, 0.42);
    }
}
