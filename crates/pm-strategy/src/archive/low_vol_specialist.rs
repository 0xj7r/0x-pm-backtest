//! One-shot low-vol directional specialist.
//!
//! This sleeve consumes the JSON decision surface emitted by
//! `scripts/low_vol_decision_model.py --out-json`. It deliberately emits at
//! most one taker order per market, so we can validate the low-vol signal
//! without inheriting BonereaperV2's repeated late-favourite ladder behaviour.

use std::collections::HashSet;

use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_model::ModelAttribution;
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LowVolDecisionSurface {
    pub features: Vec<String>,
    pub standardization: LowVolStandardization,
    pub weights: Vec<f64>,
    #[serde(default)]
    pub filters: LowVolSurfaceFilters,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct LowVolStandardization {
    pub mean: Vec<f64>,
    pub std: Vec<f64>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct LowVolSurfaceFilters {
    pub min_price: f64,
    pub max_price: f64,
    pub min_seconds_to_close: f64,
    pub max_seconds_to_close: f64,
    pub max_volatility_regime: f64,
    pub min_legacy_side_p: f64,
}

impl Default for LowVolSurfaceFilters {
    fn default() -> Self {
        Self {
            min_price: 0.0,
            max_price: 1.0,
            min_seconds_to_close: 0.0,
            max_seconds_to_close: 300.0,
            max_volatility_regime: f64::INFINITY,
            min_legacy_side_p: 0.0,
        }
    }
}

impl LowVolDecisionSurface {
    pub fn is_valid(&self) -> bool {
        !self.features.is_empty()
            && self.weights.len() == self.features.len() + 1
            && self.standardization.mean.len() == self.features.len()
            && self.standardization.std.len() == self.features.len()
            && self
                .standardization
                .std
                .iter()
                .all(|v| v.is_finite() && *v > 0.0)
    }

    fn probability(&self, values: &[f64]) -> Option<f64> {
        if !self.is_valid() || values.len() != self.features.len() {
            return None;
        }
        let mut logit = *self.weights.first()?;
        for (idx, value) in values.iter().enumerate() {
            let mean = self.standardization.mean[idx];
            let std = self.standardization.std[idx];
            let z = (value - mean) / std;
            logit += self.weights[idx + 1] * z;
        }
        Some(1.0 / (1.0 + (-logit.clamp(-35.0, 35.0)).exp()))
    }
}

#[derive(Debug, Clone)]
pub struct LowVolSpecialistConfig {
    pub surface: Option<LowVolDecisionSurface>,
    pub clip_usdc: f64,
    pub min_specialist_edge: f64,
    pub min_legacy_edge: f64,
    pub max_depth: usize,
}

impl Default for LowVolSpecialistConfig {
    fn default() -> Self {
        Self {
            surface: None,
            clip_usdc: 5.0,
            min_specialist_edge: 0.0,
            min_legacy_edge: f64::NEG_INFINITY,
            max_depth: 1,
        }
    }
}

pub struct LowVolSpecialist {
    cfg: LowVolSpecialistConfig,
    fired: bool,
    unsupported_features: HashSet<String>,
}

impl LowVolSpecialist {
    pub fn new(cfg: LowVolSpecialistConfig) -> Self {
        Self {
            cfg,
            fired: false,
            unsupported_features: HashSet::new(),
        }
    }
}

fn side_price(event: &ReplayEvent, side: Side) -> f64 {
    match side {
        Side::BuyYes => event.yes_ask as f64,
        Side::BuyNo => (1.0 - event.yes_bid).clamp(0.0, 1.0) as f64,
        Side::SellYes | Side::SellNo => 0.0,
    }
}

fn buy_side_is_yes(side: Side) -> bool {
    matches!(side, Side::BuyYes)
}

fn shares_capped(usdc: f64, fill_px: f64) -> f64 {
    if fill_px <= 0.0 {
        return 0.0;
    }
    let raw = (usdc * 0.98) / fill_px;
    ((raw * 1000.0).floor() / 1000.0).max(0.0)
}

fn derived_feature(
    name: &str,
    event: &ReplayEvent,
    ctx: &Ctx,
    attr: ModelAttribution,
    side_is_yes: bool,
    side_price: f64,
    seconds_to_close: f64,
) -> Option<f64> {
    let model = ctx.model_output?;
    let legacy_side_p = model.calibrated_p as f64;
    let legacy_edge = legacy_side_p - side_price;
    let observed_range = attr.observed_yes_range_so_far as f64;
    let flip = attr.sequence.dir_flip_rate_8 as f64;
    let whipsaw = attr.risk.whipsaw as f64;
    let risk = model.risk_score as f64;
    let fast = attr.direction.spot_fast_momentum as f64;
    let broad = attr.direction.spot_broad_momentum as f64;
    match name {
        "side_price" => Some(side_price),
        "legacy_side_p" => Some(legacy_side_p),
        "legacy_edge" => Some(legacy_edge),
        "confidence_score" => Some(model.confidence_score as f64),
        "risk_score" => Some(risk),
        "seconds_to_close" => Some(seconds_to_close),
        "yes_mid" | "market_mid" => Some(event.yes_mid as f64),
        "feature_observed_yes_range_so_far" => Some(observed_range),
        "feature_momentum" => Some(attr.direction.momentum as f64),
        "feature_book_imbalance_top3" => Some(attr.book_imbalance_top3 as f64),
        "feature_microprice_dev" => Some(attr.direction.microprice_dev as f64),
        "feature_microprice_spot_alignment" => {
            Some(attr.direction.microprice_spot_alignment as f64)
        }
        "feature_top3_delta_5s" => Some(attr.direction.top3_delta_5s as f64),
        "feature_top3_delta_15s" => Some(attr.direction.top3_delta_15s as f64),
        "feature_spot_score" => Some(attr.spot_score as f64),
        "feature_spot_fast_momentum" => Some(fast),
        "feature_spot_broad_momentum" => Some(broad),
        "feature_spot_momentum_600s" => Some(attr.direction.spot_momentum_600s as f64),
        "feature_spot_momentum_1800s" => Some(attr.direction.spot_momentum_1800s as f64),
        "feature_spot_1h_4h_alignment" => Some(attr.direction.spot_1h_4h_alignment as f64),
        "feature_spot_fast_long_alignment" => Some(attr.direction.spot_fast_long_alignment as f64),
        "feature_direction_raw" => Some(attr.direction_raw as f64),
        "feature_stability" => Some(attr.confidence.stability as f64),
        "feature_sign_persistence" => Some(attr.confidence.sign_persistence as f64),
        "feature_markov_persistence" => Some(attr.confidence.markov_persistence as f64),
        "feature_whipsaw" => Some(whipsaw),
        "feature_path_risk" => Some(attr.risk.path_risk as f64),
        "feature_imbalance_turn" => Some(attr.risk.imbalance_turn as f64),
        "feature_markov_reversal_risk" => Some(attr.risk.markov_reversal_risk as f64),
        "feature_volatility_penalty" => Some(attr.risk.volatility_penalty as f64),
        "feature_volatility_regime" => Some(attr.volatility_regime as f64),
        "feature_dir_flip_rate_8" => Some(flip),
        "feature_dir_std_8" => Some(attr.sequence.dir_std_8 as f64),
        "feature_dir_abs_mean_8" => Some(attr.sequence.dir_abs_mean_8 as f64),
        "buy_yes" => Some(if side_is_yes { 1.0 } else { 0.0 }),
        "price_x_legacy_p" => Some(side_price * legacy_side_p),
        "edge_x_conf" => Some(legacy_edge * model.confidence_score as f64),
        "range_x_flip" => Some(observed_range * flip),
        "range_x_whipsaw" => Some(observed_range * whipsaw),
        "risk_x_range" => Some(risk * observed_range),
        "fast_x_broad_spot" => Some(fast * broad),
        "price_x_seconds" => Some(side_price * seconds_to_close / 300.0),
        _ => None,
    }
}

impl Strategy for LowVolSpecialist {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        _spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        if self.fired || event.yes_bid <= 0.0 || event.yes_ask <= 0.0 {
            return StrategyOutput::hold();
        }
        let Some(surface) = self.cfg.surface.as_ref() else {
            return StrategyOutput::hold();
        };
        let Some(model) = ctx.model_output else {
            return StrategyOutput::hold();
        };
        let Some(attr) = ctx.model_attribution else {
            return StrategyOutput::hold();
        };

        let seconds_to_close = (ctx.market_close_ns - event.ts_ns) as f64 / 1e9;
        if seconds_to_close < surface.filters.min_seconds_to_close
            || seconds_to_close > surface.filters.max_seconds_to_close
            || (attr.volatility_regime as f64) > surface.filters.max_volatility_regime
            || (model.calibrated_p as f64) < surface.filters.min_legacy_side_p
        {
            return StrategyOutput::hold();
        }

        let side = if model.direction_score >= 0.0 {
            Side::BuyYes
        } else {
            Side::BuyNo
        };
        let px = side_price(event, side);
        if px < surface.filters.min_price || px > surface.filters.max_price {
            return StrategyOutput::hold();
        }

        let legacy_edge = model.calibrated_p as f64 - px;
        if legacy_edge < self.cfg.min_legacy_edge {
            return StrategyOutput::hold();
        }

        let side_is_yes = buy_side_is_yes(side);
        let mut values = Vec::with_capacity(surface.features.len());
        for name in &surface.features {
            let Some(value) =
                derived_feature(name, event, ctx, attr, side_is_yes, px, seconds_to_close)
            else {
                self.unsupported_features.insert(name.clone());
                return StrategyOutput::hold();
            };
            values.push(value);
        }
        let Some(predicted_p) = surface.probability(&values) else {
            return StrategyOutput::hold();
        };
        if predicted_p - px < self.cfg.min_specialist_edge {
            return StrategyOutput::hold();
        }

        let shares = shares_capped(self.cfg.clip_usdc, px);
        if shares <= 0.0 {
            return StrategyOutput::hold();
        }
        self.fired = true;
        StrategyOutput::one(OrderRequest {
            side,
            shares,
            max_depth: self.cfg.max_depth.max(1),
            limit_price: Some(px as f32),
            tag: "low_vol_specialist",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_model::{
        ConfidenceScore, DirectionScore, ModelAttribution, ModelOutput, RiskScore, SequenceFeatures,
    };
    use pm_types::{BookLevel, MarketId, ReplayFlags, tape::TAPE_DEPTH};

    fn surface() -> LowVolDecisionSurface {
        LowVolDecisionSurface {
            features: vec!["side_price".to_string(), "legacy_edge".to_string()],
            standardization: LowVolStandardization {
                mean: vec![0.5, 0.0],
                std: vec![1.0, 1.0],
            },
            weights: vec![3.0, -1.0, 20.0],
            filters: LowVolSurfaceFilters {
                min_price: 0.0,
                max_price: 0.99,
                min_seconds_to_close: 5.0,
                max_seconds_to_close: 180.0,
                max_volatility_regime: 10.0,
                min_legacy_side_p: 0.55,
            },
        }
    }

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
            flags: ReplayFlags::default(),
        }
    }

    fn ctx(direction_score: f32, calibrated_p: f32) -> Ctx {
        Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 1000.0,
            market_yes_range_so_far: 0.2,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: Some(ModelOutput {
                direction_score,
                confidence_score: 0.7,
                calibrated_p,
                risk_score: 0.4,
            }),
            model_attribution: Some(ModelAttribution {
                confidence: ConfidenceScore {
                    stability: 0.5,
                    sign_persistence: 0.5,
                    markov_persistence: 0.5,
                    early_market_penalty: 0.0,
                    time_of_day_advantage: 0.0,
                    composite: 0.5,
                },
                direction: DirectionScore {
                    momentum: 0.5,
                    spot_fast_momentum: 0.1,
                    spot_broad_momentum: 0.1,
                    composite: 0.5,
                    ..DirectionScore::default()
                },
                risk: RiskScore {
                    whipsaw: 0.1,
                    path_risk: 0.1,
                    composite: 0.1,
                    ..RiskScore::default()
                },
                sequence: SequenceFeatures {
                    dir_mean_3: 0.1,
                    dir_mean_8: 0.1,
                    dir_slope_8: 0.1,
                    dir_flip_rate_8: 0.1,
                    dir_std_8: 0.1,
                    dir_abs_mean_8: 0.1,
                },
                volatility_regime: 1.0,
                ..ModelAttribution::default()
            }),
            market_close_ns: 300_000_000_000,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
        }
    }

    #[test]
    fn emits_once_when_surface_edge_passes() {
        let mut strategy = LowVolSpecialist::new(LowVolSpecialistConfig {
            surface: Some(surface()),
            clip_usdc: 10.0,
            min_specialist_edge: 0.0,
            min_legacy_edge: 0.0,
            max_depth: 1,
        });
        let out = strategy.on_event(
            &event(240_000_000_000, 0.68, 0.70),
            &ctx(0.8, 0.78),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyYes);
        assert_eq!(out.orders[0].tag, "low_vol_specialist");

        let second = strategy.on_event(
            &event(241_000_000_000, 0.68, 0.70),
            &ctx(0.8, 0.78),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(second.orders.is_empty());
    }

    #[test]
    fn refuses_negative_legacy_edge() {
        let mut strategy = LowVolSpecialist::new(LowVolSpecialistConfig {
            surface: Some(surface()),
            min_legacy_edge: 0.0,
            ..LowVolSpecialistConfig::default()
        });
        let out = strategy.on_event(
            &event(240_000_000_000, 0.68, 0.80),
            &ctx(0.8, 0.60),
            &SpotHistory::default(),
            &TradeHistory::default(),
        );
        assert!(out.orders.is_empty());
    }
}
