//! ReactiveDirectional — the spec's primary directional strategy.
//!
//! Phases per market:
//!   1. **Pre-window** (>5min from resolution): hold.
//!   2. **Early window** (0–45s into the 5-min betting window): paired
//!      YES/NO probes around the mid.
//!   3. **Mid window** (45–180s): directional add-on on the dominant side.
//!   4. **Late window** (last 120s): aggressive loading on high-confidence,
//!      high-skew windows.
//!
//! Signal stack: book-internal direction_score is *augmented* with multi-TF
//! spot momentum and regime gating when spot data is available. Strategy
//! degrades gracefully (book-only) when `spot` is empty.

use crate::regime::{BtcRegime, BtcRegimeSnapshot};
use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_model::{ModelConfig, ModelOutput, ModelState, entry_gate_satisfied};
use pm_types::{ReplayEvent, SpotHistory};
use std::sync::{Arc, Mutex};

const BETTING_WINDOW_SECS: i64 = 300;
const EARLY_PHASE_SECS: f32 = 45.0;
const MID_PHASE_END_SECS: f32 = 180.0;
const MIN_ENTRY_CONFIDENCE: f32 = 0.68;
const MAX_ENTRY_RISK: f32 = 0.72;
const MIN_EDGE: f32 = 0.05;
const MIN_AGGRESSIVE_CONFIDENCE: f32 = 0.82;
const MIN_AGGRESSIVE_SKEW_MULTIPLIER: f32 = 3.0;
const EARLY_PAIR_MAX_NOTIONAL: f64 = 1.04;

#[derive(Debug, Clone)]
pub struct ReactiveDirectionalConfig {
    pub bankroll_usdc: f64,
    pub kelly_fraction: f64,
    pub max_clip_usdc: f64,
    pub early_pair_clip_usdc: f64,
    /// Conviction floor for BuyYes (Up) entries.
    pub conviction_threshold_yes: f32,
    /// Conviction floor for BuyNo (Down) entries.
    pub conviction_threshold_no: f32,
    /// Weight of book-internal direction in the composite (0..1).
    pub book_weight: f32,
    /// Weight of spot momentum in the composite (0..1). Auto-zeroed when no spot.
    pub spot_weight: f32,
    /// Optional shared model state for walk-forward calibration continuity.
    pub shared_model_state: Option<Arc<Mutex<pm_model::ModelState>>>,
    pub shared_skew_table: Option<Arc<Mutex<pm_model::SkewWinRateTable>>>,
}

impl Default for ReactiveDirectionalConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 100.0,
            kelly_fraction: 0.25,
            max_clip_usdc: 5.0,
            early_pair_clip_usdc: 0.5,
            conviction_threshold_yes: 0.68,
            conviction_threshold_no: 0.68,
            book_weight: 0.4,
            spot_weight: 0.6,
            shared_model_state: None,
            shared_skew_table: None,
        }
    }
}

impl ReactiveDirectionalConfig {
    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            book_weight: self.book_weight,
            spot_weight: self.spot_weight,
            ..ModelConfig::default()
        }
    }
}

pub struct ReactiveDirectional {
    cfg: ReactiveDirectionalConfig,
    model_state: ModelState,
    early_paired_emitted: bool,
    last_load_ts_ns: i64,
    last_prediction_is_yes: Option<bool>,
}

impl ReactiveDirectional {
    pub fn new(cfg: ReactiveDirectionalConfig) -> Self {
        Self {
            cfg,
            model_state: ModelState::new(),
            early_paired_emitted: false,
            last_load_ts_ns: 0,
            last_prediction_is_yes: None,
        }
    }

    fn with_model_state<R>(&mut self, f: impl FnOnce(&mut ModelState) -> R) -> R {
        if let Some(shared) = &self.cfg.shared_model_state {
            let mut shared = shared.lock().expect("shared model mutex poisoned");
            f(&mut shared)
        } else {
            f(&mut self.model_state)
        }
    }

    fn record_market_result(&mut self, market_mid: f32, predicted_yes: bool, resolved_yes: bool) {
        self.with_model_state(|state| {
            state.record_market_result(market_mid, predicted_yes, resolved_yes);
        });
        if let Some(shared) = &self.cfg.shared_skew_table {
            if let Ok(mut table) = shared.lock() {
                table.record(market_mid, predicted_yes, resolved_yes);
            }
        }
    }

    fn window_open_ns(&self, ctx: &Ctx) -> i64 {
        ctx.market_close_ns - BETTING_WINDOW_SECS * 1_000_000_000
    }

    fn seconds_since_window_open(&self, event: &ReplayEvent, ctx: &Ctx) -> f32 {
        ((event.ts_ns - self.window_open_ns(ctx)) as f64 / 1e9) as f32
    }

    fn in_betting_window(&self, event: &ReplayEvent, ctx: &Ctx) -> bool {
        let secs = self.seconds_since_window_open(event, ctx);
        (0.0..=BETTING_WINDOW_SECS as f32).contains(&secs)
    }

    fn side_probability_ratio(&self, output: &ModelOutput, mid: f32) -> f32 {
        let side_mid = if output.direction_score >= 0.0 {
            mid.clamp(0.0, 1.0)
        } else {
            (1.0 - mid).clamp(0.0, 1.0)
        };
        if side_mid <= 0.0 {
            return 1.0;
        }
        (output.calibrated_p / side_mid).clamp(0.0, 16.0)
    }

    fn early_pair_orders(&self, event: &ReplayEvent) -> Option<[OrderRequest; 2]> {
        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return None;
        }
        let yes_px = event.yes_ask as f64;
        let no_px = (1.0 - event.yes_bid).max(0.01) as f64;
        let yes_clip = self.cfg.early_pair_clip_usdc.max(0.0);
        let mut yes_shares = if yes_px > 0.0 { yes_clip / yes_px } else { 0.0 };
        let mut no_shares = if no_px > 0.0 { yes_clip / no_px } else { 0.0 };

        let mut paired_notional = yes_shares * yes_px + no_shares * no_px;
        if paired_notional > EARLY_PAIR_MAX_NOTIONAL && paired_notional > 0.0 {
            let scale = EARLY_PAIR_MAX_NOTIONAL / paired_notional;
            yes_shares *= scale;
            no_shares *= scale;
            paired_notional = EARLY_PAIR_MAX_NOTIONAL;
        }
        if paired_notional <= 0.0 || yes_shares <= 0.0 || no_shares <= 0.0 {
            return None;
        }
        Some([
            OrderRequest {
                side: Side::BuyYes,
                shares: yes_shares,
                max_depth: 1,
                limit_price: None,
                tag: "rd_early_pair_yes",
            },
            OrderRequest {
                side: Side::BuyNo,
                shares: no_shares,
                max_depth: 1,
                limit_price: None,
                tag: "rd_early_pair_no",
            },
        ])
    }

    fn dominant_stake(&self, output: &ModelOutput, event: &ReplayEvent, aggressive: bool) -> f64 {
        let side_mid = if output.direction_score >= 0.0 {
            event.yes_mid
        } else {
            1.0 - event.yes_mid
        };
        let mut stake = pm_risk::fractional_kelly_stake(
            output.calibrated_p as f64,
            side_mid as f64,
            self.cfg.bankroll_usdc,
            self.cfg.kelly_fraction,
            self.cfg.max_clip_usdc,
        );
        stake *= output.confidence_score as f64 * (1.0 - output.risk_score as f64) * 0.65;
        if aggressive {
            stake *= 1.35;
        }
        stake.max(0.0)
    }
}

impl Strategy for ReactiveDirectional {
    fn on_event_scored(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        trades: &pm_types::TradeHistory,
    ) -> (StrategyOutput, Option<ModelOutput>) {
        let secs = self.seconds_since_window_open(event, ctx);
        let model_cfg = self.cfg.model_config();
        let model_output: ModelOutput =
            self.with_model_state(|state| state.evaluate(event, spot, secs, &model_cfg));
        let mut conf = model_output.confidence_score;
        let mut risk = model_output.risk_score;

        // Regime gating: skip directional loading in Whipsaw; bonus to confidence
        // in DirectionalSmooth.
        let regime = BtcRegimeSnapshot::from_history(event.ts_ns, spot).regime();
        let regime_conf_mul = match regime {
            Some(BtcRegime::DirectionalSmooth) => 1.15,
            Some(BtcRegime::TrendingVolatile) => 0.95,
            Some(BtcRegime::Flat) => 0.85,
            Some(BtcRegime::Whipsaw) => 0.55,
            None => 1.0,
        };
        conf = (conf * regime_conf_mul).clamp(0.0, 1.0);
        // keep this assignment mutable for later weighting in size calc.
        risk = (risk).clamp(0.0, 1.0);

        let adjusted = ModelOutput {
            confidence_score: conf,
            risk_score: risk,
            ..model_output
        };
        self.last_prediction_is_yes = Some(adjusted.direction_score >= 0.0);
        let skew_ratio = self.side_probability_ratio(&adjusted, event.yes_mid);
        let _ = trades;
        if !self.in_betting_window(event, ctx) || event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return (StrategyOutput::hold(), Some(adjusted));
        }

        // Early window: paired probes (once).
        if !self.early_paired_emitted && secs <= EARLY_PHASE_SECS {
            self.early_paired_emitted = true;
            let Some([yes_order, no_order]) = self.early_pair_orders(event) else {
                return (StrategyOutput::hold(), Some(adjusted));
            };
            return (
                StrategyOutput {
                    orders: vec![yes_order, no_order],
                },
                Some(adjusted),
            );
        }

        // Mid/late directional load.
        if secs <= EARLY_PHASE_SECS {
            return (StrategyOutput::hold(), Some(adjusted));
        }
        if matches!(regime, Some(BtcRegime::Whipsaw)) {
            return (StrategyOutput::hold(), Some(adjusted));
        }
        let mut adjusted = adjusted;
        adjusted.confidence_score = conf;

        if !entry_gate_satisfied(
            &adjusted,
            event.yes_mid,
            MIN_EDGE,
            MIN_ENTRY_CONFIDENCE.max(if adjusted.direction_score >= 0.0 {
                self.cfg.conviction_threshold_yes
            } else {
                self.cfg.conviction_threshold_no
            }),
            MAX_ENTRY_RISK,
        ) {
            return (StrategyOutput::hold(), Some(adjusted));
        }
        if event.ts_ns - self.last_load_ts_ns < 5 * 1_000_000_000 {
            return (StrategyOutput::hold(), Some(adjusted));
        }
        self.last_load_ts_ns = event.ts_ns;

        let is_late = secs >= MID_PHASE_END_SECS;
        let is_aggressive_late = is_late
            && adjusted.confidence_score >= MIN_AGGRESSIVE_CONFIDENCE
            && skew_ratio >= MIN_AGGRESSIVE_SKEW_MULTIPLIER;
        let stake = self.dominant_stake(&adjusted, event, is_aggressive_late);
        if stake <= 0.10 {
            return (StrategyOutput::hold(), Some(adjusted));
        }

        let (side, fill_px) = if adjusted.direction_score >= 0.0 {
            (Side::BuyYes, event.yes_ask)
        } else {
            (Side::BuyNo, (1.0 - event.yes_bid).max(0.01))
        };
        let shares = (stake / fill_px as f64).max(0.0);
        if shares <= 0.0 {
            return (StrategyOutput::hold(), Some(adjusted));
        }
        let tag = if is_aggressive_late {
            "rd_dominant_load_aggressive"
        } else {
            "rd_dominant_load"
        };
        (
            StrategyOutput::one(OrderRequest {
                side,
                shares,
                max_depth: 1,
                limit_price: None,
                tag,
            }),
            Some(adjusted),
        )
    }

    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        _trades: &pm_types::TradeHistory,
    ) -> StrategyOutput {
        let (out, model_output) = self.on_event_scored(event, ctx, spot, _trades);
        if let Some(output) = model_output {
            self.last_prediction_is_yes = Some(output.direction_score >= 0.0);
        }
        out
    }

    fn on_market_resolved(&mut self, market_mid: f32, resolved_yes: bool) {
        let Some(predicted_yes) = self.last_prediction_is_yes else {
            return;
        };
        self.record_market_result(market_mid, predicted_yes, resolved_yes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{BookLevel, MarketId, ReplayFlags, tape::TAPE_DEPTH};

    fn evt(ts_ns: i64, bid: f32, ask: f32) -> ReplayEvent {
        let mut bids = [BookLevel::default(); TAPE_DEPTH];
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        for i in 0..TAPE_DEPTH {
            bids[i] = BookLevel {
                price: bid - i as f32 * 0.01,
                size: 200.0,
            };
            asks[i] = BookLevel {
                price: ask + i as f32 * 0.01,
                size: 200.0,
            };
        }
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
    fn holds_outside_betting_window() {
        let close_ns: i64 = 1_000_000_000_000;
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
        let mut s = ReactiveDirectional::new(ReactiveDirectionalConfig::default());
        let spot = SpotHistory::default();
        let out = s.on_event(
            &evt(close_ns - 600 * 1_000_000_000, 0.50, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert!(out.orders.is_empty(), "should not trade pre-window");
    }

    #[test]
    fn emits_model_output_outside_window_for_attribution() {
        let close_ns: i64 = 1_000_000_000_000;
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
        let mut s = ReactiveDirectional::new(ReactiveDirectionalConfig::default());
        let spot = SpotHistory::default();
        let (_out, model) = s.on_event_scored(
            &evt(close_ns - 600 * 1_000_000_000, 0.50, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert!(
            model.is_some(),
            "model output should be emitted for attribution"
        );
        let model = model.unwrap();
        assert!(model.confidence_score >= 0.0);
        assert!(model.risk_score >= 0.0);
    }

    #[test]
    fn emits_paired_entry_in_early_window_once() {
        let close_ns: i64 = 1_000_000_000_000;
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
        let mut s = ReactiveDirectional::new(ReactiveDirectionalConfig::default());
        let spot = SpotHistory::default();
        let first = s.on_event(
            &evt(close_ns - 290 * 1_000_000_000, 0.50, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert_eq!(first.orders.len(), 2);
        let second = s.on_event(
            &evt(close_ns - 289 * 1_000_000_000, 0.50, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert!(
            second.orders.is_empty()
                || second
                    .orders
                    .iter()
                    .all(|o| !o.tag.starts_with("rd_early_pair")),
        );
    }

    #[test]
    fn does_not_emit_paired_orders_after_early_phase() {
        let close_ns: i64 = 1_000_000_000_000;
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
        let mut s = ReactiveDirectional::new(ReactiveDirectionalConfig::default());
        let spot = SpotHistory::default();
        let out = s.on_event(
            &evt(close_ns - 200 * 1_000_000_000, 0.50, 0.51),
            &ctx,
            &spot,
            &pm_types::TradeHistory::default(),
        );
        assert!(out.orders.is_empty(), "should not pair after 45s");
    }

    #[test]
    fn early_pair_orders_respect_combined_cost_cap() {
        let cfg = ReactiveDirectionalConfig {
            early_pair_clip_usdc: 2.0,
            ..ReactiveDirectionalConfig::default()
        };
        let s = ReactiveDirectional::new(cfg);
        let event = evt(100_000_000_000, 0.50, 0.50);
        let orders = s.early_pair_orders(&event).expect("expected pair orders");
        let total_notional: f64 = orders
            .iter()
            .map(|o| match o.side {
                Side::BuyYes => o.shares * event.yes_ask as f64,
                Side::BuyNo => o.shares * (1.0 - event.yes_bid) as f64,
                _ => 0.0,
            })
            .sum();
        assert!(total_notional <= EARLY_PAIR_MAX_NOTIONAL + 1e-6);
    }

    #[test]
    fn late_window_skew_ratio_is_calculated_from_side_implied() {
        let s = ReactiveDirectional::new(ReactiveDirectionalConfig::default());
        let output = ModelOutput {
            direction_score: 1.0,
            confidence_score: 0.8,
            calibrated_p: 0.94,
            risk_score: 0.1,
        };
        let ratio = s.side_probability_ratio(&output, 0.30);
        assert!((ratio - 3.133333).abs() < 1e-3);
    }

    #[test]
    fn records_prediction_in_shared_skew_table_on_resolution() {
        let close_ns: i64 = 1_000_000_000_000;
        let mut cfg = ReactiveDirectionalConfig::default();
        let shared = Arc::new(Mutex::new(pm_model::SkewWinRateTable::new()));
        cfg.shared_skew_table = Some(shared.clone());
        let mut s = ReactiveDirectional::new(cfg);
        let spot = SpotHistory::default();
        let ctx = Ctx {
            events_seen: 10,
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
        let event = evt(close_ns - 100 * 1_000_000_000, 0.50, 0.51);
        let (_out, _model) =
            s.on_event_scored(&event, &ctx, &spot, &pm_types::TradeHistory::default());
        s.on_market_resolved(event.yes_mid, true);

        let lock = shared.lock().unwrap();
        let predicted_yes = s.last_prediction_is_yes.unwrap_or(false);
        let yes_rate = lock.expected_side_win_rate(event.yes_mid, true);
        let no_rate = lock.expected_side_win_rate(event.yes_mid, false);
        if predicted_yes {
            assert!(yes_rate > 0.9);
            assert!(no_rate < 0.1);
        } else {
            assert!(no_rate > 0.9);
            assert!(yes_rate < 0.1);
        }
    }
}
