//! LateConfirmation — fires only when the book has *already* committed.
//!
//! Logic:
//!   * Stays dormant until the last `late_seconds` of the betting window.
//!   * Requires `yes_mid >= confirm_threshold` (or `<= 1 - confirm_threshold`)
//!     AND the spot-momentum signal agrees with that direction.
//!   * Takes the favored side at ask, holds to resolution.
//!
//! Expected profile: high hit rate (the book is already nearly certain), low
//! payoff per bet (paying $0.75+ per share, winning at $1). Complement to
//! LateDirectionalLag which trades coin-flip prices on signal leads.

use crate::signals::Ring;
use crate::spot_momentum::weighted_multi_tf_return;
use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

const BETTING_WINDOW_SECS: i64 = 300;
const SPOT_SCALE: f32 = 300.0;
const MOM_WINDOW: usize = 16;

#[derive(Debug, Clone)]
pub struct LateConfirmationConfig {
    pub bankroll_usdc: f64,
    pub max_clip_usdc: f64,
    pub late_seconds: f32,
    /// yes_mid must be >= this OR <= (1 - this) before we'll enter.
    pub confirm_threshold: f32,
    /// Spot momentum sign must agree with book direction; magnitude must be
    /// at least this absolute value.
    pub min_spot_alignment: f32,
    /// Max number of trades per market.
    pub max_clips: usize,
}

impl Default for LateConfirmationConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 1000.0,
            max_clip_usdc: 5.0,
            late_seconds: 60.0,
            confirm_threshold: 0.70,
            min_spot_alignment: 0.05,
            max_clips: 2,
        }
    }
}

pub struct LateConfirmation {
    cfg: LateConfirmationConfig,
    spot_mids: Ring,
    clips_used: usize,
    last_clip_ns: i64,
}

impl LateConfirmation {
    pub fn new(cfg: LateConfirmationConfig) -> Self {
        Self {
            cfg,
            spot_mids: Ring::new(MOM_WINDOW),
            clips_used: 0,
            last_clip_ns: i64::MIN / 2,
        }
    }
}

fn shares_capped(usdc: f64, fill_px: f32) -> f64 {
    let raw = (usdc * 0.98) / fill_px as f64;
    ((raw * 1000.0).floor() / 1000.0).max(0.0)
}

impl Strategy for LateConfirmation {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        self.spot_mids.push(event.yes_mid);
        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }
        if self.clips_used >= self.cfg.max_clips {
            return StrategyOutput::hold();
        }
        let window_open_ns = ctx.market_close_ns - BETTING_WINDOW_SECS * 1_000_000_000;
        let secs_in_window = ((event.ts_ns - window_open_ns) as f64 / 1e9) as f32;
        if !(0.0..=BETTING_WINDOW_SECS as f32).contains(&secs_in_window) {
            return StrategyOutput::hold();
        }
        let secs_to_close = BETTING_WINDOW_SECS as f32 - secs_in_window;
        if secs_to_close > self.cfg.late_seconds {
            return StrategyOutput::hold();
        }
        if (event.ts_ns - self.last_clip_ns) < 5_000_000_000 {
            return StrategyOutput::hold();
        }

        let book_picks_yes = event.yes_mid >= self.cfg.confirm_threshold;
        let book_picks_no = event.yes_mid <= (1.0 - self.cfg.confirm_threshold);
        if !book_picks_yes && !book_picks_no {
            return StrategyOutput::hold();
        }

        let spot_mom = if !spot.is_empty() {
            weighted_multi_tf_return(event.ts_ns, spot)
                .map(|r| (r as f32 * SPOT_SCALE).clamp(-1.0, 1.0))
                .unwrap_or(0.0)
        } else {
            0.0
        };

        let (side, fill_px) = if book_picks_yes && spot_mom >= self.cfg.min_spot_alignment {
            (Side::BuyYes, event.yes_ask)
        } else if book_picks_no && spot_mom <= -self.cfg.min_spot_alignment {
            (Side::BuyNo, (1.0 - event.yes_bid).max(0.01))
        } else {
            return StrategyOutput::hold();
        };

        let shares = shares_capped(self.cfg.max_clip_usdc, fill_px);
        if shares <= 0.0 {
            return StrategyOutput::hold();
        }
        self.clips_used += 1;
        self.last_clip_ns = event.ts_ns;
        StrategyOutput::one(OrderRequest {
            side,
            shares,
            max_depth: 1,
            limit_price: None,
            tag: "lconf",
        })
    }
}
