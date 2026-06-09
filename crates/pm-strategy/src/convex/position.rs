use crate::Side;
use crate::convex::signal::Conviction;

const BETTING_WINDOW_SECS: f32 = 300.0;

/// Real both-leg top-of-book prices for one event.
#[derive(Debug, Clone, Copy)]
pub struct BothBookPrices {
    pub yes_ask: f32,
    pub yes_bid: f32,
    pub no_ask: f32,
    pub no_bid: f32,
}
impl BothBookPrices {
    pub(crate) fn ask(&self, side: Side) -> f32 {
        match side {
            Side::BuyYes | Side::SellNo => self.yes_ask,
            Side::BuyNo | Side::SellYes => self.no_ask,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PositionConfig {
    /// Reserved for Plan 4 sizing-curve tuning (bankroll-relative clip scaling).
    pub bankroll_usdc: f64,
    pub max_clip_usdc: f64,
    pub favourite_start_secs: f32,
    pub favourite_min_ask: f32,
    pub favourite_max_ask: f32,
    pub favourite_clip_frac: f64,
    pub favourite_max_clips: usize,
    pub favourite_refresh_secs: f32,
    pub favourite_sweep_depth: usize,
    pub tail_min_ask: f32,
    pub tail_max_ask: f32,
    pub tail_min_seconds_to_close: f32,
    pub tail_max_clips: usize,
    pub tail_sweep_depth: usize,
    pub tail_refresh_secs: f32,
    pub tail_coverage_frac: f64,
    /// Reserved for Plan 4 sizing-curve tuning (|yes_mid-0.5| skew gate on the tail).
    pub tail_extreme_skew: f32,
}
impl Default for PositionConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 1000.0, max_clip_usdc: 30.0,
            favourite_start_secs: 180.0, favourite_min_ask: 0.70, favourite_max_ask: 0.97,
            favourite_clip_frac: 1.0, favourite_max_clips: 12, favourite_refresh_secs: 4.0,
            favourite_sweep_depth: 7,
            tail_min_ask: 0.01, tail_max_ask: 0.10, tail_min_seconds_to_close: 10.0,
            tail_max_clips: 3, tail_sweep_depth: 3, tail_refresh_secs: 5.0,
            tail_coverage_frac: 0.50, tail_extreme_skew: 0.20,
        }
    }
}

/// One leg to acquire this tick. `price_ref` is the reference ask used for sizing;
/// ExecutionPolicy decides the actual order price/posture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetLeg {
    pub side: Side,
    pub shares: f64,
    pub max_depth: usize,
    pub price_ref: f32,
}

#[derive(Debug, Clone, Default)]
pub struct TargetIncrement {
    pub legs: Vec<TargetLeg>,
}

/// Stateful per-market convex-book accumulator. One instance per market (the
/// engine clones the strategy per market, so this state is naturally isolated).
#[derive(Clone)]
pub struct PositionManager {
    cfg: PositionConfig,
    favourite_side: Option<Side>,
    favourite_clips: usize,
    /// Reserved for Plan 4 sizing-curve tuning (per-share inventory accounting).
    favourite_shares: f64,
    favourite_notional: f64,
    last_favourite_secs: f32,
    tail_clips: usize,
    tail_notional: f64,
    last_tail_secs: f32,
}

fn shares_capped(usdc: f64, px: f32) -> f64 {
    if px <= 0.0 { return 0.0; }
    ((usdc * 0.98 / px as f64) * 1000.0).floor() / 1000.0
}

impl PositionManager {
    pub fn new(cfg: PositionConfig) -> Self {
        Self {
            cfg,
            favourite_side: None, favourite_clips: 0, favourite_shares: 0.0,
            favourite_notional: 0.0, last_favourite_secs: f32::INFINITY,
            tail_clips: 0, tail_notional: 0.0, last_tail_secs: f32::INFINITY,
        }
    }

    /// `conviction` is `Some` only when the model SUPPORTS the favourite this tick.
    /// The favourite leg loads only under a current supported conviction (br2
    /// re-gates the favourite by model support on every fire); the convex tail
    /// runs off internal inventory regardless, so it can still fire opportunistically
    /// on ticks where the model gate currently fails.
    pub fn plan(&mut self, conviction: Option<&Conviction>, prices: &BothBookPrices, secs_to_close: f32) -> TargetIncrement {
        let mut legs = Vec::new();
        let secs_in = (BETTING_WINDOW_SECS - secs_to_close).clamp(0.0, BETTING_WINDOW_SECS);

        // Favourite leg (directional PnL engine): only on a current supported conviction.
        if let Some(conv) = conviction {
            let fav_ask = prices.ask(conv.favourite);
            let side_locked_ok = self.favourite_side.map_or(true, |s| s == conv.favourite);
            let refresh_ok = (self.last_favourite_secs - secs_to_close).abs() >= self.cfg.favourite_refresh_secs
                || self.favourite_clips == 0;
            if secs_in >= self.cfg.favourite_start_secs
                && self.favourite_clips < self.cfg.favourite_max_clips
                && side_locked_ok
                && refresh_ok
                && fav_ask >= self.cfg.favourite_min_ask
                && fav_ask <= self.cfg.favourite_max_ask
            {
                let clip_usdc = self.cfg.max_clip_usdc * self.cfg.favourite_clip_frac;
                let shares = shares_capped(clip_usdc, fav_ask);
                if shares > 0.0 {
                    legs.push(TargetLeg {
                        side: conv.favourite, shares,
                        max_depth: self.cfg.favourite_sweep_depth, price_ref: fav_ask,
                    });
                    self.favourite_side = Some(conv.favourite);
                    self.favourite_clips += 1;
                    self.favourite_shares += shares;
                    self.favourite_notional += shares * fav_ask as f64;
                    self.last_favourite_secs = secs_to_close;
                }
            }
        }

        // Convex tail leg (cheap opposite side)
        if let Some(fav) = self.favourite_side {
            let tail_side = opposite(fav);
            let tail_ask = prices.ask(tail_side);
            let tail_refresh_ok = (self.last_tail_secs - secs_to_close).abs() >= self.cfg.tail_refresh_secs
                || self.tail_clips == 0;
            if self.favourite_notional > 0.0
                && self.tail_clips < self.cfg.tail_max_clips
                && tail_refresh_ok
                && secs_to_close >= self.cfg.tail_min_seconds_to_close
                && tail_ask >= self.cfg.tail_min_ask
                && tail_ask <= self.cfg.tail_max_ask
            {
                let target = self.favourite_notional * self.cfg.tail_coverage_frac * tail_ask as f64;
                let clip_usdc = (target - self.tail_notional).max(0.0);
                let shares = shares_capped(clip_usdc, tail_ask);
                if shares > 0.0 {
                    legs.push(TargetLeg {
                        side: tail_side, shares,
                        max_depth: self.cfg.tail_sweep_depth, price_ref: tail_ask,
                    });
                    self.tail_clips += 1;
                    self.tail_notional += shares * tail_ask as f64;
                    self.last_tail_secs = secs_to_close;
                }
            }
        }

        TargetIncrement { legs }
    }
}

fn opposite(side: Side) -> Side {
    match side {
        Side::BuyYes => Side::BuyNo,
        Side::BuyNo => Side::BuyYes,
        Side::SellYes => Side::SellNo,
        Side::SellNo => Side::SellYes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Side;

    fn conv(side: Side, side_p: f32, edge: f32) -> Conviction {
        Conviction { favourite: side, side_p, edge, confidence: 0.75, risk: 0.3 }
    }
    fn prices(yes_ask: f32, yes_bid: f32, no_ask: f32, no_bid: f32) -> BothBookPrices {
        BothBookPrices { yes_ask, yes_bid, no_ask, no_bid }
    }

    #[test]
    fn loads_favourite_late_within_ask_range() {
        let mut pm = PositionManager::new(PositionConfig::default());
        let inc = pm.plan(Some(&conv(Side::BuyYes, 0.86, 0.06)), &prices(0.80, 0.79, 0.21, 0.19), 100.0);
        let fav = inc.legs.iter().find(|l| l.side == Side::BuyYes).expect("favourite leg");
        assert!(fav.shares > 0.0, "favourite clip should size > 0");
    }

    #[test]
    fn no_favourite_load_before_start_secs() {
        let mut pm = PositionManager::new(PositionConfig::default());
        let inc = pm.plan(Some(&conv(Side::BuyYes, 0.86, 0.06)), &prices(0.80, 0.79, 0.21, 0.19), 200.0);
        assert!(inc.legs.iter().all(|l| l.side != Side::BuyYes), "no favourite before start");
    }

    #[test]
    fn adds_cheap_convex_tail_after_favourite_built() {
        let mut pm = PositionManager::new(PositionConfig::default());
        let _ = pm.plan(Some(&conv(Side::BuyYes, 0.86, 0.06)), &prices(0.80, 0.79, 0.21, 0.19), 100.0);
        let inc = pm.plan(Some(&conv(Side::BuyYes, 0.90, 0.06)), &prices(0.92, 0.91, 0.09, 0.07), 60.0);
        let tail = inc.legs.iter().find(|l| l.side == Side::BuyNo).expect("tail leg");
        assert!(tail.shares > 0.0, "cheap tail should size > 0 once favourite exists");
    }

    #[test]
    fn favourite_clips_respect_max_clips() {
        let mut pm = PositionManager::new(PositionConfig { favourite_max_clips: 1, ..PositionConfig::default() });
        let _ = pm.plan(Some(&conv(Side::BuyYes, 0.86, 0.06)), &prices(0.80, 0.79, 0.21, 0.19), 100.0);
        let inc2 = pm.plan(Some(&conv(Side::BuyYes, 0.86, 0.06)), &prices(0.80, 0.79, 0.21, 0.19), 90.0);
        assert!(inc2.legs.iter().all(|l| l.side != Side::BuyYes), "favourite capped at max_clips");
    }

    #[test]
    fn no_favourite_without_current_conviction_but_tail_still_fires() {
        let mut pm = PositionManager::new(PositionConfig::default());
        // Tick 1: model supports YES -> favourite loads.
        let _ = pm.plan(Some(&conv(Side::BuyYes, 0.86, 0.06)), &prices(0.80, 0.79, 0.21, 0.19), 100.0);
        // Tick 2: model no longer supports the favourite (conviction None) while the
        // favourite ask is still in range and NO is cheap. No second favourite clip,
        // but the convex tail still fires off existing inventory.
        let inc = pm.plan(None, &prices(0.92, 0.91, 0.09, 0.07), 60.0);
        assert!(inc.legs.iter().all(|l| l.side != Side::BuyYes), "no favourite without current support");
        let tail = inc.legs.iter().find(|l| l.side == Side::BuyNo).expect("tail leg");
        assert!(tail.shares > 0.0, "tail stays opportunistic without a current conviction");
    }
}
