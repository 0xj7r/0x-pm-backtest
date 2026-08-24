//! TWAP-aware digital fair value for time-weighted-average settlement.
//!
//! Polymarket's post-Aug-2026 regime settles markets on a time-weighted
//! average price (TWAP) over the final `w` seconds of the window, not on the
//! terminal print. The classic [`fair_value`] digital `P(S_T >= K)` is no
//! longer the right primitive: a single late print cannot move the average,
//! so the effective variance of the settling quantity is smaller than the
//! terminal-price variance, and once the window is open the already-printed
//! portion of the average is locked in and dominates as time runs out.
//!
//! This module implements [`twap_digital`], the probability that the
//! time-weighted average over the final `w_s` seconds finishes at or above the
//! strike, under a driftless geometric Brownian motion on log-price. We
//! approximate the arithmetic TWAP by a geometric average of the log-price
//! (the log-average); this is the standard analytically tractable proxy and
//! shares the GBM's log-normality, giving a closed-form Gaussian distribution
//! for the log-average and a one-line CDF for the digital. The bias between
//! arithmetic and geometric averages is O(sigma^2 * w), small for the vol and
//! window lengths here and correctable later by the calibrator layer against
//! realized outcomes (same role it plays for the classic digital).

use crate::fair_value::standard_normal_cdf;

/// Variance below this is treated as a degenerate (decided) step function.
const VARIANCE_FLOOR: f64 = 1e-24;

/// P(TWAP over the final `w_s` seconds >= strike), driftless GBM approximation
/// on log-price (geometric-average proxy for the arithmetic TWAP; see module
/// docs).
///
/// Under driftless GBM, `ln S(t) = ln S_0 + sigma * W(t)` with `W` a standard
/// Brownian motion, so `ln S(t)` is Gaussian with mean `ln S_0` and variance
/// `sigma^2 * t`. The settling quantity is the log-average over the final
/// window, `A = (1/w) * integral of ln S over the window`, whose distribution
/// is Gaussian because Brownian integrals of deterministic kernels are
/// Gaussian. Two regimes:
///
/// # Case A: outside the window (`t_rem_s >= w_s`)
///
/// The window runs from `t1 = t_rem_s - w_s` to `t2 = t_rem_s` (both measured
/// from now, ending at close). The log-average is
/// `A = (1/w) * integral_{t1}^{t2} ln S(u) du`, with
/// - mean = `ln(spot)` (every `ln S(u)` centers on `ln S_0`), and
/// - variance = `sigma^2 / w^2 * integral_{t1}^{t2} integral_{t1}^{t2} min(u,v) du dv`.
///
/// The double integral of the Brownian covariance kernel `min(u,v)` over a
/// window `[t1, t2]` of width `w = t2 - t1` works out to
/// `w^2 * (t1 + w/3) = w^2 * ((t_rem_s - w_s) + w_s/3)`, so
/// `var(A) = sigma^2 * ((t_rem_s - w_s) + w_s/3)`. As `w_s -> 0` this reduces
/// to `sigma^2 * t_rem_s`, the classic terminal-price variance, and the
/// digital reduces to `Phi(ln(S/K) / (sigma * sqrt(t_rem_s)))`.
///
/// # Case B: inside the window (`t_rem_s < w_s`)
///
/// The window opened `e = w_s - t_rem_s` seconds ago and has `r = t_rem_s`
/// seconds left. The already-printed portion over `[-e, 0]` has (geometric)
/// average `ln(P_avg)` from `locked` (or `ln(spot)` if `locked` is `None`); the
/// remaining portion over `[0, r]` is a fresh Brownian integral from the
/// current spot. The final log-average is
/// `A = (1/w) * (e * ln(P_avg) + integral_0^r ln S(u) du)`, with
/// - mean = `(e * ln(P_avg) + r * ln(spot)) / w_s`, and
/// - variance = `sigma^2 * r^3 / (3 * w_s^2)`
///   (the remaining integral contributes `sigma^2 * r^3/3`; the printed
///   portion is already known and adds no variance).
///
/// As `r -> 0` the variance vanishes and the digital collapses to a step
/// function on the locked average versus the strike.
///
/// # Inputs
/// - `spot`: current price (positive, finite).
/// - `strike`: settlement strike (positive, finite).
/// - `sigma_per_sqrt_s`: log-price volatility per `sqrt(second)` (the `sigma`
///   above).
/// - `t_rem_s`: seconds until market close (`>= 0`).
/// - `w_s`: averaging-window length in seconds (`> 0`).
/// - `locked`: when inside the window, optionally `Some((elapsed_in_window_s,
///   partial_avg_price))` giving the already-realized portion of the average;
///   `None` approximates the elapsed portion's average with `spot`.
///
/// # Validation
/// Non-finite or non-positive `spot`/`strike`/`w_s`, negative `t_rem_s`, or
/// non-finite `sigma` return `0.5` (no signal). `sigma <= 0` or a variance
/// below [`VARIANCE_FLOOR`] degrades to the deterministic step (1.0 or 0.0 by
/// the sign of the relevant mean-versus-strike comparison). Output is clamped
/// to `[0, 1]`.
pub fn twap_digital(
    spot: f64,
    strike: f64,
    sigma_per_sqrt_s: f64,
    t_rem_s: f64,
    w_s: f64,
    locked: Option<(f64, f64)>,
) -> f64 {
    // Input validation: mirror fair_value.rs conventions. Anything that makes
    // the belief uncomputable returns the safe no-signal value 0.5.
    if !spot.is_finite() || spot <= 0.0 {
        return 0.5;
    }
    if !strike.is_finite() || strike <= 0.0 {
        return 0.5;
    }
    if !w_s.is_finite() || w_s <= 0.0 {
        return 0.5;
    }
    if !t_rem_s.is_finite() || t_rem_s < 0.0 {
        return 0.5;
    }
    if !sigma_per_sqrt_s.is_finite() {
        return 0.5;
    }

    let ln_spot = spot.ln();
    let ln_strike = strike.ln();
    let sigma = sigma_per_sqrt_s;

    // Distribution parameters of the log-average A.
    let (mean, variance) = if t_rem_s >= w_s {
        // Case A: outside the window.
        let v = (t_rem_s - w_s) + w_s / 3.0;
        let variance = sigma * sigma * v;
        (ln_spot, variance)
    } else {
        // Case B: inside the window.
        let r = t_rem_s;
        let e = w_s - t_rem_s;
        let p_avg = match locked {
            Some((_, p)) if p.is_finite() && p > 0.0 => p,
            _ => spot,
        };
        let mean = (e * p_avg.ln() + r * ln_spot) / w_s;
        let variance = sigma * sigma * r * r * r / (3.0 * w_s * w_s);
        (mean, variance)
    };

    // Degenerate vol / variance: collapse to the deterministic step.
    if sigma <= 0.0 || variance < VARIANCE_FLOOR {
        let p: f64 = if mean > ln_strike { 1.0 } else { 0.0 };
        return p.clamp(0.0, 1.0);
    }

    let z = (mean - ln_strike) / variance.sqrt();
    standard_normal_cdf(z).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    /// Classic terminal-price digital at the same inputs, for reduction checks.
    fn classic_digital(spot: f64, strike: f64, sigma: f64, t_rem_s: f64) -> f64 {
        standard_normal_cdf((spot / strike).ln() / (sigma * t_rem_s.sqrt()))
    }

    #[test]
    fn reduces_to_classic_digital_as_w_shrinks() {
        // Outside the window (t_rem_s >= w_s); as w_s -> 0 the effective
        // variance sigma^2*((t_rem - w) + w/3) -> sigma^2 * t_rem, matching the
        // classic terminal-price digital.
        let sigma = 5e-3;
        let t_rem_s = 60.0;
        let w_s = 1e-7;

        // ITM.
        let spot = 100.5;
        let strike = 100.0;
        let classic = classic_digital(spot, strike, sigma, t_rem_s);
        let twap = twap_digital(spot, strike, sigma, t_rem_s, w_s, None);
        assert!(approx_eq(twap, classic, 1e-6), "ITM twap={twap} classic={classic}");

        // OTM.
        let spot = 99.5;
        let classic = classic_digital(spot, strike, sigma, t_rem_s);
        let twap = twap_digital(spot, strike, sigma, t_rem_s, w_s, None);
        assert!(approx_eq(twap, classic, 1e-6), "OTM twap={twap} classic={classic}");
    }

    #[test]
    fn narrower_effective_variance_outside_window_raises_p() {
        // Spot slightly above strike: the TWAP's effective variance is smaller
        // than the terminal-price variance (by 2w/3 of the window), so with a
        // positive log-moneyness the sharper distribution pushes p higher.
        let spot = 100.01;
        let strike = 100.0;
        let sigma = 5e-3;
        let t_rem_s = 300.0;
        let w_s = 60.0;

        let classic = classic_digital(spot, strike, sigma, t_rem_s);
        let twap = twap_digital(spot, strike, sigma, t_rem_s, w_s, None);
        assert!(
            twap > classic,
            "expected twap > classic outside window; twap={twap} classic={classic}"
        );
    }

    #[test]
    fn locked_average_dominates_near_close() {
        // 55 of 60s elapsed, 5s remaining. Spot is 0.2% BELOW strike but the
        // locked partial average is 0.2% ABOVE strike; the locked average
        // dominates and p should be high. Mirrored case should be low.
        let strike = 100.0;
        let w_s = 60.0;
        let t_rem_s = 5.0; // 55s elapsed, 5s remaining
        let sigma = 5e-3;

        let spot_below = 100.0 * (1.0 - 0.002);
        let avg_above = 100.0 * (1.0 + 0.002);
        let p = twap_digital(spot_below, strike, sigma, t_rem_s, w_s, Some((55.0, avg_above)));
        assert!(p > 0.95, "locked-above should dominate, got p={p}");

        let spot_above = 100.0 * (1.0 + 0.002);
        let avg_below = 100.0 * (1.0 - 0.002);
        let p_mirrored =
            twap_digital(spot_above, strike, sigma, t_rem_s, w_s, Some((55.0, avg_below)));
        assert!(p_mirrored < 0.05, "locked-below should dominate, got p={p_mirrored}");
    }

    #[test]
    fn step_behavior_at_r_near_zero() {
        // With 1e-9s remaining the variance collapses below the floor; the
        // result is a step on the locked average versus the strike.
        let strike = 100.0;
        let w_s = 60.0;
        let t_rem_s = 1e-9;
        let sigma = 5e-3;

        let p_up = twap_digital(100.0, strike, sigma, t_rem_s, w_s, Some((60.0, 101.0)));
        assert_eq!(p_up, 1.0, "locked above strike at r->0 should step to 1.0");

        let p_down = twap_digital(100.0, strike, sigma, t_rem_s, w_s, Some((60.0, 99.0)));
        assert_eq!(p_down, 0.0, "locked below strike at r->0 should step to 0.0");
    }

    #[test]
    fn monotone_in_spot_outside_window() {
        let strike = 100.0;
        let sigma = 5e-3;
        let t_rem_s = 300.0;
        let w_s = 60.0;
        let mut prev = -f64::INFINITY;
        for i in -200..=200 {
            let spot = 100.0 + i as f64 * 0.01;
            let p = twap_digital(spot, strike, sigma, t_rem_s, w_s, None);
            assert!(p + 1e-12 >= prev, "non-monotone outside at spot={spot}: p={p} prev={prev}");
            prev = p;
        }
        // Strictly increasing somewhere across the strike.
        let lo = twap_digital(99.5, strike, sigma, t_rem_s, w_s, None);
        let hi = twap_digital(100.5, strike, sigma, t_rem_s, w_s, None);
        assert!(hi > lo, "expected strict increase across strike outside window");
    }

    #[test]
    fn monotone_in_spot_inside_window() {
        // Inside the window with locked = None the mean reduces to ln(spot) and
        // the variance is constant in spot, so p is monotone increasing in spot.
        let strike = 100.0;
        let sigma = 5e-3;
        let w_s = 60.0;
        let t_rem_s = 30.0; // inside the window
        let mut prev = -f64::INFINITY;
        for i in -200..=200 {
            let spot = 100.0 + i as f64 * 0.01;
            let p = twap_digital(spot, strike, sigma, t_rem_s, w_s, None);
            assert!(p + 1e-12 >= prev, "non-monotone inside at spot={spot}: p={p} prev={prev}");
            prev = p;
        }
        let lo = twap_digital(99.5, strike, sigma, t_rem_s, w_s, None);
        let hi = twap_digital(100.5, strike, sigma, t_rem_s, w_s, None);
        assert!(hi > lo, "expected strict increase across strike inside window");
    }

    #[test]
    fn nan_and_negative_inputs_return_half() {
        assert_eq!(twap_digital(f64::NAN, 100.0, 1e-3, 60.0, 30.0, None), 0.5);
        assert_eq!(twap_digital(-1.0, 100.0, 1e-3, 60.0, 30.0, None), 0.5);
        assert_eq!(twap_digital(100.0, f64::NAN, 1e-3, 60.0, 30.0, None), 0.5);
        assert_eq!(twap_digital(100.0, 0.0, 1e-3, 60.0, 30.0, None), 0.5);
        assert_eq!(twap_digital(100.0, 100.0, f64::NAN, 60.0, 30.0, None), 0.5);
        assert_eq!(twap_digital(100.0, 100.0, 1e-3, -1.0, 30.0, None), 0.5);
        assert_eq!(twap_digital(100.0, 100.0, 1e-3, 60.0, 0.0, None), 0.5);
        assert_eq!(twap_digital(100.0, 100.0, 1e-3, f64::INFINITY, 30.0, None), 0.5);
    }

    #[test]
    fn bounded_over_stress_grid() {
        let spots = [1e-6, 1e-3, 1.0, 50.0, 100.0, 1e3, 1e6, f64::INFINITY];
        let strikes = [1e-6, 1.0, 100.0, 1e3];
        let sigmas = [0.0, -1e-3, 1e-9, 1e-3, 1e-2, 1.0, f64::NAN];
        let t_rems = [0.0, 1e-9, 1.0, 30.0, 60.0, 300.0];
        let ws = [1e-6, 1.0, 30.0, 60.0, 300.0];
        for &spot in &spots {
            for &strike in &strikes {
                for &sigma in &sigmas {
                    for &t_rem in &t_rems {
                        for &w in &ws {
                            let p = twap_digital(spot, strike, sigma, t_rem, w, None);
                            assert!(p.is_finite(), "non-finite at spot={spot} strike={strike} sigma={sigma} t={t_rem} w={w}");
                            assert!(p >= 0.0 && p <= 1.0, "out of bounds p={p} at spot={spot} strike={strike} sigma={sigma} t={t_rem} w={w}");
                            let p_locked =
                                twap_digital(spot, strike, sigma, t_rem, w, Some((w - t_rem, spot * 1.1)));
                            assert!(p_locked.is_finite() && p_locked >= 0.0 && p_locked <= 1.0);
                        }
                    }
                }
            }
        }
    }
}
