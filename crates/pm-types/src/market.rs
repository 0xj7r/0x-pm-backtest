use serde::{Deserialize, Serialize};

/// Compact opaque market identifier. Mapping (slug ↔ id) lives in the
/// manifest alongside prepared tapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(transparent)]
pub struct MarketId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Yes,
    No,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketInfo {
    pub id: MarketId,
    pub slug: String,
    pub condition_id: String,
    pub yes_token_id: String,
    pub no_token_id: String,
    /// Bar open (UTC, ns since epoch).
    pub open_ts_ns: i64,
    /// Bar close / resolution time (UTC, ns since epoch).
    pub close_ts_ns: i64,
    /// Underlying spot symbol (e.g. "BTCUSDT").
    pub spot_symbol: String,
}
