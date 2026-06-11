//! The edge model: exogenous belief from `ExoState` only.
//!
//! `belief` is the single entry point that turns exogenous data into
//! "our probability the market resolves Up." Its signature is the leakage
//! guarantee: it accepts only an [`ExoState`], which contains no Polymarket
//! book data. `edge = p_exo - price` happens strictly downstream (harness,
//! strategy), never here.

use crate::fair_value::{
    FairValueEstimate, FairValueModel, estimate_fair_value, estimate_fair_value_with_momentum,
};
use crate::state::ExoState;
use crate::vol::realized_vol_bps_over_bar;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct AlphaModelConfig {
    /// Trailing window for realized vol (seconds).
    pub vol_lookback_s: u32,
    /// Sampling cadence for the vol estimator (seconds).
    pub vol_sample_dt_s: u32,
    /// Momentum/drift lookback in seconds; 0 disables the drift term (base model).
    pub momentum_lookback_s: u32,
    /// Scale applied to the momentum return before it enters the drift term.
    pub momentum_weight: f64,
    /// Cross-asset lead: weight on the reference asset's trailing 60s return
    /// as an extra drift term (0 disables; needs `ExoState.ref_spot`).
    #[serde(default)]
    pub xasset_weight: f64,
    /// Perp-led level: weight on the basis-adjusted perp last in the
    /// effective-spot blend (0 disables; needs `ExoState.perp` trades).
    #[serde(default)]
    pub perp_price_weight: f64,
}

impl Default for AlphaModelConfig {
    fn default() -> Self {
        Self {
            vol_lookback_s: 1800,
            vol_sample_dt_s: 1,
            momentum_lookback_s: 0,
            momentum_weight: 1.0,
            xasset_weight: 0.0,
            perp_price_weight: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct Belief {
    /// Probability the market resolves Up (YES), exogenous-only.
    pub p_up: f64,
    /// Realized vol input used, bps over one bar.
    pub sigma_bar_bps: f64,
    /// Momentum return input used (0.0 when disabled).
    pub momentum_return: f64,
    pub estimate: FairValueEstimate,
}

/// Trailing-return window for the cross-asset drift term.
const XASSET_RETURN_LOOKBACK_S: i64 = 60;
/// Sampling cadence for the rolling perp-spot basis median.
const BASIS_SAMPLE_DT_S: i64 = 10;

/// Effective spot level for the belief: `(1-w)*spot + w*(perp - basis)`,
/// where `basis` is the rolling median perp-minus-spot difference over the
/// vol lookback (level consistency). Falls back to spot when the perp tape
/// or the basis estimate is unavailable.
fn effective_spot(state: &ExoState, cfg: &AlphaModelConfig, spot_now: f64) -> f64 {
    let w = cfg.perp_price_weight;
    let Some(perp) = state.perp else {
        return spot_now;
    };
    let Some(perp_now) = perp.trades.price_at_or_before(state.now_ns) else {
        return spot_now;
    };
    let Some(basis) = perp.median_basis_abs(
        state.spot,
        state.now_ns,
        cfg.vol_lookback_s as i64 * 1_000_000_000,
        BASIS_SAMPLE_DT_S * 1_000_000_000,
    ) else {
        return spot_now;
    };
    let blended = (1.0 - w) * spot_now + w * (perp_now - basis);
    if blended.is_finite() && blended > 0.0 { blended } else { spot_now }
}

/// Cross-asset drift: the reference asset's trailing 60s return, rescaled to
/// one bar horizon and weighted. 0.0 when the reference tape is missing.
fn xasset_drift(state: &ExoState, cfg: &AlphaModelConfig, bar_secs: u32) -> f64 {
    let Some(ref_spot) = state.ref_spot else {
        return 0.0;
    };
    let raw = ref_spot
        .trailing_return(state.now_ns, XASSET_RETURN_LOOKBACK_S * 1_000_000_000)
        .unwrap_or(0.0);
    if !raw.is_finite() {
        return 0.0;
    }
    raw * (bar_secs as f64 / XASSET_RETURN_LOOKBACK_S as f64) * cfg.xasset_weight
}

/// Exogenous belief at one decision instant. Returns `None` when spot or vol
/// is unavailable, or when the fair-value model reports `NoSignal` — callers
/// must stand down rather than trade a default.
pub fn belief(state: &ExoState, cfg: &AlphaModelConfig) -> Option<Belief> {
    let spot_now = state.spot_now()?;
    let bar_secs = state.market.window_secs;
    let sigma_bar_bps = realized_vol_bps_over_bar(
        state.spot,
        state.now_ns,
        cfg.vol_lookback_s,
        cfg.vol_sample_dt_s,
        bar_secs,
    )?;

    let eff_spot = if cfg.perp_price_weight != 0.0 {
        effective_spot(state, cfg, spot_now)
    } else {
        spot_now
    };

    let estimate = if cfg.momentum_lookback_s == 0 && cfg.xasset_weight == 0.0 {
        estimate_fair_value(
            eff_spot,
            state.market.strike,
            state.time_remaining_s(),
            sigma_bar_bps,
            bar_secs as f64,
        )
    } else {
        // Drift over one bar-equivalent horizon: trailing return over the
        // lookback, rescaled linearly to bar length, then weighted. The
        // cross-asset term adds the reference asset's (e.g. BTC) trailing
        // return on top, same rescaling.
        let own_momentum = if cfg.momentum_lookback_s == 0 {
            0.0
        } else {
            let lookback_ns = cfg.momentum_lookback_s as i64 * 1_000_000_000;
            let raw = state.spot.trailing_return(state.now_ns, lookback_ns)?;
            let to_bar = bar_secs as f64 / cfg.momentum_lookback_s as f64;
            raw * to_bar * cfg.momentum_weight
        };
        let momentum_return = own_momentum
            + if cfg.xasset_weight != 0.0 {
                xasset_drift(state, cfg, bar_secs)
            } else {
                0.0
            };
        let est = estimate_fair_value_with_momentum(
            eff_spot,
            state.market.strike,
            state.tau_fraction(),
            sigma_bar_bps / 10_000.0,
            momentum_return,
            bar_secs as f64,
        );
        return finish(est, sigma_bar_bps, momentum_return);
    };

    finish(estimate, sigma_bar_bps, 0.0)
}

fn finish(estimate: FairValueEstimate, sigma_bar_bps: f64, momentum_return: f64) -> Option<Belief> {
    match estimate.model {
        FairValueModel::NoSignal(_) => None,
        _ => Some(Belief {
            p_up: estimate.p_up,
            sigma_bar_bps,
            momentum_return,
            estimate,
        }),
    }
}

/// The deployable model: base fair value plus an optional trained calibrator.
/// Still a pure function of `ExoState` — the calibrator's features are built
/// from the same exogenous state, so the leakage guarantee is unchanged.
#[derive(Debug, Clone, Default)]
pub struct AlphaModel {
    pub cfg: AlphaModelConfig,
    pub calibrator: Option<crate::calibrator::ExoCalibrator>,
    /// Trained continuation head; consulted only for Aligned entries (the
    /// fade's belief stays the pure exogenous fair value).
    pub dir_model: Option<crate::directional::DirModel>,
}

#[derive(Debug, Clone, Copy)]
pub struct Evaluation {
    /// The deployed belief: calibrated when a calibrator is loaded, else raw.
    pub p: f64,
    pub raw: Belief,
    /// Present when a calibrator is loaded or features were requested.
    pub features: Option<crate::calibrator::ExoFeatures>,
}

impl AlphaModel {
    pub fn evaluate(&self, state: &ExoState, want_features: bool) -> Option<Evaluation> {
        let raw = belief(state, &self.cfg)?;
        let need_features = want_features || self.calibrator.is_some();
        let features =
            need_features.then(|| crate::calibrator::exo_features(state, &raw, self.cfg.vol_lookback_s));
        let p = match (&self.calibrator, &features) {
            (Some(cal), Some(f)) => cal.predict(raw.p_up as f32, f) as f64,
            _ => raw.p_up,
        };
        Some(Evaluation { p, raw, features })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{MarketMeta, Token};
    use pm_types::{SpotHistory, SpotTick};

    fn tick(ts_s: i64, price: f64) -> SpotTick {
        SpotTick {
            ts_ns: ts_s * 1_000_000_000,
            price,
            quantity: 1.0,
            is_buyer_maker: false,
        }
    }

    fn wavy_history(n_secs: i64) -> SpotHistory {
        let mut ticks = Vec::new();
        let mut price = 100_000.0;
        for s in 0..n_secs {
            ticks.push(tick(s, price));
            price *= if s % 2 == 0 { 1.0001 } else { 0.9999 };
        }
        SpotHistory::new(ticks)
    }

    fn state(spot: &SpotHistory, now_s: i64, strike: f64) -> ExoState<'_> {
        ExoState {
            spot,
            perp: None,
            ref_spot: None,
            market: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: (now_s - 100) * 1_000_000_000,
                close_ts_ns: (now_s + 200) * 1_000_000_000,
                strike,
            },
            now_ns: now_s * 1_000_000_000,
        }
    }

    #[test]
    fn empty_spot_yields_none() {
        let spot = SpotHistory::default();
        let s = state(&spot, 2000, 100_000.0);
        assert!(belief(&s, &AlphaModelConfig::default()).is_none());
    }

    #[test]
    fn base_belief_is_deterministic_and_sane() {
        let spot = wavy_history(2000);
        let s = state(&spot, 1900, 100_000.0);
        let cfg = AlphaModelConfig::default();
        let b1 = belief(&s, &cfg).expect("belief");
        let b2 = belief(&s, &cfg).expect("belief");
        assert_eq!(b1.p_up, b2.p_up);
        assert!(b1.p_up > 0.0 && b1.p_up < 1.0);
        assert!(b1.sigma_bar_bps > 0.0);
    }

    #[test]
    fn momentum_disabled_equals_base_within_linearization() {
        let spot = wavy_history(2000);
        let s = state(&spot, 1900, 100_000.0);
        let base = belief(&s, &AlphaModelConfig::default()).unwrap();
        let mom_zero_weight = belief(
            &s,
            &AlphaModelConfig {
                momentum_lookback_s: 300,
                momentum_weight: 0.0,
                ..AlphaModelConfig::default()
            },
        )
        .unwrap();
        assert!(
            (base.p_up - mom_zero_weight.p_up).abs() < 5e-3,
            "base={} mom={}",
            base.p_up,
            mom_zero_weight.p_up
        );
    }

    #[test]
    fn strike_above_spot_lowers_p_up() {
        let spot = wavy_history(2000);
        let below = belief(&state(&spot, 1900, 99_000.0), &AlphaModelConfig::default()).unwrap();
        let above = belief(&state(&spot, 1900, 101_000.0), &AlphaModelConfig::default()).unwrap();
        assert!(below.p_up > above.p_up);
    }

    fn trending_history(n_secs: i64, per_sec: f64) -> SpotHistory {
        let mut ticks = Vec::new();
        let mut price = 100_000.0;
        for s in 0..n_secs {
            ticks.push(tick(s, price));
            price *= 1.0 + per_sec;
        }
        SpotHistory::new(ticks)
    }

    #[test]
    fn xasset_zero_weight_is_byte_identical_to_base() {
        let spot = wavy_history(2000);
        let reference = trending_history(2000, 2e-5);
        let mut s = state(&spot, 1900, 100_000.0);
        s.ref_spot = Some(&reference);
        let base = belief(&state(&spot, 1900, 100_000.0), &AlphaModelConfig::default()).unwrap();
        let zero = belief(&s, &AlphaModelConfig::default()).unwrap();
        assert_eq!(base.p_up, zero.p_up);
        assert_eq!(base.momentum_return, zero.momentum_return);
    }

    #[test]
    fn positive_ref_return_raises_p_up() {
        let spot = wavy_history(2000);
        let ref_up = trending_history(2000, 2e-5);
        let ref_down = trending_history(2000, -2e-5);
        let cfg = AlphaModelConfig {
            xasset_weight: 1.0,
            ..AlphaModelConfig::default()
        };
        let mut s_up = state(&spot, 1900, 100_000.0);
        s_up.ref_spot = Some(&ref_up);
        let mut s_down = state(&spot, 1900, 100_000.0);
        s_down.ref_spot = Some(&ref_down);
        let up = belief(&s_up, &cfg).unwrap();
        let down = belief(&s_down, &cfg).unwrap();
        assert!(up.p_up > down.p_up, "up={} down={}", up.p_up, down.p_up);
        // Drift math: 60s trailing return * (300/60) * weight.
        let raw60 = ref_up
            .trailing_return(1900 * 1_000_000_000, 60 * 1_000_000_000)
            .unwrap();
        assert!(
            (up.momentum_return - raw60 * 5.0).abs() < 1e-12,
            "got {} want {}",
            up.momentum_return,
            raw60 * 5.0
        );
    }

    #[test]
    fn xasset_weight_without_ref_spot_degrades_to_zero_drift() {
        let spot = wavy_history(2000);
        let cfg = AlphaModelConfig {
            xasset_weight: 0.5,
            ..AlphaModelConfig::default()
        };
        let b = belief(&state(&spot, 1900, 100_000.0), &cfg).unwrap();
        assert_eq!(b.momentum_return, 0.0);
    }

    fn perp_state_offset(spot: &SpotHistory, offset: f64) -> crate::state::PerpState {
        let ticks: Vec<SpotTick> = spot
            .range(i64::MIN, i64::MAX)
            .iter()
            .map(|t| SpotTick {
                ts_ns: t.ts_ns,
                price: t.price + offset,
                quantity: t.quantity,
                is_buyer_maker: t.is_buyer_maker,
            })
            .collect();
        crate::state::PerpState {
            trades: SpotHistory::new(ticks),
            ..Default::default()
        }
    }

    #[test]
    fn perp_blend_with_constant_basis_matches_spot_only() {
        // Perp = spot + constant offset everywhere: after the median basis
        // adjustment the blend collapses to the spot level, so the belief is
        // unchanged (up to median sampling noise, which is zero here).
        let spot = wavy_history(4000);
        let perp = perp_state_offset(&spot, 40.0);
        let base = belief(&state(&spot, 3900, 100_000.0), &AlphaModelConfig::default()).unwrap();
        let cfg = AlphaModelConfig {
            perp_price_weight: 1.0,
            ..AlphaModelConfig::default()
        };
        let mut s = state(&spot, 3900, 100_000.0);
        s.perp = Some(&perp);
        let blended = belief(&s, &cfg).unwrap();
        assert!(
            (base.p_up - blended.p_up).abs() < 1e-6,
            "base={} blended={}",
            base.p_up,
            blended.p_up
        );
    }

    #[test]
    fn perp_lead_moves_belief_toward_perp() {
        // Perp tape carries a fresh up-move the spot has not printed yet:
        // blending should raise p_up versus spot-only.
        let spot = wavy_history(4000);
        let mut perp = perp_state_offset(&spot, 0.0);
        let mut ticks: Vec<SpotTick> = perp
            .trades
            .range(0, 3895 * 1_000_000_000)
            .to_vec();
        ticks.push(SpotTick {
            ts_ns: 3899 * 1_000_000_000,
            price: 100_400.0,
            quantity: 1.0,
            is_buyer_maker: false,
        });
        perp.trades = SpotHistory::new(ticks);
        let base = belief(&state(&spot, 3900, 100_000.0), &AlphaModelConfig::default()).unwrap();
        let cfg = AlphaModelConfig {
            perp_price_weight: 0.5,
            ..AlphaModelConfig::default()
        };
        let mut s = state(&spot, 3900, 100_000.0);
        s.perp = Some(&perp);
        let blended = belief(&s, &cfg).unwrap();
        assert!(
            blended.p_up > base.p_up,
            "base={} blended={}",
            base.p_up,
            blended.p_up
        );
    }

    #[test]
    fn perp_weight_without_perp_state_falls_back_to_spot() {
        let spot = wavy_history(2000);
        let base = belief(&state(&spot, 1900, 100_000.0), &AlphaModelConfig::default()).unwrap();
        let cfg = AlphaModelConfig {
            perp_price_weight: 0.5,
            ..AlphaModelConfig::default()
        };
        let b = belief(&state(&spot, 1900, 100_000.0), &cfg).unwrap();
        assert_eq!(base.p_up, b.p_up);
    }
}
