//! Telonex Parquet -> internal `ReplayEvent` tape.
//!
//! Reads directly from S3 via `object_store` — no local cache. Same code path
//! runs whether the binary is on a laptop or in an AWS container next to the
//! bucket.

#![forbid(unsafe_code)]

pub mod binance_trades;
pub mod book_snapshot;
pub mod polymarket_onchain;
pub mod polymarket_trades;
pub mod s3;
pub mod schema;

pub use binance_trades::{
    BinanceLoadStats, build_binance_prefix, load_binance_agg_trades_async, resolve_binance_day,
};
pub use book_snapshot::{LoadStats, load_book_snapshot_async};
pub use polymarket_onchain::{
    OnchainFill, OnchainLoadStats, build_pm_onchain_prefix, load_pm_onchain_async,
    resolve_pm_onchain_day, whale_net_flow,
};
pub use polymarket_trades::{
    TradesLoadStats, build_pm_trades_prefix, load_pm_trades_async, resolve_pm_trades_day,
};
pub use s3::{TelonexStore, TelonexStoreConfig, build_object_path};
pub use schema::{Channel, TelonexFile};
