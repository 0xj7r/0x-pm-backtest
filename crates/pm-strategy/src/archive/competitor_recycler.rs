//! CompetitorRecycler — bid-side paired inventory accumulation.
//!
//! This is a dedicated profile for the wallet-derived pattern: accumulate both
//! outcomes only when the combined bid-side pair cost is attractive, keep
//! residual bounded, and optionally allow a spot-confirmed lean.

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput, regime::WhipsawRiskSnapshot};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

const BETTING_WINDOW_SECS: i64 = 300;
const NS_PER_SEC: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairQuoteMode {
    PassiveBid,
    SharedSlackEven,
    SharedSlackLeader,
}

#[derive(Debug, Clone, Copy)]
pub struct CompetitorRecyclerConfig {
    pub child_clip_shares: f64,
    pub max_leg_shares: f64,
    pub max_pair_cost: f64,
    pub repair_delta_shares: f64,
    pub lean_delta_shares: f64,
    pub min_abs_delta_bps_for_lean: f64,
    pub stop_secs_before_close: f32,
    pub max_spot_30s_abs_bps: f64,
    pub min_regime_realized_vol_180s_bps: f32,
    pub min_regime_sign_flip_rate: f32,
    pub max_regime_path_efficiency: f32,
    pub max_abs_spot_flow_30s: f64,
    pub stress_warmup_events: u64,
    pub max_attractive_pair_frac_so_far: f64,
    pub min_top_bid_ask_size_ratio: f64,
    pub stress_clip_multiplier: f64,
    pub min_mid: f64,
    pub max_mid: f64,
    pub min_refresh_ns: i64,
    pub max_orders_per_leg: usize,
    pub quote_mode: PairQuoteMode,
}

impl Default for CompetitorRecyclerConfig {
    fn default() -> Self {
        Self {
            child_clip_shares: 10.0,
            max_leg_shares: 5_000.0,
            max_pair_cost: 0.970,
            repair_delta_shares: 4.0,
            lean_delta_shares: 30.0,
            min_abs_delta_bps_for_lean: 5.0,
            stop_secs_before_close: 30.0,
            max_spot_30s_abs_bps: 8.0,
            min_regime_realized_vol_180s_bps: 0.0,
            min_regime_sign_flip_rate: 0.0,
            max_regime_path_efficiency: 1.0,
            max_abs_spot_flow_30s: f64::INFINITY,
            stress_warmup_events: 20,
            max_attractive_pair_frac_so_far: f64::INFINITY,
            min_top_bid_ask_size_ratio: 0.0,
            stress_clip_multiplier: 0.0,
            min_mid: 0.20,
            max_mid: 0.80,
            min_refresh_ns: 250_000_000,
            max_orders_per_leg: 1_000,
            quote_mode: PairQuoteMode::PassiveBid,
        }
    }
}

pub struct CompetitorRecycler {
    cfg: CompetitorRecyclerConfig,
    yes_emitted: usize,
    no_emitted: usize,
    last_yes_emit_ns: i64,
    last_no_emit_ns: i64,
    book_events_seen: u64,
    attractive_pair_events: u64,
}

impl CompetitorRecycler {
    pub fn new(cfg: CompetitorRecyclerConfig) -> Self {
        Self {
            cfg,
            yes_emitted: 0,
            no_emitted: 0,
            last_yes_emit_ns: i64::MIN / 2,
            last_no_emit_ns: i64::MIN / 2,
            book_events_seen: 0,
            attractive_pair_events: 0,
        }
    }
}

fn book_prices(event: &ReplayEvent) -> Option<(f64, f64, f64, f64)> {
    let yes_bid = event.yes_bid as f64;
    let yes_ask = event.yes_ask as f64;
    if !yes_bid.is_finite()
        || !yes_ask.is_finite()
        || yes_bid <= 0.0
        || yes_ask <= 0.0
        || yes_bid >= yes_ask
        || yes_ask >= 1.0
    {
        return None;
    }
    let mid = 0.5 * (yes_bid + yes_ask);
    let no_bid = (1.0 - yes_ask).clamp(0.0, 1.0);
    Some((yes_bid, yes_ask, no_bid, mid))
}

fn window_delta_bps(spot: &SpotHistory, market_close_ns: i64, now_ns: i64) -> f64 {
    let open_ns = market_close_ns - BETTING_WINDOW_SECS * NS_PER_SEC;
    let Some(open) = spot.price_at_or_before(open_ns) else {
        return 0.0;
    };
    let Some(now) = spot.price_at_or_before(now_ns) else {
        return 0.0;
    };
    if open <= 0.0 || !open.is_finite() {
        return 0.0;
    }
    (now / open - 1.0) * 10_000.0
}

fn allocate_pair_quotes(
    yes_bid: f64,
    yes_ask: f64,
    cap: f64,
    mode: PairQuoteMode,
    delta_bps: f64,
) -> Option<(f64, f64)> {
    let base_yes = yes_bid;
    let base_no = (1.0 - yes_ask).clamp(0.0, 1.0);
    let base_pair_cost = base_yes + base_no;
    let slack = cap - base_pair_cost;
    if slack < -1e-9 {
        return None;
    }

    // Local single-YES-book replay mirrors NO spread from the YES spread. This
    // keeps the strategy parametrized by pair cost; full two-token replay can
    // replace this allocator with native YES/NO books.
    let yes_capacity = (yes_ask - yes_bid - 1e-6).max(0.0);
    let no_capacity = yes_capacity;
    let (mut yes_extra, mut no_extra) = match mode {
        PairQuoteMode::PassiveBid => return Some((base_yes, base_no)),
        PairQuoteMode::SharedSlackEven => {
            let yes = yes_capacity.min(slack * 0.5);
            let no = no_capacity.min(slack - yes);
            (yes, no)
        }
        PairQuoteMode::SharedSlackLeader => {
            let yes_weight = if delta_bps > 0.0 {
                0.75
            } else if delta_bps < 0.0 {
                0.25
            } else {
                0.50
            };
            let yes = yes_capacity.min(slack * yes_weight);
            let no = no_capacity.min(slack - yes);
            (yes, no)
        }
    };

    let mut leftover = slack - yes_extra - no_extra;
    if leftover > 0.0 {
        let add_yes = (yes_capacity - yes_extra).max(0.0).min(leftover);
        yes_extra += add_yes;
        leftover -= add_yes;
        no_extra += (no_capacity - no_extra).max(0.0).min(leftover);
    }

    let yes_px = base_yes + yes_extra;
    let no_px = base_no + no_extra;
    if yes_px + no_px > cap + 1e-9 {
        return None;
    }
    Some((yes_px, no_px))
}

impl Strategy for CompetitorRecycler {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        let Some((yes_bid, yes_ask, _no_bid, mid)) = book_prices(event) else {
            return StrategyOutput::hold();
        };
        if mid < self.cfg.min_mid || mid > self.cfg.max_mid {
            return StrategyOutput::hold();
        }

        let window_open_ns = ctx.market_close_ns - BETTING_WINDOW_SECS * NS_PER_SEC;
        let secs_in = ((event.ts_ns - window_open_ns) as f64 / 1e9) as f32;
        if !(0.0..=BETTING_WINDOW_SECS as f32).contains(&secs_in) {
            return StrategyOutput::hold();
        }
        let secs_to_close = BETTING_WINDOW_SECS as f32 - secs_in;
        if secs_to_close <= self.cfg.stop_secs_before_close {
            return StrategyOutput::hold();
        }

        let spot_30s_abs_bps = spot
            .trailing_return(event.ts_ns, 30 * NS_PER_SEC)
            .map(|r| r.abs() * 10_000.0)
            .unwrap_or(0.0);
        if spot_30s_abs_bps > self.cfg.max_spot_30s_abs_bps {
            return StrategyOutput::hold();
        }
        let abs_spot_flow_30s = spot
            .signed_flow_and_adverse(event.ts_ns, 30 * NS_PER_SEC, true)
            .imbalance
            .abs();
        let mut stress_mult: f64 = 1.0;
        if abs_spot_flow_30s > self.cfg.max_abs_spot_flow_30s {
            return StrategyOutput::hold();
        }
        let whipsaw = WhipsawRiskSnapshot::from_history(event.ts_ns, spot);
        if whipsaw.realized_vol_180s_bps < self.cfg.min_regime_realized_vol_180s_bps {
            return StrategyOutput::hold();
        }
        if whipsaw.sign_flip_rate < self.cfg.min_regime_sign_flip_rate {
            return StrategyOutput::hold();
        }
        if whipsaw.path_efficiency > self.cfg.max_regime_path_efficiency {
            return StrategyOutput::hold();
        }

        let delta = ctx.yes_shares - ctx.no_shares;
        let delta_bps = window_delta_bps(spot, ctx.market_close_ns, event.ts_ns);
        let base_pair_cost = yes_bid + (1.0 - yes_ask).clamp(0.0, 1.0);
        self.book_events_seen = self.book_events_seen.saturating_add(1);
        if base_pair_cost <= self.cfg.max_pair_cost {
            self.attractive_pair_events = self.attractive_pair_events.saturating_add(1);
        }
        if self.book_events_seen >= self.cfg.stress_warmup_events {
            let attractive_frac =
                self.attractive_pair_events as f64 / self.book_events_seen.max(1) as f64;
            let bid_size = event.bids[0].size as f64;
            let ask_size = event.asks[0].size as f64;
            let bid_ask_ratio = bid_size / ask_size.max(1e-9);
            if attractive_frac > self.cfg.max_attractive_pair_frac_so_far
                && bid_ask_ratio < self.cfg.min_top_bid_ask_size_ratio
            {
                stress_mult = stress_mult.min(self.cfg.stress_clip_multiplier.clamp(0.0, 1.0));
            }
        }
        if stress_mult <= 1e-9 {
            return StrategyOutput::hold();
        }
        let Some((yes_quote, no_quote)) = allocate_pair_quotes(
            yes_bid,
            yes_ask,
            self.cfg.max_pair_cost,
            self.cfg.quote_mode,
            delta_bps,
        ) else {
            return StrategyOutput::hold();
        };
        let lean_yes = delta_bps >= self.cfg.min_abs_delta_bps_for_lean;
        let lean_no = delta_bps <= -self.cfg.min_abs_delta_bps_for_lean;
        let max_delta = self.cfg.repair_delta_shares
            + if lean_yes {
                self.cfg.lean_delta_shares
            } else {
                0.0
            };
        let min_delta = -self.cfg.repair_delta_shares
            - if lean_no {
                self.cfg.lean_delta_shares
            } else {
                0.0
            };

        let can_yes = delta < max_delta
            && ctx.yes_shares < self.cfg.max_leg_shares
            && self.yes_emitted < self.cfg.max_orders_per_leg
            && event.ts_ns - self.last_yes_emit_ns >= self.cfg.min_refresh_ns;
        let can_no = delta > min_delta
            && ctx.no_shares < self.cfg.max_leg_shares
            && self.no_emitted < self.cfg.max_orders_per_leg
            && event.ts_ns - self.last_no_emit_ns >= self.cfg.min_refresh_ns;

        let child_clip = self.cfg.child_clip_shares * stress_mult;
        let mut orders = Vec::with_capacity(2);
        if can_yes {
            orders.push(OrderRequest {
                side: Side::BuyYes,
                shares: self
                    .cfg
                    .child_clip_shares
                    .min(child_clip)
                    .min((max_delta - delta).max(0.0))
                    .min((self.cfg.max_leg_shares - ctx.yes_shares).max(0.0)),
                max_depth: 1,
                limit_price: Some(yes_quote as f32),
                tag: "comp_pair_yes",
            });
            self.yes_emitted += 1;
            self.last_yes_emit_ns = event.ts_ns;
        }
        if can_no {
            orders.push(OrderRequest {
                side: Side::BuyNo,
                shares: self
                    .cfg
                    .child_clip_shares
                    .min(child_clip)
                    .min((delta - min_delta).max(0.0))
                    .min((self.cfg.max_leg_shares - ctx.no_shares).max(0.0)),
                max_depth: 1,
                limit_price: Some(no_quote as f32),
                tag: "comp_pair_no",
            });
            self.no_emitted += 1;
            self.last_no_emit_ns = event.ts_ns;
        }

        orders.retain(|order| order.shares > 1e-9);
        StrategyOutput { orders }
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

    fn spot(close_ns: i64, open: f64, now: f64) -> SpotHistory {
        let now_ns = close_ns - 200 * NS_PER_SEC;
        SpotHistory::new(vec![
            SpotTick {
                ts_ns: close_ns - BETTING_WINDOW_SECS * NS_PER_SEC,
                price: open,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns - 30 * NS_PER_SEC,
                price: 100.05,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: now,
                quantity: 1.0,
                is_buyer_maker: false,
            },
        ])
    }

    #[test]
    fn quotes_bid_side_pair_when_cost_is_attractive() {
        let close_ns = 300 * NS_PER_SEC;
        let mut s = CompetitorRecycler::new(CompetitorRecyclerConfig::default());
        let out = s.on_event(
            &evt(close_ns - 200 * NS_PER_SEC, 0.46, 0.51),
            &ctx(0.0, close_ns),
            &spot(close_ns, 100.0, 100.02),
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 2);
        let yes = out.orders.iter().find(|o| o.side == Side::BuyYes).unwrap();
        let no = out.orders.iter().find(|o| o.side == Side::BuyNo).unwrap();
        assert_eq!(yes.limit_price, Some(0.46));
        assert_eq!(no.limit_price, Some(0.49));
    }

    #[test]
    fn skips_when_bid_side_pair_cost_is_too_high() {
        let close_ns = 300 * NS_PER_SEC;
        let mut s = CompetitorRecycler::new(CompetitorRecyclerConfig::default());
        let out = s.on_event(
            &evt(close_ns - 200 * NS_PER_SEC, 0.49, 0.51),
            &ctx(0.0, close_ns),
            &spot(close_ns, 100.0, 100.02),
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }

    #[test]
    fn allows_spot_confirmed_yes_lean_but_blocks_unconfirmed_heavy_side() {
        let close_ns = 300 * NS_PER_SEC;
        let mut s = CompetitorRecycler::new(CompetitorRecyclerConfig::default());
        let out = s.on_event(
            &evt(close_ns - 200 * NS_PER_SEC, 0.46, 0.51),
            &ctx(10.0, close_ns),
            &spot(close_ns, 100.0, 100.10),
            &TradeHistory::default(),
        );
        assert!(out.orders.iter().any(|o| o.side == Side::BuyYes));
        assert!(out.orders.iter().any(|o| o.side == Side::BuyNo));
    }

    #[test]
    fn pulls_when_spot_30s_move_is_toxic() {
        let close_ns = 300 * NS_PER_SEC;
        let now_ns = close_ns - 200 * NS_PER_SEC;
        let spot = SpotHistory::new(vec![
            SpotTick {
                ts_ns: now_ns - 30 * NS_PER_SEC,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 100.2,
                quantity: 1.0,
                is_buyer_maker: false,
            },
        ]);
        let mut s = CompetitorRecycler::new(CompetitorRecyclerConfig::default());
        let out = s.on_event(
            &evt(now_ns, 0.46, 0.51),
            &ctx(0.0, close_ns),
            &spot,
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }

    #[test]
    fn pulls_when_regime_vol_is_too_low() {
        let close_ns = 300 * NS_PER_SEC;
        let mut s = CompetitorRecycler::new(CompetitorRecyclerConfig {
            min_regime_realized_vol_180s_bps: 1.0,
            ..CompetitorRecyclerConfig::default()
        });
        let out = s.on_event(
            &evt(close_ns - 200 * NS_PER_SEC, 0.46, 0.51),
            &ctx(0.0, close_ns),
            &spot(close_ns, 100.0, 100.02),
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }

    #[test]
    fn pulls_when_attractive_pair_is_persistent_and_bid_depth_is_weak() {
        let close_ns = 300 * NS_PER_SEC;
        let mut s = CompetitorRecycler::new(CompetitorRecyclerConfig {
            stress_warmup_events: 1,
            max_attractive_pair_frac_so_far: 0.5,
            min_top_bid_ask_size_ratio: 2.0,
            ..CompetitorRecyclerConfig::default()
        });
        let out = s.on_event(
            &evt(close_ns - 200 * NS_PER_SEC, 0.46, 0.51),
            &ctx(0.0, close_ns),
            &spot(close_ns, 100.0, 100.02),
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }

    #[test]
    fn stress_can_scale_clip_instead_of_pulling() {
        let close_ns = 300 * NS_PER_SEC;
        let mut s = CompetitorRecycler::new(CompetitorRecyclerConfig {
            stress_warmup_events: 1,
            max_attractive_pair_frac_so_far: 0.5,
            min_top_bid_ask_size_ratio: 2.0,
            stress_clip_multiplier: 0.25,
            ..CompetitorRecyclerConfig::default()
        });
        let out = s.on_event(
            &evt(close_ns - 200 * NS_PER_SEC, 0.46, 0.51),
            &ctx(0.0, close_ns),
            &spot(close_ns, 100.0, 100.02),
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 2);
        assert!(out.orders.iter().all(|o| (o.shares - 2.5).abs() < 1e-9));
    }
}
