//! UnlawfulRecycler — tight two-sided maker/redeem sleeve.
//!
//! This mirrors the historical Unlawful-Shear shape: tiny high-cadence maker
//! bids on both outcomes, strict pair-cost control, and inventory repair before
//! directional exposure grows. It is intentionally separate from
//! `PairedMmDense`; that strategy is a generic ladder, while this one is a
//! paired-cost recycler.

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

const BETTING_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone, Copy)]
pub struct UnlawfulRecyclerConfig {
    pub tick: f64,
    pub clip_shares: f64,
    pub max_pair_cost: f64,
    pub min_price: f64,
    pub max_price: f64,
    pub max_inventory_delta_shares: f64,
    pub repair_inventory_delta_shares: f64,
    pub min_refresh_ns: i64,
    pub max_orders_per_leg: usize,
    pub start_secs: f32,
    pub stop_secs_before_close: f32,
}

impl Default for UnlawfulRecyclerConfig {
    fn default() -> Self {
        Self {
            tick: 0.01,
            clip_shares: 5.0,
            // 1c spread at touch-tick gives roughly 0.99 pair cost:
            // (yes_ask - 0.01) + (no_ask - 0.01).
            max_pair_cost: 0.99,
            min_price: 0.03,
            max_price: 0.97,
            max_inventory_delta_shares: 25.0,
            repair_inventory_delta_shares: 5.0,
            min_refresh_ns: 250_000_000,
            max_orders_per_leg: 500,
            start_secs: 0.0,
            stop_secs_before_close: 20.0,
        }
    }
}

pub struct UnlawfulRecycler {
    cfg: UnlawfulRecyclerConfig,
    yes_emitted: usize,
    no_emitted: usize,
    last_yes_emit_ns: i64,
    last_no_emit_ns: i64,
}

impl UnlawfulRecycler {
    pub fn new(cfg: UnlawfulRecyclerConfig) -> Self {
        Self {
            cfg,
            yes_emitted: 0,
            no_emitted: 0,
            last_yes_emit_ns: i64::MIN / 2,
            last_no_emit_ns: i64::MIN / 2,
        }
    }
}

fn book_valid(event: &ReplayEvent) -> bool {
    let yes_bid = event.yes_bid as f64;
    let yes_ask = event.yes_ask as f64;
    yes_bid.is_finite()
        && yes_ask.is_finite()
        && yes_bid > 0.0
        && yes_ask > 0.0
        && yes_bid < yes_ask
        && yes_ask < 1.0
}

impl Strategy for UnlawfulRecycler {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        _spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        if !book_valid(event) {
            return StrategyOutput::hold();
        }

        let window_open_ns = ctx.market_close_ns - BETTING_WINDOW_SECS * 1_000_000_000;
        let secs_in = ((event.ts_ns - window_open_ns) as f64 / 1e9) as f32;
        if secs_in < self.cfg.start_secs || secs_in > BETTING_WINDOW_SECS as f32 {
            return StrategyOutput::hold();
        }
        let secs_to_close = BETTING_WINDOW_SECS as f32 - secs_in;
        if secs_to_close <= self.cfg.stop_secs_before_close {
            return StrategyOutput::hold();
        }

        let yes_bid = event.yes_bid as f64;
        let yes_ask = event.yes_ask as f64;
        let no_ask = (1.0 - yes_bid).clamp(0.0, 1.0);
        let yes_px = yes_ask - self.cfg.tick;
        let no_px = no_ask - self.cfg.tick;
        if yes_px + no_px > self.cfg.max_pair_cost {
            return StrategyOutput::hold();
        }

        let delta = ctx.yes_shares - ctx.no_shares;
        let repair_yes = delta <= -self.cfg.repair_inventory_delta_shares;
        let repair_no = delta >= self.cfg.repair_inventory_delta_shares;
        let too_long_yes = delta >= self.cfg.max_inventory_delta_shares;
        let too_long_no = -delta >= self.cfg.max_inventory_delta_shares;

        let can_yes = !too_long_yes
            && !repair_no
            && self.yes_emitted < self.cfg.max_orders_per_leg
            && (event.ts_ns - self.last_yes_emit_ns) >= self.cfg.min_refresh_ns
            && yes_px >= self.cfg.min_price
            && yes_px <= self.cfg.max_price;
        let can_no = !too_long_no
            && !repair_yes
            && self.no_emitted < self.cfg.max_orders_per_leg
            && (event.ts_ns - self.last_no_emit_ns) >= self.cfg.min_refresh_ns
            && no_px >= self.cfg.min_price
            && no_px <= self.cfg.max_price;

        let mut orders = Vec::with_capacity(2);
        if can_yes {
            orders.push(OrderRequest {
                side: Side::BuyYes,
                shares: self.cfg.clip_shares,
                max_depth: 1,
                limit_price: Some(yes_px as f32),
                tag: "ur_yes",
            });
            self.yes_emitted += 1;
            self.last_yes_emit_ns = event.ts_ns;
        }
        if can_no {
            orders.push(OrderRequest {
                side: Side::BuyNo,
                shares: self.cfg.clip_shares,
                max_depth: 1,
                limit_price: Some(no_px as f32),
                tag: "ur_no",
            });
            self.no_emitted += 1;
            self.last_no_emit_ns = event.ts_ns;
        }

        StrategyOutput { orders }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{BookLevel, MarketId, ReplayFlags, tape::TAPE_DEPTH};

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

    fn ctx(delta_yes: f64, close_ns: i64) -> Ctx {
        Ctx {
            events_seen: 1,
            yes_shares: delta_yes.max(0.0),
            no_shares: (-delta_yes).max(0.0),
            cash_usdc: 1000.0,
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
    fn emits_tandem_quotes_for_tight_spread() {
        let close_ns = 300_000_000_000;
        let mut s = UnlawfulRecycler::new(UnlawfulRecyclerConfig::default());
        let out = s.on_event(
            &evt(10_000_000_000, 0.49, 0.50),
            &ctx(0.0, close_ns),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 2);
        assert!(out.orders.iter().any(|o| o.tag == "ur_yes"));
        assert!(out.orders.iter().any(|o| o.tag == "ur_no"));
    }

    #[test]
    fn skips_when_pair_cost_is_too_high() {
        let close_ns = 300_000_000_000;
        let mut s = UnlawfulRecycler::new(UnlawfulRecyclerConfig::default());
        let out = s.on_event(
            &evt(10_000_000_000, 0.48, 0.51),
            &ctx(0.0, close_ns),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }

    #[test]
    fn repairs_inventory_by_quoting_underfilled_side_only() {
        let close_ns = 300_000_000_000;
        let mut s = UnlawfulRecycler::new(UnlawfulRecyclerConfig::default());
        let out = s.on_event(
            &evt(10_000_000_000, 0.49, 0.50),
            &ctx(10.0, close_ns),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].tag, "ur_no");
    }

    #[test]
    fn stops_before_resolution() {
        let close_ns = 300_000_000_000;
        let mut s = UnlawfulRecycler::new(UnlawfulRecyclerConfig::default());
        let out = s.on_event(
            &evt(close_ns - 10_000_000_000, 0.49, 0.50),
            &ctx(0.0, close_ns),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }
}
