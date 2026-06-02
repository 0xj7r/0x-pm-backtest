//! DeltaNeutralMM — pure spread-capture, designed to escape directional alpha.
//!
//! Thesis: every analysis so far shows what looks like directional edge is
//! actually long-YES bias. The fix isn't a better signal — it's a strategy
//! whose P&L is **structurally independent** of which side wins.
//!
//! Mechanism:
//!   1. Quote BOTH legs at the touch-tick (yes_ask - tick and no_ask - tick)
//!      as resting limit orders.
//!   2. Cancel + replace when the touch moves by >= tick.
//!   3. Hard inventory delta cap: if |yes_shares - no_shares| > cap, STOP
//!      emitting new orders on the heavy side AND cancel resting on that
//!      side. The runner-level inventory cancel does most of this for us;
//!      we just don't re-emit.
//!   4. Earn the spread × fill rate. Profitability requires the maker
//!      rebate program or natural cross-the-spread bid/ask matching.
//!
//! Why this beats PairedMmDense:
//!   * `PairedMmDense` emits ladders that accumulate one-sided exposure
//!     when the book trends.
//!   * `DeltaNeutralMM` quotes only at the touch and is aggressive about
//!     cancelling. Smaller resting footprint, less stuck inventory.

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

const BETTING_WINDOW_SECS: i64 = 300;

#[derive(Debug, Clone, Copy)]
pub struct DeltaNeutralMmConfig {
    pub tick: f64,
    /// Clip per rung in shares.
    pub clip_shares: f64,
    /// If |yes_shares - no_shares| >= this, stop emitting new orders on
    /// the over-filled side. Runner inventory-cancel handles existing rests.
    pub max_inventory_delta_shares: f64,
    /// Reject emission when (yes_ask + no_ask - 2*tick) > this. Same gate
    /// as PairedMmDense but at the rung level.
    pub max_pair_cost: f64,
    /// Minimum nanoseconds between same-leg emissions (prevents thrash).
    pub min_refresh_ns: i64,
    /// Stop emitting brand-new rungs when fewer than this many seconds
    /// remain to resolution (avoid being stuck holding inventory).
    pub stop_emit_secs_before_close: f32,
    pub price_min: f64,
    pub price_max: f64,
}

impl Default for DeltaNeutralMmConfig {
    fn default() -> Self {
        Self {
            tick: 0.01,
            clip_shares: 1.0,
            max_inventory_delta_shares: 1.0,
            max_pair_cost: 0.97,
            min_refresh_ns: 750_000_000, // 750ms
            stop_emit_secs_before_close: 30.0,
            price_min: 0.05,
            price_max: 0.95,
        }
    }
}

pub struct DeltaNeutralMm {
    cfg: DeltaNeutralMmConfig,
    last_yes_emit_ns: i64,
    last_no_emit_ns: i64,
    last_yes_price: f32,
    last_no_price: f32,
}

impl DeltaNeutralMm {
    pub fn new(cfg: DeltaNeutralMmConfig) -> Self {
        Self {
            cfg,
            last_yes_emit_ns: i64::MIN / 2,
            last_no_emit_ns: i64::MIN / 2,
            last_yes_price: 0.0,
            last_no_price: 0.0,
        }
    }
}

impl Strategy for DeltaNeutralMm {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        _spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        let yes_bid = event.yes_bid as f64;
        let yes_ask = event.yes_ask as f64;
        if !yes_bid.is_finite() || !yes_ask.is_finite() || yes_bid <= 0.0 || yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }
        if yes_bid >= yes_ask {
            return StrategyOutput::hold();
        }
        // Time-to-close gate.
        let window_open_ns = ctx.market_close_ns - BETTING_WINDOW_SECS * 1_000_000_000;
        let secs_in_window = ((event.ts_ns - window_open_ns) as f64 / 1e9) as f32;
        if !(0.0..=BETTING_WINDOW_SECS as f32).contains(&secs_in_window) {
            return StrategyOutput::hold();
        }
        let secs_to_close = BETTING_WINDOW_SECS as f32 - secs_in_window;
        if secs_to_close <= self.cfg.stop_emit_secs_before_close {
            return StrategyOutput::hold();
        }
        // Pair cost gate (same math as PairedMmDense).
        let yes_rung = yes_ask - self.cfg.tick;
        let no_ask = (1.0 - yes_bid).max(self.cfg.price_min);
        let no_rung = no_ask - self.cfg.tick;
        if yes_rung + no_rung > self.cfg.max_pair_cost {
            return StrategyOutput::hold();
        }

        // Inventory-delta gating per leg.
        let delta = ctx.yes_shares - ctx.no_shares;
        let skip_yes = delta >= self.cfg.max_inventory_delta_shares
            || (event.ts_ns - self.last_yes_emit_ns) < self.cfg.min_refresh_ns
            || (self.last_yes_price as f64 - yes_rung).abs() < self.cfg.tick * 0.5;
        let skip_no = -delta >= self.cfg.max_inventory_delta_shares
            || (event.ts_ns - self.last_no_emit_ns) < self.cfg.min_refresh_ns
            || (self.last_no_price as f64 - no_rung).abs() < self.cfg.tick * 0.5;

        let mut orders = Vec::new();
        if !skip_yes && yes_rung >= self.cfg.price_min && yes_rung <= self.cfg.price_max {
            orders.push(OrderRequest {
                side: Side::BuyYes,
                shares: self.cfg.clip_shares,
                max_depth: 1,
                limit_price: Some(yes_rung as f32),
                tag: "dnmm_yes",
            });
            self.last_yes_emit_ns = event.ts_ns;
            self.last_yes_price = yes_rung as f32;
        }
        if !skip_no && no_rung >= self.cfg.price_min && no_rung <= self.cfg.price_max {
            orders.push(OrderRequest {
                side: Side::BuyNo,
                shares: self.cfg.clip_shares,
                max_depth: 1,
                limit_price: Some(no_rung as f32),
                tag: "dnmm_no",
            });
            self.last_no_emit_ns = event.ts_ns;
            self.last_no_price = no_rung as f32;
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

    #[test]
    fn quotes_both_legs_when_book_open_and_pair_cost_ok() {
        let close_ns: i64 = 100_000_000_000_000;
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
        };
        let mut s = DeltaNeutralMm::new(DeltaNeutralMmConfig {
            max_pair_cost: 1.05, // permissive for this test fixture
            ..DeltaNeutralMmConfig::default()
        });
        // 200s before close, mid 0.41/0.43 — pair_cost = 0.42 + 0.56 = 0.98, ok with 1.05 cap
        let out = s.on_event(
            &evt(close_ns - 200 * 1_000_000_000, 0.41, 0.43),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 2);
        assert!(out.orders.iter().any(|o| matches!(o.side, Side::BuyYes)));
        assert!(out.orders.iter().any(|o| matches!(o.side, Side::BuyNo)));
        // Both should be limit orders below their respective asks
        for o in &out.orders {
            let limit = o.limit_price.expect("limit order");
            match o.side {
                Side::BuyYes => assert!(limit < 0.43, "yes limit {limit}"),
                Side::BuyNo => {
                    let no_ask = 1.0 - 0.41;
                    assert!((limit as f64) < no_ask, "no limit {limit}");
                }
                _ => panic!(),
            }
        }
    }

    #[test]
    fn skips_overfilled_leg() {
        let close_ns: i64 = 100_000_000_000_000;
        let ctx = Ctx {
            events_seen: 1,
            yes_shares: 5.0,
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
        };
        let mut s = DeltaNeutralMm::new(DeltaNeutralMmConfig {
            max_pair_cost: 1.05,
            max_inventory_delta_shares: 1.0,
            ..DeltaNeutralMmConfig::default()
        });
        let out = s.on_event(
            &evt(close_ns - 200 * 1_000_000_000, 0.41, 0.43),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        // delta = 5 > 1 → skip yes; should emit only no
        let yes_count = out
            .orders
            .iter()
            .filter(|o| matches!(o.side, Side::BuyYes))
            .count();
        let no_count = out
            .orders
            .iter()
            .filter(|o| matches!(o.side, Side::BuyNo))
            .count();
        assert_eq!(yes_count, 0);
        assert_eq!(no_count, 1);
    }

    #[test]
    fn stops_emitting_near_resolution() {
        let close_ns: i64 = 100_000_000_000_000;
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
        };
        let mut s = DeltaNeutralMm::new(DeltaNeutralMmConfig::default());
        // 15s to close — should hold
        let out = s.on_event(
            &evt(close_ns - 15 * 1_000_000_000, 0.41, 0.43),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }
}
