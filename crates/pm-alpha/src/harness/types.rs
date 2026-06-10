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
    /// Real NO-token ladder (zeros when not loaded; fills then fall back to
    /// the synthetic `1 - yes` complement).
    pub no_bid: f32,
    pub no_ask: f32,
    pub no_bids: [BookLevel; TAPE_DEPTH],
    pub no_asks: [BookLevel; TAPE_DEPTH],
}

impl BookTick {
    pub fn has_real_no(&self) -> bool {
        self.no_ask > 0.0 && self.no_ask < 1.0 && self.no_bid > 0.0 && self.no_bid < 1.0
    }

    /// Cost of buying one NO share: real Down-token ask when loaded, else
    /// the synthetic complement of the YES bid.
    pub fn no_buy_price(&self) -> Option<f64> {
        if self.has_real_no() {
            Some(self.no_ask as f64)
        } else if self.yes_bid > 0.0 && self.yes_bid < 1.0 {
            Some(1.0 - self.yes_bid as f64)
        } else {
            None
        }
    }

    /// Combined cost of one YES + one NO at the touch (the pair-cost / arb
    /// observable). Only meaningful with the real NO ladder.
    pub fn pair_cost(&self) -> Option<f64> {
        if self.has_real_no() && self.yes_ask > 0.0 && self.yes_ask < 1.0 {
            Some(self.yes_ask as f64 + self.no_ask as f64)
        } else {
            None
        }
    }

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

/// How entries are selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum EntryMode {
    /// Enter when the belief disagrees with the book (the validated fade).
    #[default]
    Fade,
    /// Enter when the belief and book agree on direction and the belief
    /// still clears the touch by the threshold (directional/momentum).
    Aligned,
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
    /// Capture stress: only this fraction of displayed size at each level is
    /// available to us (competitors take the rest). 1.0 = the optimistic sim.
    pub depth_capture_frac: f64,
    /// Race stress: assume we ALWAYS lose the race to the touch — every
    /// fill (entry and exit) skips the best level and starts one deeper.
    pub skip_touch_level: bool,
    /// Re-evaluate the belief at this cadence.
    pub decision_dt_ms: u64,
    /// No entries within this many seconds of resolution.
    pub stop_before_close_s: u32,
    /// Exit at the book this many seconds after fill (0 = hold to
    /// resolution). Exits cross the spread and walk depth; unsold remainder
    /// settles at resolution.
    pub exit_after_s: u32,
    /// Take no entries when the open-time regime is calm_low_vol (the cells
    /// show ~no edge there; calm trades are variance without pay).
    pub skip_calm: bool,
    pub entry_mode: EntryMode,
    /// Aligned mode: the side's book mid must exceed this (book agreement).
    pub align_min_mid: f64,
    /// Buy the opposite cheap tail as a convexity hedge when its ask is at
    /// or below this price (0 disables).
    pub tail_max_price: f64,
    /// Tail hedge notional as a fraction of the main clip.
    pub tail_frac: f64,
    /// Max laddered clip entries per market (1 = single entry).
    pub max_clips: u32,
    /// Minimum time between clip entries.
    pub clip_cooldown_ms: u64,
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
            depth_capture_frac: 1.0,
            skip_touch_level: false,
            decision_dt_ms: 1000,
            stop_before_close_s: 10,
            exit_after_s: 0,
            skip_calm: false,
            entry_mode: EntryMode::Fade,
            align_min_mid: 0.55,
            tail_max_price: 0.0,
            tail_frac: 0.25,
            max_clips: 1,
            clip_cooldown_ms: 5000,
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

impl Side {
    pub fn opposite(self) -> Self {
        match self {
            Self::Yes => Self::No,
            Self::No => Self::Yes,
        }
    }
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
    /// Net P&L: exit proceeds (and any resolution remainder) minus cost and
    /// fees. With `exit_after_s = 0` this is settlement at resolution.
    pub pnl: f64,
    pub won: bool,
    /// Price achieved on the exited portion (None when held to resolution).
    pub exit_price: Option<f64>,
    /// Side-oriented book mid 60s after the fill (diagnostic: did the book
    /// move toward the belief, or did we only "win" at resolution?).
    pub mark_60s: Option<f64>,
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
    /// Laddered clip entries, in fill order (empty when no entry).
    pub trades: Vec<TradeRecord>,
    pub samples: Vec<ProbSample>,
    /// Exogenous regime at window open (None when spot history is too thin).
    pub regime: Option<crate::regime::Regime>,
    /// True when the model produced at least one belief during the window.
    pub had_belief: bool,
    /// Calibrator training samples (only when `collect_training` is set).
    pub train_samples: Vec<crate::calibrator::TrainingSample>,
    /// Minimum YES+NO touch cost observed in-window (real NO ladder only).
    pub min_pair_cost: Option<f64>,
    /// Fraction of ticks carrying a real NO ladder.
    pub real_no_coverage: f64,
}
