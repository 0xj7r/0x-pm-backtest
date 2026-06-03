//! Last-second window-delta snipe.
//!
//! This tests the "window delta is king near close" hypothesis in the normal
//! Rust replay engine. It uses only replay-safe information available at the
//! decision event: Binance spot at market open, current Binance spot, and the
//! current Polymarket book. The runner owns taker latency, fill price, risk, and
//! settlement accounting.

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

const BETTING_WINDOW_NS: i64 = 300_000_000_000;

#[derive(Debug, Clone)]
pub struct LastSecondSnipeConfig {
    /// Emit once the replay reaches this many seconds before close.
    pub decision_seconds_to_close: f32,
    /// Do not emit if there is less time than this before close.
    pub min_seconds_to_close: f32,
    /// Absolute spot move from window open required to choose a side.
    pub min_abs_delta_bps: f64,
    /// Refuse to pay more than this for either YES or NO.
    pub max_entry_price: f32,
    /// Dollar notional target for the taker order.
    pub clip_usdc: f64,
    /// Book depth the taker order may sweep.
    pub max_depth: usize,
}

impl Default for LastSecondSnipeConfig {
    fn default() -> Self {
        Self {
            decision_seconds_to_close: 5.0,
            min_seconds_to_close: 1.0,
            min_abs_delta_bps: 1.0,
            max_entry_price: 0.98,
            clip_usdc: 5.0,
            max_depth: 1,
        }
    }
}

pub struct LastSecondSnipe {
    cfg: LastSecondSnipeConfig,
    fired: bool,
}

impl LastSecondSnipe {
    pub fn new(cfg: LastSecondSnipeConfig) -> Self {
        Self { cfg, fired: false }
    }
}

fn shares_capped(usdc: f64, fill_px: f32) -> f64 {
    if fill_px <= 0.0 {
        return 0.0;
    }
    let raw = (usdc * 0.98) / fill_px as f64;
    ((raw * 1000.0).floor() / 1000.0).max(0.0)
}

fn no_ask(event: &ReplayEvent) -> f32 {
    (1.0 - event.yes_bid).clamp(0.0, 1.0)
}

impl Strategy for LastSecondSnipe {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        if self.fired || spot.is_empty() || ctx.market_close_ns <= BETTING_WINDOW_NS {
            return StrategyOutput::hold();
        }
        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }

        let seconds_to_close = (ctx.market_close_ns - event.ts_ns) as f32 / 1e9;
        if seconds_to_close > self.cfg.decision_seconds_to_close
            || seconds_to_close < self.cfg.min_seconds_to_close
        {
            return StrategyOutput::hold();
        }

        let market_open_ns = ctx.market_close_ns - BETTING_WINDOW_NS;
        let Some(open_px) = spot.price_at_or_before(market_open_ns) else {
            return StrategyOutput::hold();
        };
        let Some(now_px) = spot.price_at_or_before(event.ts_ns) else {
            return StrategyOutput::hold();
        };
        if open_px <= 0.0 || !open_px.is_finite() || !now_px.is_finite() {
            return StrategyOutput::hold();
        }

        let delta_bps = (now_px / open_px - 1.0) * 10_000.0;
        if delta_bps.abs() < self.cfg.min_abs_delta_bps {
            return StrategyOutput::hold();
        }

        let (side, fill_px) = if delta_bps >= 0.0 {
            (Side::BuyYes, event.yes_ask)
        } else {
            (Side::BuyNo, no_ask(event))
        };
        if fill_px <= 0.0 || fill_px > self.cfg.max_entry_price {
            return StrategyOutput::hold();
        }

        let shares = shares_capped(self.cfg.clip_usdc, fill_px);
        if shares <= 0.0 {
            return StrategyOutput::hold();
        }
        self.fired = true;
        StrategyOutput::one(OrderRequest {
            side,
            shares,
            max_depth: self.cfg.max_depth.max(1),
            limit_price: Some(self.cfg.max_entry_price),
            tag: "last_second_snipe",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{BookLevel, MarketId, ReplayFlags, SpotTick, tape::TAPE_DEPTH};

    fn event(ts_ns: i64, yes_bid: f32, yes_ask: f32) -> ReplayEvent {
        let mut bids = [BookLevel::default(); TAPE_DEPTH];
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        bids[0] = BookLevel {
            price: yes_bid,
            size: 100.0,
        };
        asks[0] = BookLevel {
            price: yes_ask,
            size: 100.0,
        };
        ReplayEvent {
            ts_ns,
            market_id: MarketId(1),
            yes_mid: 0.5 * (yes_bid + yes_ask),
            yes_bid,
            yes_ask,
            volume: 0.0,
            bids,
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    fn ctx(close_ns: i64) -> Ctx {
        Ctx {
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
        }
    }

    #[test]
    fn buys_yes_when_window_delta_is_positive_near_close() {
        let close_ns = 1_000_000_000_000;
        let open_ns = close_ns - BETTING_WINDOW_NS;
        let now_ns = close_ns - 5_000_000_000;
        let spot = SpotHistory::new(vec![
            SpotTick {
                ts_ns: open_ns,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 100.02,
                quantity: 1.0,
                is_buyer_maker: false,
            },
        ]);
        let mut s = LastSecondSnipe::new(LastSecondSnipeConfig {
            clip_usdc: 10.0,
            ..LastSecondSnipeConfig::default()
        });

        let out = s.on_event(
            &event(now_ns, 0.83, 0.85),
            &ctx(close_ns),
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyYes);
        assert_eq!(out.orders[0].limit_price, Some(0.98));
        assert_eq!(out.orders[0].tag, "last_second_snipe");
    }

    #[test]
    fn buys_no_when_window_delta_is_negative_near_close() {
        let close_ns = 1_000_000_000_000;
        let open_ns = close_ns - BETTING_WINDOW_NS;
        let now_ns = close_ns - 5_000_000_000;
        let spot = SpotHistory::new(vec![
            SpotTick {
                ts_ns: open_ns,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 99.98,
                quantity: 1.0,
                is_buyer_maker: true,
            },
        ]);
        let mut s = LastSecondSnipe::new(LastSecondSnipeConfig::default());

        let out = s.on_event(
            &event(now_ns, 0.15, 0.17),
            &ctx(close_ns),
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyNo);
    }

    #[test]
    fn refuses_to_chase_above_max_price() {
        let close_ns = 1_000_000_000_000;
        let open_ns = close_ns - BETTING_WINDOW_NS;
        let now_ns = close_ns - 5_000_000_000;
        let spot = SpotHistory::new(vec![
            SpotTick {
                ts_ns: open_ns,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 100.02,
                quantity: 1.0,
                is_buyer_maker: false,
            },
        ]);
        let mut s = LastSecondSnipe::new(LastSecondSnipeConfig {
            max_entry_price: 0.80,
            ..LastSecondSnipeConfig::default()
        });

        let out = s.on_event(
            &event(now_ns, 0.88, 0.90),
            &ctx(close_ns),
            &spot,
            &TradeHistory::default(),
        );

        assert!(out.orders.is_empty());
    }
}
