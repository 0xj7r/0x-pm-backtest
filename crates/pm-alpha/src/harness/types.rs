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
    /// Polymarket crypto taker fee curve: fee = rate * p * (1-p) per share on
    /// every aggressive fill (entry, spread-crossing exit, tail hedge, pair
    /// completion). Resolution redemption is free. 0 disables (parity).
    #[serde(default)]
    pub fee_curve_rate: f64,
    /// Fee-aware exit: at the exit instant, sell only when the depth-walked
    /// net proceeds beat the belief's hold-to-resolution EV; otherwise hold
    /// (entry fee only). Applies to the spread-crossing exit path.
    #[serde(default)]
    pub fee_aware_exit: bool,
    /// Variance-aversion premium for `fee_aware_exit`: sell only if
    /// exit_net >= hold_ev + margin * shares (higher = holds rarer).
    #[serde(default)]
    pub fee_exit_margin: f64,
    /// Enter when the chosen side's edge exceeds this.
    pub edge_threshold: f64,
    /// Dollar notional per entry, walked through book depth.
    pub notional_usdc: f64,
    /// Kelly-style per-entry sizing: notional scales with the
    /// reliability-discounted edge and equalizes per-trade variance
    /// (cheap lottery entries shrink hard). false = flat clips.
    pub kelly_sizing: bool,
    /// Thesis gate: skip when chosen-side belief is below this (0 = off).
    #[serde(default)]
    pub min_p_side: f64,
    /// Thesis gate: skip when chosen-side belief exceeds this (1.0 = off).
    #[serde(default = "default_max_entry_ask")]
    pub max_p_side: f64,
    /// Thesis gate: skip when entry ask is below this (0 = off).
    #[serde(default)]
    pub min_entry_ask: f64,
    /// No entries until this many seconds after market open (0 = off).
    #[serde(default)]
    pub min_secs_from_open: u32,
    /// Thesis gate: skip when entry ask exceeds this (1.0 = off).
    #[serde(default = "default_max_entry_ask")]
    pub max_entry_ask: f64,
    /// Vol-responsive sizing: when > 0, clip = notional * clamp(sigma_bar_bps
    /// / ref, lo, hi). Sizes up on high-movement tape (real edges), down on
    /// pinned tape (phantom edges). 0 = off. Causal (uses entry sigma).
    pub vol_sizing_ref_bps: f64,
    pub vol_sizing_lo: f64,
    pub vol_sizing_hi: f64,
    /// Basis-momentum tilt: scale the stake by whether the 60s change in
    /// perp-minus-spot basis agrees with the entry side. Yes wants basis
    /// rising, No wants it falling. 1.0/1.0 = off. Multiplies the base clip
    /// (after vol/kelly sizing) before caps; never an entry gate.
    pub basis_mom_agree: f64,
    pub basis_mom_disagree: f64,
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
    /// Entry window: entries are permitted ONLY when time-to-close is at
    /// most this many seconds (0 = disabled). Composes with
    /// `stop_before_close_s` as the inner bound: entries are allowed in
    /// [close - enter_within_close_s, close - stop_before_close_s].
    #[serde(default)]
    pub enter_within_close_s: u32,
    /// Exit at the book this many seconds after fill (0 = hold to
    /// resolution). Exits cross the spread and walk depth; unsold remainder
    /// settles at resolution.
    pub exit_after_s: u32,
    /// Passive exit study: instead of crossing the spread at the exit
    /// horizon, rest an ask at the side mid. The trade's pnl uses the exact
    /// conditional fill (filled only if a later bid crosses the level before
    /// close, else settle at resolution); the optimistic always-fills-at-mid
    /// bound is recorded alongside in `pnl_exit_mid_optimistic`.
    #[serde(default)]
    pub exit_at_mid: bool,
    /// Hybrid passive exit: rest an ask at the side mid at the exit horizon;
    /// if no bid crosses the level within this many seconds, convert to the
    /// champion spread-crossing exit against the book as of the timeout
    /// (settle at resolution if no tick remains). 0 disables (champion
    /// behavior). Takes precedence over `exit_at_mid`.
    #[serde(default)]
    pub passive_exit_timeout_s: u32,
    /// Pair completion: after the first entry (cost C1, S1 shares), buy the
    /// opposite token when its ask <= 1 - C1 - margin (S1 shares, once per
    /// market); both legs then hold to resolution, netting the locked
    /// profit. 0 disables.
    #[serde(default)]
    pub pair_completion_margin: f64,
    /// Take no entries when the open-time regime is calm_low_vol (the cells
    /// show ~no edge there; calm trades are variance without pay).
    pub skip_calm: bool,
    /// Take entries ONLY in calm_low_vol windows (calm-regime strategy
    /// exploration; mutually exclusive with skip_calm in spirit).
    pub only_calm: bool,
    pub entry_mode: EntryMode,
    /// Aligned mode: the side's book mid must exceed this (book agreement).
    pub align_min_mid: f64,
    /// Buy the opposite cheap tail as a convexity hedge when its ask is at
    /// or below this price (0 disables).
    pub tail_max_price: f64,
    /// Tail hedge notional as a fraction of the main clip.
    pub tail_frac: f64,
    /// Skip entries when sigma_bar_bps at decision exceeds this (regime
    /// gate for vol-sensitive lanes; 0 disables).
    pub max_entry_sigma_bps: f64,
    /// Vol floor: skip when belief sigma_bar_bps is below this (shadow/live
    /// uses 3.0; 0 = off).
    #[serde(default)]
    pub min_entry_sigma_bps: f64,
    /// Skip UTC-Saturday entries (shadow-final behaviour).
    #[serde(default)]
    pub skip_saturday: bool,
    /// Post-entry selldown stop (hold mode only, `exit_after_s == 0`):
    /// after the fill, sell (taker, crossing the bid, exit-leg fee) at the
    /// first subsequent tick where the entry side's ask prints at-or-below
    /// `avg_fill_price - eps`. Negative disables (parity).
    #[serde(default = "default_selldown_stop_eps")]
    pub selldown_stop_eps: f64,
    /// Pre-entry stability gate: take an entry only when, over the trailing
    /// S seconds of ticks before the decision, the entry side's ask never
    /// printed below (current side ask - stability_eps). 0 disables (parity).
    #[serde(default)]
    pub entry_stability_s: u32,
    /// Tolerance on the stability gate's trailing-min comparison.
    #[serde(default)]
    pub stability_eps: f64,
    /// Entry fills walk depth only while the marginal level retains at
    /// least this much edge vs the belief (0 = walk unconditionally). Sets
    /// the live marketable-limit price: belief - floor.
    pub min_marginal_edge: f64,
    /// Max laddered clip entries per market (1 = single entry).
    pub max_clips: u32,
    /// Minimum time between clip entries.
    pub clip_cooldown_ms: u64,
    /// Event-based re-entry: after an entry, block further entries until a
    /// later decision shows BOTH sides' edges below this level (the
    /// dislocation has closed); the next threshold crossing is then a fresh
    /// staleness event. 0 disables (cooldown-only laddering).
    #[serde(default)]
    pub rearm_edge: f64,
    /// Maker entry study: when >= 0, an entry signal rests a bid at
    /// (decision-tick side ask - offset) instead of taking the ask. The
    /// order goes live after the entry latency and fills only if a later
    /// tick's side ask trades at-or-below the level before the
    /// `stop_before_close_s` deadline (conservative crossed-through
    /// primitive); fills pay ZERO fee and hold to resolution. Unfilled
    /// orders cancel at the deadline. One resting order per market.
    /// Negative disables (taker parity).
    #[serde(default = "default_maker_entry_offset")]
    pub maker_entry_offset: f64,
    /// Emit calibrator training samples (features + raw base p + outcome).
    pub collect_training: bool,
    /// Cadence of training-sample collection (seconds into the window).
    pub train_sample_dt_s: u32,
    /// Skip when decision-time regime is `expanded_high_flip`.
    #[serde(default)]
    pub skip_expanded_high_flip: bool,
    /// Skip open-window favourites where model >> book.
    #[serde(default)]
    pub skip_open_fav_gap: bool,
    #[serde(default = "default_open_fav_p_min")]
    pub open_fav_p_min: f64,
    #[serde(default = "default_open_fav_ask_max")]
    pub open_fav_ask_max: f64,
    #[serde(default = "default_open_fav_secs")]
    pub open_fav_secs: u32,
    /// Pause entries when consecutive resolved losses >= this (0 = off).
    #[serde(default)]
    pub pause_after_consec_losses: u32,
    /// On rearm clips, skip when entry ask exceeds this (0 = off).
    #[serde(default)]
    pub max_rearm_entry_ask: f64,
    /// Skip when spot return over this lookback (seconds) disagrees with side (0 = off).
    #[serde(default)]
    pub skip_spot_misalign_s: u32,
    /// Skip when 60/300/600/900s spot all disagree with entry side.
    #[serde(default)]
    pub skip_spot_against_all: bool,
}

fn default_open_fav_p_min() -> f64 {
    0.90
}

fn default_open_fav_ask_max() -> f64 {
    0.60
}

fn default_open_fav_secs() -> u32 {
    5
}

fn default_selldown_stop_eps() -> f64 {
    -1.0
}

fn default_max_entry_ask() -> f64 {
    1.0
}

fn default_maker_entry_offset() -> f64 {
    -1.0
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            latency_ms: 150,
            taker_fee_bps: 0.0,
            fee_curve_rate: 0.0,
            fee_aware_exit: false,
            fee_exit_margin: 0.0,
            edge_threshold: 0.05,
            notional_usdc: 50.0,
            kelly_sizing: false,
            min_p_side: 0.0,
            max_p_side: 1.0,
            min_entry_ask: 0.0,
            max_entry_ask: 1.0,
            min_secs_from_open: 0,
            vol_sizing_ref_bps: 0.0,
            vol_sizing_lo: 0.5,
            vol_sizing_hi: 2.0,
            basis_mom_agree: 1.0,
            basis_mom_disagree: 1.0,
            depth_capture_frac: 1.0,
            skip_touch_level: false,
            decision_dt_ms: 1000,
            stop_before_close_s: 10,
            enter_within_close_s: 0,
            exit_after_s: 0,
            exit_at_mid: false,
            passive_exit_timeout_s: 0,
            pair_completion_margin: 0.0,
            skip_calm: false,
            only_calm: false,
            entry_mode: EntryMode::Fade,
            align_min_mid: 0.55,
            tail_max_price: 0.0,
            tail_frac: 0.25,
            max_entry_sigma_bps: 0.0,
            min_entry_sigma_bps: 0.0,
            skip_saturday: false,
            selldown_stop_eps: -1.0,
            entry_stability_s: 0,
            stability_eps: 0.005,
            min_marginal_edge: 0.0,
            max_clips: 1,
            clip_cooldown_ms: 5000,
            rearm_edge: 0.0,
            maker_entry_offset: -1.0,
            collect_training: false,
            train_sample_dt_s: 15,
            skip_expanded_high_flip: false,
            skip_open_fav_gap: false,
            open_fav_p_min: 0.90,
            open_fav_ask_max: 0.60,
            open_fav_secs: 5,
            pause_after_consec_losses: 0,
            max_rearm_entry_ask: 0.0,
            skip_spot_misalign_s: 0,
            skip_spot_against_all: false,
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
    /// Realized vol input at the entry decision (bps over one bar).
    #[serde(default)]
    pub sigma_bar_bps: f64,
    /// Entry side's ask at the decision tick (the stability reference;
    /// 0 on hedge/completion legs).
    #[serde(default)]
    pub side_ask_at_decision: f64,
    /// Trailing min of the entry side's ask over the 10/20/40 seconds
    /// before the decision (stability-proxy diagnostics; None on
    /// hedge/completion legs).
    #[serde(default)]
    pub trail_min_ask_10s: Option<f64>,
    #[serde(default)]
    pub trail_min_ask_20s: Option<f64>,
    #[serde(default)]
    pub trail_min_ask_40s: Option<f64>,
    /// Result of the configured stability gate (true when the gate is off
    /// or for hedge/completion legs).
    #[serde(default)]
    pub stable_entry: bool,
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
    /// Optimistic passive-exit bound: pnl if the exit always filled at the
    /// side mid at the exit horizon (Some only when `exit_at_mid` is on and
    /// a mid existed at the horizon).
    #[serde(default)]
    pub pnl_exit_mid_optimistic: Option<f64>,
    /// Exogenous regime at entry decision (None when spot history is thin).
    #[serde(default)]
    pub regime_at_decision: Option<crate::regime::Regime>,
    /// Seconds from window open at entry decision.
    #[serde(default)]
    pub secs_from_open: u32,
    /// Spot return over the 10s before decision (bps).
    #[serde(default)]
    pub spot_ret_10s_bps: Option<f64>,
    /// Spot return over the 30s before decision (bps); diagnostic for whipsaw.
    #[serde(default)]
    pub spot_ret_30s_bps: Option<f64>,
    /// Spot return over the 60s before decision (bps).
    #[serde(default)]
    pub spot_ret_60s_bps: Option<f64>,
    /// Spot return over the 120s before decision (bps).
    #[serde(default)]
    pub spot_ret_120s_bps: Option<f64>,
    /// 60s change in perp-minus-spot basis, bps of spot (0 when no perp).
    #[serde(default)]
    pub basis_mom_60s_bps: f64,
    /// True when 30s spot momentum agrees with the entry side.
    #[serde(default)]
    pub side_aligned_30s: Option<bool>,
    /// True for a pair-completion leg (opposite-side buy locking the pair).
    #[serde(default)]
    pub is_completion: bool,
    /// Hybrid passive exit outcome: Some(true) = the resting mid ask filled
    /// (maker); Some(false) = timed out and converted to a crossing exit (or
    /// settled when no tick remained); None = hybrid off or no exit horizon.
    #[serde(default)]
    pub exit_filled_at_mid: Option<bool>,
    /// True when `fee_aware_exit` converted the scheduled exit into a hold
    /// to resolution (sell proceeds net of fees did not beat the hold EV).
    #[serde(default)]
    pub fee_hold: bool,
    /// Counterfactual P&L had the fee-held trade sold at the exit tick
    /// (net of entry fee and the exit-leg fee). Set only when `fee_hold`.
    #[serde(default)]
    pub hold_alt_sell_pnl: Option<f64>,
    /// Exit-leg fee the fee-held trade avoided. Set only when `fee_hold`.
    #[serde(default)]
    pub hold_alt_exit_fee: Option<f64>,
    /// True when the post-entry selldown stop fired (sold before close).
    #[serde(default)]
    pub stopped: bool,
    /// Counterfactual P&L had the stopped trade been held to resolution
    /// (entry fee only). Set only when `stopped`.
    #[serde(default)]
    pub stop_hold_pnl: Option<f64>,
    /// True when this entry filled as a resting maker bid (zero fee).
    #[serde(default)]
    pub maker_entry: bool,
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
    /// Directional continuation samples (only when `collect_training` is
    /// set and a >=0.5-sigma move is in progress at the sample instant).
    pub dir_samples: Vec<crate::directional::DirSample>,
    /// Minimum YES+NO touch cost observed in-window (real NO ladder only).
    pub min_pair_cost: Option<f64>,
    /// Fraction of ticks carrying a real NO ladder.
    pub real_no_coverage: f64,
    /// Resting maker entry orders placed (filled or cancelled); nonzero only
    /// when `maker_entry_offset >= 0`. Fill rate = maker trades / this.
    #[serde(default)]
    pub maker_placed: u32,
}
