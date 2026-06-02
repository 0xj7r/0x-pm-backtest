//! LateConvexTail — small cheap bets on the *losing* side at extreme prices.
//!
//! Logic:
//!   * Fires when `yes_mid` is extreme (≥0.85 or ≤0.15) — book is near-cert.
//!   * Takes the OPPOSITE (losing/cheap) side at its ask — typically $0.05–$0.15.
//!   * Asymmetric payoff: if the market reverts, the cheap side pays $1.00
//!     (6-20x payoff). If it doesn't, lose the small clip.
//!   * Plays exactly the "reversion pop" left-tail event.
//!
//! Expected profile: ~5-15% hit rate, large payoff per win. Bounded loss
//! per market (clip is small). Smooths the equity curve of directional
//! strategies that lose hard on the wrong side.

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

const BETTING_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone)]
pub struct LateConvexTailConfig {
    pub bankroll_usdc: f64,
    pub max_clip_usdc: f64,
    /// Strategy only fires within the last `late_seconds` of the window.
    pub late_seconds: f32,
    /// Required absolute distance of yes_mid from 0.5 before firing.
    pub extreme_threshold: f32,
    /// Maximum acceptable ask on the *cheap* side. Default 0.20.
    pub max_cheap_ask: f32,
    /// Minimum acceptable ask on the cheap side (floor — below this, the
    /// market really is dead and the tail bet rarely pays).
    pub min_cheap_ask: f32,
}

impl Default for LateConvexTailConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 1000.0,
            max_clip_usdc: 1.0,
            late_seconds: 90.0,
            extreme_threshold: 0.35, // yes_mid ≥ 0.85 or ≤ 0.15
            max_cheap_ask: 0.18,
            min_cheap_ask: 0.03,
        }
    }
}

pub struct LateConvexTail {
    cfg: LateConvexTailConfig,
    fired: bool,
}

impl LateConvexTail {
    pub fn new(cfg: LateConvexTailConfig) -> Self {
        Self { cfg, fired: false }
    }
}

fn shares_capped(usdc: f64, fill_px: f32) -> f64 {
    let raw = (usdc * 0.98) / fill_px as f64;
    ((raw * 1000.0).floor() / 1000.0).max(0.0)
}

impl Strategy for LateConvexTail {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        _spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        if self.fired {
            return StrategyOutput::hold();
        }
        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
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

        let skew = (event.yes_mid - 0.5).abs();
        if skew < self.cfg.extreme_threshold {
            return StrategyOutput::hold();
        }

        // Buy the CHEAP side (opposite of where yes_mid points).
        let (side, fill_px) = if event.yes_mid > 0.5 {
            // YES is winning; NO is cheap.
            let no_ask = (1.0 - event.yes_bid).max(0.01);
            (Side::BuyNo, no_ask)
        } else {
            (Side::BuyYes, event.yes_ask)
        };
        if fill_px < self.cfg.min_cheap_ask || fill_px > self.cfg.max_cheap_ask {
            return StrategyOutput::hold();
        }

        let shares = shares_capped(self.cfg.max_clip_usdc, fill_px);
        if shares <= 0.0 {
            return StrategyOutput::hold();
        }
        self.fired = true;
        StrategyOutput::one(OrderRequest {
            side,
            shares,
            max_depth: 1,
            limit_price: None,
            tag: "lctail",
        })
    }
}
