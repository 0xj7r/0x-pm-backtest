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
}

impl Default for AlphaModelConfig {
    fn default() -> Self {
        Self {
            vol_lookback_s: 1800,
            vol_sample_dt_s: 1,
            momentum_lookback_s: 0,
            momentum_weight: 1.0,
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

    let estimate = if cfg.momentum_lookback_s == 0 {
        estimate_fair_value(
            spot_now,
            state.market.strike,
            state.time_remaining_s(),
            sigma_bar_bps,
            bar_secs as f64,
        )
    } else {
        // Drift over one bar-equivalent horizon: trailing return over the
        // lookback, rescaled linearly to bar length, then weighted.
        let lookback_ns = cfg.momentum_lookback_s as i64 * 1_000_000_000;
        let raw = state.spot.trailing_return(state.now_ns, lookback_ns)?;
        let to_bar = bar_secs as f64 / cfg.momentum_lookback_s as f64;
        let momentum_return = raw * to_bar * cfg.momentum_weight;
        let est = estimate_fair_value_with_momentum(
            spot_now,
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
}
