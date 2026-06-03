//! BonereaperLite — composite multi-lane strategy distilled from the 8.6k-LOC
//! `polymarket-exec/src/strategies/bonereaper_mm.rs` reference.
//!
//! Lanes (each is gated independently; multiple may fire on the same tick):
//!
//! 1. **PairedCore** (0–60s of betting window): paired YES/NO probes at the
//!    spread (taker fills, small clip). Same as ReactiveDirectional's early
//!    phase but per-leg sized to ~$0.30.
//!
//! 2. **LateFavoriteClimb** (last 90s of window): when calibrated_p > 0.65
//!    AND confidence high, add up to a clip on the favored side. Repeatable
//!    every 15 seconds.
//!
//! 3. **ConvexTail** (any time during window): when yes_mid > 0.92 OR < 0.08
//!    (highly skewed), put a tiny resting limit on the OPPOSITE side. If the
//!    market reverts, captures the pop. Cheap optionality.
//!
//! 4. **ReversalHedge** (last 60s): if composite direction sign flipped from
//!    our paired-core entry, take a small offsetting position.
//!
//! Sizing: each lane has its own per-clip budget. Aggregate per-market gross
//! exposure is bounded by `PortfolioLimits.max_per_market_exposure_usdc` at
//! the runner level.

use crate::regime::{BtcRegime, BtcRegimeSnapshot};
use crate::signals::{Ring, calibrated_p, confidence_score, direction_score};
use crate::spot_momentum::weighted_multi_tf_return;
use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory, compute_trade_flow};

const BETTING_WINDOW_SECS: i64 = 300;
const MOMENTUM_WINDOW: usize = 32;
const DIR_HISTORY_WINDOW: usize = 16;
const MICRO_DEV_SCALE: f32 = 0.6;
const SPOT_MOMENTUM_SCALE: f32 = 250.0;
const TRADE_FLOW_LOOKBACK_NS: i64 = 60 * 1_000_000_000;
const EARLY_PHASE_SECS: f32 = 60.0;
const LATE_FAV_PHASE_SECS: f32 = 90.0; // last 90s
const REVERSAL_PHASE_SECS: f32 = 60.0;

#[derive(Debug, Clone)]
pub struct BonereaperLiteConfig {
    pub bankroll_usdc: f64,
    pub max_clip_usdc: f64,

    // PairedCore
    pub paired_clip_usdc: f64,

    // LateFavoriteClimb
    pub late_fav_min_p: f32,
    pub late_fav_min_conf: f32,
    pub late_fav_clip_usdc: f64,
    pub late_fav_refresh_secs: f32,

    // ConvexTail
    pub convex_extreme_mid: f32, // e.g. 0.92
    pub convex_clip_usdc: f64,

    // ReversalHedge
    pub reversal_clip_usdc: f64,
    pub reversal_min_flip_score: f32,
}

impl Default for BonereaperLiteConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 100.0,
            max_clip_usdc: 5.0,
            paired_clip_usdc: 0.50,
            late_fav_min_p: 0.62,
            late_fav_min_conf: 0.30,
            late_fav_clip_usdc: 2.50,
            late_fav_refresh_secs: 15.0,
            convex_extreme_mid: 0.90,
            convex_clip_usdc: 0.30,
            reversal_clip_usdc: 0.40,
            reversal_min_flip_score: 0.30,
        }
    }
}

pub struct BonereaperLite {
    cfg: BonereaperLiteConfig,
    recent_mids: Ring,
    recent_dirs: Ring,
    paired_fired: bool,
    /// Sign of the composite direction at paired-core entry. Used by
    /// ReversalHedge to detect flip.
    paired_entry_dir_sign: f32,
    last_late_fav_ns: i64,
    convex_fired: bool,
    reversal_fired: bool,
}

impl BonereaperLite {
    pub fn new(cfg: BonereaperLiteConfig) -> Self {
        Self {
            cfg,
            recent_mids: Ring::new(MOMENTUM_WINDOW),
            recent_dirs: Ring::new(DIR_HISTORY_WINDOW),
            paired_fired: false,
            paired_entry_dir_sign: 0.0,
            last_late_fav_ns: 0,
            convex_fired: false,
            reversal_fired: false,
        }
    }
}

fn shares_capped(clip_usdc: f64, fill_px: f32) -> f64 {
    let raw = (clip_usdc * 0.98) / fill_px as f64;
    ((raw * 1000.0).floor() / 1000.0).max(0.0)
}

impl Strategy for BonereaperLite {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        trades: &TradeHistory,
    ) -> StrategyOutput {
        self.recent_mids.push(event.yes_mid);
        let book_dir = direction_score(event, &self.recent_mids, MICRO_DEV_SCALE);
        let spot_mom = if !spot.is_empty() {
            weighted_multi_tf_return(event.ts_ns, spot)
                .map(|r| (r as f32 * SPOT_MOMENTUM_SCALE).clamp(-1.0, 1.0))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        let trade_flow = if !trades.is_empty() {
            compute_trade_flow(event.ts_ns, TRADE_FLOW_LOOKBACK_NS, trades).flow_imbalance
        } else {
            0.0
        };
        // Empirical: aggressor flow CONTRA on 5m BTC binaries (see reactive.rs).
        let composite_dir =
            (0.30 * book_dir.composite + 0.55 * spot_mom + 0.15 * (-trade_flow)).clamp(-1.0, 1.0);
        self.recent_dirs.push(composite_dir);

        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }
        let window_open_ns = ctx.market_close_ns - BETTING_WINDOW_SECS * 1_000_000_000;
        let secs_in_window = ((event.ts_ns - window_open_ns) as f64 / 1e9) as f32;
        if !(0.0..=BETTING_WINDOW_SECS as f32).contains(&secs_in_window) {
            return StrategyOutput::hold();
        }
        let secs_to_close = BETTING_WINDOW_SECS as f32 - secs_in_window;

        let conf = confidence_score(&self.recent_dirs, secs_in_window);
        let regime = BtcRegimeSnapshot::from_history(event.ts_ns, spot).regime();
        let regime_mul = match regime {
            Some(BtcRegime::DirectionalSmooth) => 1.15,
            Some(BtcRegime::TrendingVolatile) => 1.00,
            Some(BtcRegime::Flat) => 0.90,
            Some(BtcRegime::Whipsaw) => 0.70,
            None => 0.95,
        };
        let conf_eff = (conf.composite * regime_mul).clamp(0.0, 1.0);
        let p = calibrated_p(composite_dir, conf_eff, event.yes_mid, 0.7);

        let mut orders: Vec<OrderRequest> = Vec::new();

        // Lane 1: PairedCore (once, early window)
        if !self.paired_fired && secs_in_window <= EARLY_PHASE_SECS {
            self.paired_fired = true;
            self.paired_entry_dir_sign = composite_dir.signum();
            orders.push(OrderRequest {
                side: Side::BuyYes,
                shares: shares_capped(self.cfg.paired_clip_usdc, event.yes_ask),
                max_depth: 1,
                limit_price: None,
                tag: "br_paired_yes",
            });
            let no_px = (1.0 - event.yes_bid).max(0.01);
            orders.push(OrderRequest {
                side: Side::BuyNo,
                shares: shares_capped(self.cfg.paired_clip_usdc, no_px),
                max_depth: 1,
                limit_price: None,
                tag: "br_paired_no",
            });
        }

        // Lane 2: LateFavoriteClimb (last 90s, repeated every refresh_secs)
        if secs_to_close <= LATE_FAV_PHASE_SECS
            && p > self.cfg.late_fav_min_p
            && conf_eff > self.cfg.late_fav_min_conf
            && !matches!(regime, Some(BtcRegime::Whipsaw))
            && (event.ts_ns - self.last_late_fav_ns) as f32 / 1e9 > self.cfg.late_fav_refresh_secs
        {
            self.last_late_fav_ns = event.ts_ns;
            let (side, fill_px, max_ok) = if p >= 0.5 {
                (Side::BuyYes, event.yes_ask, event.yes_ask <= 0.94)
            } else {
                let no_ask = (1.0 - event.yes_bid).max(0.01);
                (Side::BuyNo, no_ask, event.yes_bid >= 0.06)
            };
            if max_ok {
                orders.push(OrderRequest {
                    side,
                    shares: shares_capped(self.cfg.late_fav_clip_usdc, fill_px),
                    max_depth: 1,
                    limit_price: None,
                    tag: "br_late_fav",
                });
            }
        }

        // Lane 3: ConvexTail (any time, once per market)
        if !self.convex_fired {
            if event.yes_mid >= self.cfg.convex_extreme_mid {
                // YES very high → NO is cheap; small contrarian BuyNo as optionality.
                let no_px = (1.0 - event.yes_bid).max(0.01);
                if no_px >= 0.02 && no_px <= 0.20 {
                    self.convex_fired = true;
                    orders.push(OrderRequest {
                        side: Side::BuyNo,
                        shares: shares_capped(self.cfg.convex_clip_usdc, no_px),
                        max_depth: 1,
                        limit_price: None,
                        tag: "br_convex_no",
                    });
                }
            } else if event.yes_mid <= (1.0 - self.cfg.convex_extreme_mid) {
                // YES very low → cheap; small contrarian BuyYes.
                if event.yes_ask <= 0.20 && event.yes_ask >= 0.02 {
                    self.convex_fired = true;
                    orders.push(OrderRequest {
                        side: Side::BuyYes,
                        shares: shares_capped(self.cfg.convex_clip_usdc, event.yes_ask),
                        max_depth: 1,
                        limit_price: None,
                        tag: "br_convex_yes",
                    });
                }
            }
        }

        // Lane 4: ReversalHedge (last 60s, once)
        if !self.reversal_fired
            && self.paired_fired
            && secs_to_close <= REVERSAL_PHASE_SECS
            && composite_dir.signum() != self.paired_entry_dir_sign
            && composite_dir.abs() >= self.cfg.reversal_min_flip_score
            && conf_eff > 0.25
        {
            self.reversal_fired = true;
            let (side, fill_px, max_ok) = if composite_dir > 0.0 {
                (Side::BuyYes, event.yes_ask, event.yes_ask <= 0.94)
            } else {
                let no_ask = (1.0 - event.yes_bid).max(0.01);
                (Side::BuyNo, no_ask, event.yes_bid >= 0.06)
            };
            if max_ok {
                orders.push(OrderRequest {
                    side,
                    shares: shares_capped(self.cfg.reversal_clip_usdc, fill_px),
                    max_depth: 1,
                    limit_price: None,
                    tag: "br_reversal_hedge",
                });
            }
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
    fn paired_core_fires_in_early_window() {
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
            ..Ctx::default()
        };
        let mut s = BonereaperLite::new(BonereaperLiteConfig::default());
        // 10s into the window
        let out = s.on_event(
            &evt(close_ns - 290 * 1_000_000_000, 0.49, 0.51),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(out.orders.iter().any(|o| o.tag == "br_paired_yes"));
        assert!(out.orders.iter().any(|o| o.tag == "br_paired_no"));
    }

    #[test]
    fn convex_tail_fires_on_extreme_yes_mid() {
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
            ..Ctx::default()
        };
        let mut s = BonereaperLite::new(BonereaperLiteConfig::default());
        // mid = 0.92, very high YES; should fire convex_no
        let out = s.on_event(
            &evt(close_ns - 290 * 1_000_000_000, 0.91, 0.93),
            &ctx,
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(
            out.orders.iter().any(|o| o.tag == "br_convex_no"),
            "got {:?}",
            out.orders.iter().map(|o| o.tag).collect::<Vec<_>>()
        );
    }
}
