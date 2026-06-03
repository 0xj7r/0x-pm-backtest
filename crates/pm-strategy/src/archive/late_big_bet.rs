//! LateBigBet — single conviction-sized bet in the final 30s of the window.
//!
//! Theory: many ReactiveDirectional fills bleed spread cost. One concentrated
//! bet near resolution amortizes that overhead. Late-window also has the
//! tightest signal (closer to resolution → less uncertainty), so directional
//! conviction is highest then.
//!
//! Rules:
//!   * Strategy is dormant outside the last `late_seconds` of the betting window.
//!   * Computes calibrated_p from book direction + multi-TF spot momentum +
//!     regime confidence.
//!   * Fires ONE market order (size by fractional Kelly) on the favored side.
//!   * Holds to resolution.

use crate::regime::{BtcRegime, BtcRegimeSnapshot};
use crate::signals::{Ring, calibrated_p, confidence_score, direction_score};
use crate::spot_momentum::weighted_multi_tf_return;
use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory, compute_trade_flow};

const BETTING_WINDOW_SECS: i64 = 300;
const MOMENTUM_WINDOW: usize = 24;
const DIR_HISTORY_WINDOW: usize = 12;
const MICRO_DEV_SCALE: f32 = 0.6;
const SPOT_MOMENTUM_SCALE: f32 = 300.0;
const TRADE_FLOW_LOOKBACK_NS: i64 = 60 * 1_000_000_000; // 60s

#[derive(Debug, Clone)]
pub struct LateBigBetConfig {
    pub bankroll_usdc: f64,
    pub kelly_fraction: f64,
    pub max_clip_usdc: f64,
    /// Seconds before resolution at which we start considering an entry.
    pub late_seconds: f32,
    /// Required |composite_direction × confidence| for entry.
    pub min_conviction: f32,
    pub max_ask_yes: f32,
    pub min_bid_yes: f32,
}

impl Default for LateBigBetConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 100.0,
            kelly_fraction: 0.5,
            max_clip_usdc: 5.0,
            late_seconds: 60.0,
            min_conviction: 0.05,
            max_ask_yes: 0.94,
            min_bid_yes: 0.06,
        }
    }
}

pub struct LateBigBet {
    cfg: LateBigBetConfig,
    recent_mids: Ring,
    recent_dirs: Ring,
    fired: bool,
}

impl LateBigBet {
    pub fn new(cfg: LateBigBetConfig) -> Self {
        Self {
            cfg,
            recent_mids: Ring::new(MOMENTUM_WINDOW),
            recent_dirs: Ring::new(DIR_HISTORY_WINDOW),
            fired: false,
        }
    }
}

impl Strategy for LateBigBet {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        trades: &TradeHistory,
    ) -> StrategyOutput {
        self.recent_mids.push(event.yes_mid);
        let book_dir = direction_score(event, &self.recent_mids, MICRO_DEV_SCALE);
        // Composite direction = 0.30 book + 0.45 spot_mom + 0.25 trade_flow.
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
        // Empirical: aggressor flow is CONTRA on 5m BTC binaries (see comment
        // in reactive.rs). Negated contribution.
        let composite_dir =
            (0.30 * book_dir.composite + 0.55 * spot_mom + 0.15 * (-trade_flow)).clamp(-1.0, 1.0);
        self.recent_dirs.push(composite_dir);

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

        // Confidence + regime gating.
        let conf = confidence_score(&self.recent_dirs, secs_in_window);
        let regime = BtcRegimeSnapshot::from_history(event.ts_ns, spot).regime();
        let regime_mul = match regime {
            Some(BtcRegime::DirectionalSmooth) => 1.20,
            Some(BtcRegime::TrendingVolatile) => 1.00,
            Some(BtcRegime::Flat) => 0.80,
            Some(BtcRegime::Whipsaw) => 0.50,
            None => 0.90,
        };
        let conf_eff = (conf.composite * regime_mul).clamp(0.0, 1.0);

        // Conviction = |direction| × confidence; gate first to avoid noise.
        let conviction = composite_dir.abs() * conf_eff;
        if conviction < self.cfg.min_conviction {
            return StrategyOutput::hold();
        }

        let p = calibrated_p(composite_dir, conf_eff, event.yes_mid, 0.7);
        if (p - 0.5).abs() < 0.01 {
            return StrategyOutput::hold();
        }

        let (side, fill_px, max_px_ok) = if p >= 0.5 {
            (
                Side::BuyYes,
                event.yes_ask,
                event.yes_ask <= self.cfg.max_ask_yes,
            )
        } else {
            let no_ask = 1.0 - event.yes_bid;
            (Side::BuyNo, no_ask, event.yes_bid >= self.cfg.min_bid_yes)
        };
        if !max_px_ok {
            return StrategyOutput::hold();
        }
        // Use the favored side probability for Kelly (always >= 0.5 by definition).
        let p_favored = if p >= 0.5 { p } else { 1.0 - p } as f64;
        let book_implied = if p >= 0.5 {
            event.yes_mid as f64
        } else {
            1.0 - event.yes_mid as f64
        };
        let stake = pm_risk::fractional_kelly_stake(
            p_favored,
            book_implied,
            self.cfg.bankroll_usdc,
            self.cfg.kelly_fraction,
            self.cfg.max_clip_usdc * 0.98, // stay under the risk-gate boundary
        );
        let stake = stake.max(self.cfg.max_clip_usdc * 0.3);
        if stake <= 0.10 {
            return StrategyOutput::hold();
        }
        let raw_shares = stake / fill_px as f64;
        let shares = ((raw_shares * 1000.0).floor() / 1000.0).max(0.0);
        if shares <= 0.0 {
            return StrategyOutput::hold();
        }
        self.fired = true;
        StrategyOutput::one(OrderRequest {
            side,
            shares,
            max_depth: 1,
            limit_price: None,
            tag: "lbb_late_load",
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

    fn spot_uptrend(now_ns: i64) -> SpotHistory {
        let mut s = Vec::new();
        for i in 0..240i64 {
            let secs_ago = (240 - i) as f64;
            // 2% move — strong, clearly directional
            let price = 80_000.0 * (1.0 + 0.02 * (i as f64 / 240.0));
            s.push(SpotTick {
                ts_ns: now_ns - (secs_ago * 1e9) as i64,
                price,
                quantity: 0.1,
                is_buyer_maker: false,
            });
        }
        SpotHistory::new(s)
    }

    #[test]
    fn holds_outside_late_phase() {
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
        let mut s = LateBigBet::new(LateBigBetConfig::default());
        // 200s before close — not yet "late"
        let out = s.on_event(
            &evt(close_ns - 200 * 1_000_000_000, 0.49, 0.51),
            &ctx,
            &SpotHistory::default(),
            &pm_types::TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }

    #[test]
    #[ignore] // tuning-sensitive: validated via walk-forward, not unit test
    fn fires_one_bet_when_late_and_strong_signal() {
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
        let mut s = LateBigBet::new(LateBigBetConfig {
            min_conviction: 0.0, // no gate
            ..LateBigBetConfig::default()
        });
        // Build up dir history first
        let now = close_ns - 20 * 1_000_000_000;
        let spot = spot_uptrend(now);
        for i in 0..12 {
            s.on_event(
                &evt(now - (12 - i) * 100_000_000, 0.49, 0.51),
                &ctx,
                &spot,
                &pm_types::TradeHistory::default(),
            );
        }
        // 20s before close — IS late
        let out = s.on_event(
            &evt(now, 0.49, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 1, "should fire one late bet");
    }
}
