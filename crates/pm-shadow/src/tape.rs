//! Normalized market tape events for the shared-store collector (phase 2).
//!
//! One collector process ingests feeds and appends `TapeEvent`s to a hot store;
//! shadow, live executor, and backtest replay all read the same bytes.
//! Preserve both `exchange_ms` and `receipt_ms` on every event — basis/vol
//! semantics depend on which clock each layer uses.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: f64,
    pub size: f64,
}

/// Durable event written by the collector; consumers rebuild `ShadowCore` state
/// by replaying the hot window (e.g. 3600s spot+perp + latest book per token).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TapeEvent {
    SpotTrade {
        exchange_ms: i64,
        receipt_ms: i64,
        price: f64,
        quantity: f64,
        is_buyer_maker: bool,
    },
    PerpTrade {
        exchange_ms: i64,
        receipt_ms: i64,
        price: f64,
        quantity: f64,
    },
    BookSnapshot {
        token_id: String,
        exchange_ms: i64,
        receipt_ms: i64,
        bids: Vec<BookLevel>,
        asks: Vec<BookLevel>,
    },
    MarketMeta {
        slug: String,
        up_token: String,
        down_token: String,
        open_ts_s: i64,
        close_ts_s: i64,
        strike: f64,
        strike_source: String,
        condition_id: Option<String>,
        up_index_set: u64,
        down_index_set: u64,
    },
}

/// Hot-store seam: collector writes; readers subscribe or replay_since.
pub trait TapeStore: Send + Sync {
    fn append(&self, event: &TapeEvent) -> anyhow::Result<()>;
    /// Replay events with receipt_ms >= since_ms (inclusive), in order.
    fn replay_since(&self, since_ms: i64) -> anyhow::Result<Vec<TapeEvent>>;
}