//! Shared types for polymarket-agent-v2.
//!
//! Kept dependency-light on purpose: this crate does NOT pull in `nautilus-*`.
//! Adapters in `pm-telonex-loader` convert between these and Nautilus types.

pub mod config;
pub mod market;
pub mod spot;
pub mod tape;
pub mod trade;

pub use config::{AppConfig, BacktestConfig, BacktestMode};
pub use market::{MarketId, MarketInfo, Outcome};
pub use spot::{FlowStats, SpotAccelStats, SpotHistory, SpotTick};
pub use tape::{BookLevel, NoBook, ReplayEvent, ReplayFlags, TAPE_DEPTH};
pub use trade::{TradeFlowSignal, TradeHistory, TradeTick, compute_trade_flow};

#[derive(Debug, thiserror::Error)]
pub enum PmError {
    #[error("config: {0}")]
    Config(String),
    #[error("data: {0}")]
    Data(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

pub type PmResult<T> = std::result::Result<T, PmError>;
