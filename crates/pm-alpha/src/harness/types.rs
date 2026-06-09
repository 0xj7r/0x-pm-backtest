//! Harness input/output types.

use crate::state::MarketMeta;
use pm_types::tape::{BookLevel, TAPE_DEPTH};

/// One Polymarket YES-book observation. Built from `ReplayEvent` by the
/// replay adapter (pm-app); the harness is the ONLY pm-alpha layer that sees
/// book data, and only to price the bet, never to form the belief.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct BookTick {
    pub ts_ns: i64,
    pub yes_bid: f32,
    pub yes_ask: f32,
    pub bids: [BookLevel; TAPE_DEPTH],
    pub asks: [BookLevel; TAPE_DEPTH],
}

impl BookTick {
    pub fn mid(&self) -> Option<f64> {
        if self.yes_bid > 0.0 && self.yes_ask > 0.0 && self.yes_ask < 1.0 && self.yes_bid < 1.0 {
            Some(((self.yes_bid + self.yes_ask) / 2.0) as f64)
        } else {
            None
        }
    }
}

/// One market's full replay input.
#[derive(Debug, Clone)]
pub struct MarketSeries {
    pub meta: MarketMeta,
    pub resolved_yes: bool,
    pub ticks: Vec<BookTick>,
    /// Date partition (YYYY-MM-DD), for split bookkeeping.
    pub date: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct HarnessConfig {
    /// Decision at T fills against the book at the first tick >= T + latency.
    pub latency_ms: u64,
    pub taker_fee_bps: f64,
    /// Enter when the chosen side's edge exceeds this.
    pub edge_threshold: f64,
    /// Dollar notional per entry, walked through book depth.
    pub notional_usdc: f64,
    /// Re-evaluate the belief at this cadence.
    pub decision_dt_ms: u64,
    /// No entries within this many seconds of resolution.
    pub stop_before_close_s: u32,
    /// Emit calibrator training samples (features + raw base p + outcome).
    pub collect_training: bool,
    /// Cadence of training-sample collection (seconds into the window).
    pub train_sample_dt_s: u32,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            latency_ms: 150,
            taker_fee_bps: 0.0,
            edge_threshold: 0.05,
            notional_usdc: 50.0,
            decision_dt_ms: 1000,
            stop_before_close_s: 10,
            collect_training: false,
            train_sample_dt_s: 15,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Side {
    Yes,
    No,
}

/// One executed simulated trade, held to resolution.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct TradeRecord {
    pub side: Side,
    pub decision_ts_ns: i64,
    pub fill_ts_ns: i64,
    pub avg_price: f64,
    pub shares: f64,
    pub fee: f64,
    /// p_exo at decision time.
    pub p_exo: f64,
    /// Book mid at decision time (diagnostic; not used in the belief).
    pub mid_at_decision: f64,
    /// Net P&L at resolution: shares * (payout - avg_price) - fee.
    pub pnl: f64,
    pub won: bool,
}

/// A probability sample at a fixed checkpoint, for log-loss scoring of the
/// exogenous belief against the book-implied baseline on identical instants.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ProbSample {
    pub ts_ns: i64,
    pub p_exo: f64,
    pub p_book: f64,
    pub resolved_yes: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct MarketRunOutput {
    pub trade: Option<TradeRecord>,
    pub samples: Vec<ProbSample>,
    /// True when the model produced at least one belief during the window.
    pub had_belief: bool,
    /// Calibrator training samples (only when `collect_training` is set).
    pub train_samples: Vec<crate::calibrator::TrainingSample>,
}
