//! Black-Scholes binary digital fair value for up/down markets.
//!
//! Ported verbatim from `polymarket-agent/polymarket-exec/src/signals/fair_value.rs`
//! (the SSOT for this math now lives here; the agent re-points once pm-alpha
//! proves out). One generalization: the bar duration is a parameter
//! (`bar_total_duration_s`) instead of a 300s constant so 15m markets work.
//!
//!   P(BTC_T > K | S_t) = Phi(ln(S_t / K) / (sigma_bar * sqrt(T_remaining / T_total)))
//!
//! `sigma_bar` is realized vol over one bar horizon. BSM assumes log-normal
//! returns; BTC at 5m horizon has fat tails and GARCH effects — the calibrator
//! layer corrects this empirically against realized outcomes (never price).

/// When time-remaining is below this floor, treat the resolution as
/// effectively decided (sigma_remaining -> 0 produces a numerically unstable
/// CDF). Falls back to step function: P(Up) = 1 if spot > strike else 0.
const TIME_FLOOR_SECONDS: f64 = 1.0;

/// Output of the fair-value model. `p_up` in [0, 1] is the model's
/// probability that the Up leg wins. Carries inputs for post-hoc analysis.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FairValueEstimate {
    pub p_up: f64,
    pub p_down: f64,
    pub log_moneyness: f64,
    pub sigma_remaining: f64,
    pub time_remaining_s: f64,
    pub model: FairValueModel,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FairValueModel {
    /// Black-Scholes binary digital — closed-form.
    BsmBinary,
    /// Spot known to be on one side of strike with insufficient remaining
    /// time for reversal — degenerate "decided" case.
    StepFunctionDecided,
    /// One or more inputs missing or invalid. Output `p_up = 0.5` is a safe
    /// default but callers must treat it as "no signal", never a belief.
    NoSignal(NoSignalReason),
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum NoSignalReason {
    SpotInvalid,
    StrikeInvalid,
    TimeRemainingInvalid,
    VolInvalid,
}

impl std::fmt::Display for NoSignalReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoSignalReason::SpotInvalid => f.write_str("spot_invalid"),
            NoSignalReason::StrikeInvalid => f.write_str("strike_invalid"),
            NoSignalReason::TimeRemainingInvalid => f.write_str("time_remaining_invalid"),
            NoSignalReason::VolInvalid => f.write_str("vol_invalid"),
        }
    }
}

/// Probability that the Up leg wins, from spot, strike, time remaining and
/// realized vol.
///
/// - `realized_vol_bps_over_bar`: std-dev of log returns over ONE bar
///   horizon, in basis points (NOT annualized).
/// - `bar_total_duration_s`: 300.0 for 5m markets, 900.0 for 15m.
pub fn estimate_fair_value(
    spot_price: f64,
    strike: f64,
    time_remaining_s: f64,
    realized_vol_bps_over_bar: f64,
    bar_total_duration_s: f64,
) -> FairValueEstimate {
    let no_signal = |reason: NoSignalReason| FairValueEstimate {
        p_up: 0.5,
        p_down: 0.5,
        log_moneyness: f64::NAN,
        sigma_remaining: f64::NAN,
        time_remaining_s,
        model: FairValueModel::NoSignal(reason),
    };

    if !spot_price.is_finite() || spot_price <= 0.0 {
        return no_signal(NoSignalReason::SpotInvalid);
    }
    if !strike.is_finite() || strike <= 0.0 {
        return no_signal(NoSignalReason::StrikeInvalid);
    }
    if !time_remaining_s.is_finite() || time_remaining_s < 0.0 {
        return no_signal(NoSignalReason::TimeRemainingInvalid);
    }
    if !realized_vol_bps_over_bar.is_finite() || realized_vol_bps_over_bar <= 0.0 {
        return no_signal(NoSignalReason::VolInvalid);
    }
    if !bar_total_duration_s.is_finite() || bar_total_duration_s <= 0.0 {
        return no_signal(NoSignalReason::TimeRemainingInvalid);
    }

    let log_moneyness = (spot_price / strike).ln();

    if time_remaining_s < TIME_FLOOR_SECONDS {
        let p_up = if log_moneyness > 0.0 { 1.0 } else { 0.0 };
        return FairValueEstimate {
            p_up,
            p_down: 1.0 - p_up,
            log_moneyness,
            sigma_remaining: 0.0,
            time_remaining_s,
            model: FairValueModel::StepFunctionDecided,
        };
    }

    let sigma_bar = realized_vol_bps_over_bar / 10_000.0;
    let time_fraction = (time_remaining_s / bar_total_duration_s).clamp(0.0, 1.0);
    let sigma_remaining = sigma_bar * time_fraction.sqrt();

    if sigma_remaining < 1e-12 {
        let p_up = if log_moneyness > 0.0 { 1.0 } else { 0.0 };
        return FairValueEstimate {
            p_up,
            p_down: 1.0 - p_up,
            log_moneyness,
            sigma_remaining,
            time_remaining_s,
            model: FairValueModel::StepFunctionDecided,
        };
    }

    let d = log_moneyness / sigma_remaining;
    let p_up = standard_normal_cdf(d).clamp(0.0, 1.0);

    FairValueEstimate {
        p_up,
        p_down: (1.0 - p_up).clamp(0.0, 1.0),
        log_moneyness,
        sigma_remaining,
        time_remaining_s,
        model: FairValueModel::BsmBinary,
    }
}

/// Momentum-aware binary probability:
///
/// `P(up) = Phi((delta + momentum * tau) / (sigma * sqrt(tau)))`
///
/// Inputs are dimensionless returns, not bps:
/// - `delta = (S-K)/K` from `spot_price` and `strike`.
/// - `tau_fraction`: remaining time / total bar duration.
/// - `sigma_return`: realized std-dev over one bar horizon (return units).
/// - `momentum_return`: recent drift over one bar-equivalent horizon.
pub fn estimate_fair_value_with_momentum(
    spot_price: f64,
    strike: f64,
    tau_fraction: f64,
    sigma_return: f64,
    momentum_return: f64,
    bar_total_duration_s: f64,
) -> FairValueEstimate {
    let no_signal = |reason: NoSignalReason| FairValueEstimate {
        p_up: 0.5,
        p_down: 0.5,
        log_moneyness: f64::NAN,
        sigma_remaining: f64::NAN,
        time_remaining_s: tau_fraction * bar_total_duration_s,
        model: FairValueModel::NoSignal(reason),
    };

    if !spot_price.is_finite() || spot_price <= 0.0 {
        return no_signal(NoSignalReason::SpotInvalid);
    }
    if !strike.is_finite() || strike <= 0.0 {
        return no_signal(NoSignalReason::StrikeInvalid);
    }
    if !tau_fraction.is_finite() || tau_fraction < 0.0 {
        return no_signal(NoSignalReason::TimeRemainingInvalid);
    }
    if !sigma_return.is_finite() || sigma_return <= 0.0 {
        return no_signal(NoSignalReason::VolInvalid);
    }
    if !bar_total_duration_s.is_finite() || bar_total_duration_s <= 0.0 {
        return no_signal(NoSignalReason::TimeRemainingInvalid);
    }

    let tau = tau_fraction.clamp(0.0, 1.0);
    let delta = (spot_price - strike) / strike;
    let log_moneyness = (spot_price / strike).ln();
    if tau <= TIME_FLOOR_SECONDS / bar_total_duration_s {
        let p_up = if delta >= 0.0 { 1.0 } else { 0.0 };
        return FairValueEstimate {
            p_up,
            p_down: 1.0 - p_up,
            log_moneyness,
            sigma_remaining: 0.0,
            time_remaining_s: tau * bar_total_duration_s,
            model: FairValueModel::StepFunctionDecided,
        };
    }

    let sigma_remaining = sigma_return * tau.sqrt();
    if sigma_remaining <= 1e-12 {
        let p_up = if delta >= 0.0 { 1.0 } else { 0.0 };
        return FairValueEstimate {
            p_up,
            p_down: 1.0 - p_up,
            log_moneyness,
            sigma_remaining,
            time_remaining_s: tau * bar_total_duration_s,
            model: FairValueModel::StepFunctionDecided,
        };
    }

    let z = (delta + momentum_return * tau) / sigma_remaining;
    let p_up = standard_normal_cdf(z).clamp(0.0, 1.0);
    FairValueEstimate {
        p_up,
        p_down: (1.0 - p_up).clamp(0.0, 1.0),
        log_moneyness,
        sigma_remaining,
        time_remaining_s: tau * bar_total_duration_s,
        model: FairValueModel::BsmBinary,
    }
}

/// Standard normal CDF via erf: Phi(x) = 0.5 * (1 + erf(x / sqrt(2))).
pub(crate) fn standard_normal_cdf(x: f64) -> f64 {
    if !x.is_finite() {
        if x.is_nan() {
            return f64::NAN;
        }
        return if x > 0.0 { 1.0 } else { 0.0 };
    }
    0.5 * (1.0 + erf_approx(x * std::f64::consts::FRAC_1_SQRT_2))
}

/// Abramowitz & Stegun 7.1.26 approximation of erf. Max abs error ~1.5e-7.
fn erf_approx(x: f64) -> f64 {
    const A1: f64 = 0.254_829_592;
    const A2: f64 = -0.284_496_736;
    const A3: f64 = 1.421_413_741;
    const A4: f64 = -1.453_152_027;
    const A5: f64 = 1.061_405_429;
    const P: f64 = 0.327_591_1;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let abs_x = x.abs();
    let t = 1.0 / (1.0 + P * abs_x);
    let y = 1.0 - (((((A5 * t + A4) * t) + A3) * t + A2) * t + A1) * t * (-abs_x * abs_x).exp();
    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR_5M: f64 = 300.0;

    fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn no_signal_with_specific_reason() {
        let r = estimate_fair_value(f64::NAN, 100.0, 60.0, 5.0, BAR_5M);
        assert_eq!(r.model, FairValueModel::NoSignal(NoSignalReason::SpotInvalid));
        assert_eq!(r.p_up, 0.5);

        let r = estimate_fair_value(100.0, 0.0, 60.0, 5.0, BAR_5M);
        assert_eq!(r.model, FairValueModel::NoSignal(NoSignalReason::StrikeInvalid));

        let r = estimate_fair_value(100.0, 100.0, -1.0, 5.0, BAR_5M);
        assert_eq!(
            r.model,
            FairValueModel::NoSignal(NoSignalReason::TimeRemainingInvalid)
        );

        let r = estimate_fair_value(100.0, 100.0, 60.0, 0.0, BAR_5M);
        assert_eq!(r.model, FairValueModel::NoSignal(NoSignalReason::VolInvalid));

        let r = estimate_fair_value(100.0, 100.0, 60.0, f64::NAN, BAR_5M);
        assert_eq!(r.model, FairValueModel::NoSignal(NoSignalReason::VolInvalid));

        let r = estimate_fair_value(-100.0, 100.0, 60.0, 5.0, BAR_5M);
        assert_eq!(r.model, FairValueModel::NoSignal(NoSignalReason::SpotInvalid));
    }

    #[test]
    fn at_strike_with_finite_vol_returns_half() {
        let r = estimate_fair_value(100.0, 100.0, 60.0, 5.0, BAR_5M);
        assert_eq!(r.model, FairValueModel::BsmBinary);
        assert!(approx_eq(r.p_up, 0.5, 1e-9), "p_up={}", r.p_up);
        assert!(approx_eq(r.p_down, 0.5, 1e-9));
    }

    #[test]
    fn spot_far_above_strike_approaches_one() {
        let r = estimate_fair_value(101.0, 100.0, 60.0, 5.0, BAR_5M);
        assert!(r.p_up > 0.99, "expected p_up > 0.99, got {}", r.p_up);
    }

    #[test]
    fn spot_far_below_strike_approaches_zero() {
        let r = estimate_fair_value(99.0, 100.0, 60.0, 5.0, BAR_5M);
        assert!(r.p_up < 0.01, "expected p_up < 0.01, got {}", r.p_up);
    }

    #[test]
    fn zero_time_remaining_uses_step_function() {
        let r = estimate_fair_value(100.5, 100.0, 0.5, 5.0, BAR_5M);
        assert_eq!(r.model, FairValueModel::StepFunctionDecided);
        assert_eq!(r.p_up, 1.0);

        let r = estimate_fair_value(99.5, 100.0, 0.5, 5.0, BAR_5M);
        assert_eq!(r.model, FairValueModel::StepFunctionDecided);
        assert_eq!(r.p_up, 0.0);
    }

    #[test]
    fn high_vol_softens_extremes() {
        let low_vol = estimate_fair_value(100.05, 100.0, 60.0, 1.0, BAR_5M);
        let high_vol = estimate_fair_value(100.05, 100.0, 60.0, 50.0, BAR_5M);
        assert!(
            high_vol.p_up < low_vol.p_up,
            "higher vol should soften extremes; low={} high={}",
            low_vol.p_up,
            high_vol.p_up
        );
    }

    #[test]
    fn p_up_plus_p_down_sums_to_one() {
        for spot_premium_bps in [-100, -10, 0, 10, 100] {
            for time_s in [10.0, 60.0, 120.0, 290.0] {
                for vol_bps in [1.0, 5.0, 50.0] {
                    let spot = 100.0 * (1.0 + spot_premium_bps as f64 / 10_000.0);
                    let r = estimate_fair_value(spot, 100.0, time_s, vol_bps, BAR_5M);
                    assert!(
                        approx_eq(r.p_up + r.p_down, 1.0, 1e-9),
                        "sum != 1 for spot={spot} time={time_s} vol={vol_bps}"
                    );
                }
            }
        }
    }

    #[test]
    fn more_time_remaining_pulls_extremes_toward_half() {
        let near = estimate_fair_value(100.01, 100.0, 30.0, 5.0, BAR_5M);
        let far = estimate_fair_value(100.01, 100.0, 290.0, 5.0, BAR_5M);
        assert!(near.p_up > far.p_up, "near={} far={}", near.p_up, far.p_up);
        assert!(far.p_up > 0.5, "far={}", far.p_up);
        assert!(near.p_up < 0.95, "near={}", near.p_up);
    }

    #[test]
    fn bar_duration_generalizes_to_15m() {
        // Same absolute remaining time and vol-per-bar: a 15m bar with 60s
        // left has burned more of its window than a 5m bar with 60s left,
        // so sigma_remaining is smaller relative to the bar's vol... but in
        // tau terms 60/900 < 60/300, so sqrt(tau) is smaller -> sharper p.
        let p5 = estimate_fair_value(100.01, 100.0, 60.0, 5.0, 300.0);
        let p15 = estimate_fair_value(100.01, 100.0, 60.0, 5.0, 900.0);
        assert!(p15.p_up > p5.p_up, "p15={} p5={}", p15.p_up, p5.p_up);
    }

    #[test]
    fn momentum_variant_zero_momentum_close_to_base() {
        // delta ~ ln(S/K) for small moves, so the two parameterizations agree
        // to within the linearization error.
        let base = estimate_fair_value(100.02, 100.0, 150.0, 5.0, BAR_5M);
        let mom = estimate_fair_value_with_momentum(100.02, 100.0, 0.5, 5.0 / 10_000.0, 0.0, BAR_5M);
        assert!(
            (base.p_up - mom.p_up).abs() < 1e-3,
            "base={} mom={}",
            base.p_up,
            mom.p_up
        );
    }

    #[test]
    fn positive_momentum_raises_p_up() {
        let flat = estimate_fair_value_with_momentum(100.0, 100.0, 0.5, 5e-4, 0.0, BAR_5M);
        let up = estimate_fair_value_with_momentum(100.0, 100.0, 0.5, 5e-4, 3e-4, BAR_5M);
        let down = estimate_fair_value_with_momentum(100.0, 100.0, 0.5, 5e-4, -3e-4, BAR_5M);
        assert!(up.p_up > flat.p_up && flat.p_up > down.p_up);
    }

    #[test]
    fn cdf_matches_known_values() {
        assert!(approx_eq(standard_normal_cdf(0.0), 0.5, 1e-7));
        assert!(approx_eq(standard_normal_cdf(1.0), 0.8413447, 1e-5));
        assert!(approx_eq(standard_normal_cdf(-1.0), 0.1586553, 1e-5));
        assert!(approx_eq(standard_normal_cdf(1.96), 0.9750021, 1e-5));
        assert!(approx_eq(standard_normal_cdf(-1.96), 0.0249979, 1e-5));
    }

    #[test]
    fn cdf_handles_extreme_inputs() {
        assert_eq!(standard_normal_cdf(f64::INFINITY), 1.0);
        assert_eq!(standard_normal_cdf(f64::NEG_INFINITY), 0.0);
        assert!(standard_normal_cdf(f64::NAN).is_nan());
        assert!(standard_normal_cdf(100.0) > 0.9999);
        assert!(standard_normal_cdf(-100.0) < 0.0001);
    }
}
