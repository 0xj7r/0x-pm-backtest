//! SpotMomentumFollower — pure spot-driven directional strategy.
//!
//! Hypothesis: the market under-reacts to late BTC moves. Within the betting
//! window, if BTC has trended up in the last 30/60/120s with high
//! confidence, the YES (Up) side is undervalued. Take the cheap side as a
//! market order, hold to resolution.
//!
//! No book signal, no regime gating beyond an absolute momentum threshold.
//! Acts as a baseline to show the *isolated* edge from the spot tape.

use crate::spot_momentum::weighted_multi_tf_return;
use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory};

const BETTING_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone)]
pub struct SpotMomentumFollowerConfig {
    /// Only enter once we are this many seconds INTO the 5-min window.
    pub min_seconds_in_window: f32,
    /// Threshold on the multi-TF weighted spot return (fractional, e.g.
    /// 0.0003 = 30 bps).
    pub entry_threshold: f64,
    /// Dollar size of the directional clip.
    pub clip_usdc: f64,
    /// Hard ceiling on yes_ask above which we refuse to buy YES (anti-chase).
    pub max_ask_yes: f32,
    /// Hard floor on yes_bid below which we refuse to buy NO (i.e. avoid no_ask >= max_ask).
    pub min_bid_yes: f32,
}

impl Default for SpotMomentumFollowerConfig {
    fn default() -> Self {
        Self {
            min_seconds_in_window: 30.0,
            entry_threshold: 0.00002, // 0.2 bps weighted return — very permissive
            clip_usdc: 5.0,
            max_ask_yes: 0.94,
            min_bid_yes: 0.06,
        }
    }
}

pub struct SpotMomentumFollower {
    cfg: SpotMomentumFollowerConfig,
    fired: bool,
}

impl SpotMomentumFollower {
    pub fn new(cfg: SpotMomentumFollowerConfig) -> Self {
        Self { cfg, fired: false }
    }
}

impl Strategy for SpotMomentumFollower {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        _trades: &pm_types::TradeHistory,
    ) -> StrategyOutput {
        if self.fired {
            return StrategyOutput::hold();
        }
        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }
        let window_open_ns = ctx.market_close_ns - BETTING_WINDOW_SECS * 1_000_000_000;
        let secs_in_window = ((event.ts_ns - window_open_ns) as f64 / 1e9) as f32;
        if !(self.cfg.min_seconds_in_window..=BETTING_WINDOW_SECS as f32).contains(&secs_in_window)
        {
            return StrategyOutput::hold();
        }
        if spot.is_empty() {
            return StrategyOutput::hold();
        }
        let r = match weighted_multi_tf_return(event.ts_ns, spot) {
            Some(x) => x,
            None => return StrategyOutput::hold(),
        };
        if r.abs() < self.cfg.entry_threshold {
            return StrategyOutput::hold();
        }
        self.fired = true;
        let (side, fill_px) = if r > 0.0 {
            if event.yes_ask > self.cfg.max_ask_yes {
                return StrategyOutput::hold();
            }
            (Side::BuyYes, event.yes_ask)
        } else {
            let no_ask = 1.0 - event.yes_bid;
            if event.yes_bid < self.cfg.min_bid_yes {
                return StrategyOutput::hold();
            }
            (Side::BuyNo, no_ask)
        };
        // Size so `shares * fill_px` stays strictly < max_clip_usdc (avoid the
        // risk-gate floating-point boundary). Round down to 0.001-share precision.
        let raw_shares = (self.cfg.clip_usdc * 0.98) / fill_px as f64;
        let shares = ((raw_shares * 1000.0).floor() / 1000.0).max(0.0);
        if shares <= 0.0 {
            return StrategyOutput::hold();
        }
        StrategyOutput::one(OrderRequest {
            side,
            shares,
            max_depth: 1,
            limit_price: None,
            tag: "smf_directional",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{BookLevel, MarketId, ReplayFlags, SpotTick, tape::TAPE_DEPTH};

    fn evt(ts_ns: i64, bid: f32, ask: f32) -> ReplayEvent {
        let mut bids = [BookLevel::default(); TAPE_DEPTH];
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        bids[0] = BookLevel {
            price: bid,
            size: 200.0,
        };
        asks[0] = BookLevel {
            price: ask,
            size: 200.0,
        };
        ReplayEvent {
            ts_ns,
            market_id: MarketId(1),
            yes_mid: 0.5 * (bid + ask),
            yes_bid: bid,
            yes_ask: ask,
            volume: 0.0,
            bids,
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    fn spot_with_uptrend(now_ns: i64) -> SpotHistory {
        // 1% (100 bps) move over the last 120s — strong enough to clear the
        // weighted-multi-TF threshold (3 bps).
        let mut samples = Vec::new();
        for i in 0..240i64 {
            let secs_ago = (240 - i) as f64;
            let price = 80_000.0 * (1.0 + 0.01 * (i as f64 / 240.0));
            samples.push(SpotTick {
                ts_ns: now_ns - (secs_ago * 1_000_000_000.0) as i64,
                price,
                quantity: 0.1,
                is_buyer_maker: false,
            });
        }
        SpotHistory::new(samples)
    }

    #[test]
    fn fires_buy_yes_on_uptrend() {
        let close_ns: i64 = 100_000_000_000_000;
        let now_ns = close_ns - 100 * 1_000_000_000; // 100s before close
        let ctx = Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 100.0,
            market_yes_range_so_far: 0.0,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: None,
            model_attribution: None,
            market_close_ns: close_ns,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
            ..Ctx::default()
        };
        let mut s = SpotMomentumFollower::new(SpotMomentumFollowerConfig::default());
        let spot = spot_with_uptrend(now_ns);
        let out = s.on_event(
            &evt(now_ns, 0.49, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyYes);
    }

    #[test]
    fn holds_outside_window() {
        let close_ns: i64 = 100_000_000_000_000;
        let now_ns = close_ns - 600 * 1_000_000_000; // 10 min before close
        let ctx = Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 100.0,
            market_yes_range_so_far: 0.0,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: None,
            model_attribution: None,
            market_close_ns: close_ns,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
            ..Ctx::default()
        };
        let mut s = SpotMomentumFollower::new(SpotMomentumFollowerConfig::default());
        let spot = spot_with_uptrend(now_ns);
        let out = s.on_event(
            &evt(now_ns, 0.49, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }
}
