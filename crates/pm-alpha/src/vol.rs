//! Trailing realized-vol estimator over the underlying spot tape.
//!
//! Output is the std-dev of log returns scaled to ONE BAR horizon, in basis
//! points — exactly the `realized_vol_bps_over_bar` input the fair-value
//! model expects. Estimation: sample last-price at a fixed `sample_dt_s`
//! cadence over the trailing `lookback_s` window, take log returns between
//! consecutive samples, and scale the per-step std-dev by
//! `sqrt(bar_secs / sample_dt_s)` (diffusion scaling).

use pm_types::SpotHistory;

/// Minimum number of valid log-return samples to emit an estimate.
const MIN_SAMPLES: usize = 30;

/// Selectable vol estimator. `Realized` is the original equal-weight
/// trailing estimator (the default, behavior-identical); the others are
/// evaluated on the same 1s sampling grid and scaled to the bar the same way.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VolEstimator {
    #[default]
    Realized,
    /// RiskMetrics-style exponentially-weighted variance (zero-mean),
    /// parameterized by half-life in seconds: per-step
    /// `lambda = 0.5^(sample_dt_s / halflife_s)`.
    Ewma { halflife_s: f64 },
    /// `max(realized over the fast window, realized over the configured
    /// lookback)` — picks up vol expansion the slow window lags.
    BlendFastSlow { fast_lookback_s: u32 },
    /// Realized over the configured lookback scaled by an hour-of-day (UTC)
    /// seasonality factor supplied by the caller (learned out-of-window).
    Seasonal { factors: [f64; 24] },
    /// Bipower variation: per-step variance `(pi/2)·mean(|r_i||r_{i-1}|)`,
    /// which downweights single-print jumps relative to squared returns.
    JumpRobust,
}

/// Dispatch over [`VolEstimator`]. `Realized` delegates to
/// [`realized_vol_bps_over_bar`] untouched.
pub fn vol_bps_over_bar(
    spot: &SpotHistory,
    now_ns: i64,
    lookback_s: u32,
    sample_dt_s: u32,
    bar_secs: u32,
    estimator: VolEstimator,
) -> Option<f64> {
    match estimator {
        VolEstimator::Realized => {
            realized_vol_bps_over_bar(spot, now_ns, lookback_s, sample_dt_s, bar_secs)
        }
        VolEstimator::Ewma { halflife_s } => {
            ewma_vol_bps_over_bar(spot, now_ns, lookback_s, sample_dt_s, bar_secs, halflife_s)
        }
        VolEstimator::BlendFastSlow { fast_lookback_s } => {
            let slow = realized_vol_bps_over_bar(spot, now_ns, lookback_s, sample_dt_s, bar_secs)?;
            let fast =
                realized_vol_bps_over_bar(spot, now_ns, fast_lookback_s, sample_dt_s, bar_secs);
            Some(fast.map_or(slow, |f| f.max(slow)))
        }
        VolEstimator::Seasonal { factors } => {
            let base = realized_vol_bps_over_bar(spot, now_ns, lookback_s, sample_dt_s, bar_secs)?;
            Some(base * seasonal_multiplier(&factors, now_ns, lookback_s))
        }
        VolEstimator::JumpRobust => {
            bipower_vol_bps_over_bar(spot, now_ns, lookback_s, sample_dt_s, bar_secs)
        }
    }
}

pub fn hour_of_day_utc(now_ns: i64) -> usize {
    (now_ns.div_euclid(1_000_000_000).rem_euclid(86_400) / 3_600) as usize
}

/// Seasonal correction for a trailing-window vol estimate: the window
/// already carries the seasonal level of the hours it spans, so the
/// multiplier is `f[current hour] / (window-weighted mix of f)`, not the
/// raw factor (which would double-count deep into an hour). Two-hour
/// approximation, exact for `lookback_s <= 3600`.
pub fn seasonal_multiplier(factors: &[f64; 24], now_ns: i64, lookback_s: u32) -> f64 {
    let sane = |f: f64| if f.is_finite() && f > 0.0 { f } else { 1.0 };
    let h = hour_of_day_utc(now_ns);
    let f_now = sane(factors[h]);
    let f_prev = sane(factors[(h + 23) % 24]);
    let elapsed_in_hour = now_ns.div_euclid(1_000_000_000).rem_euclid(3_600) as f64;
    let w = if lookback_s == 0 {
        1.0
    } else {
        (elapsed_in_hour / lookback_s as f64).min(1.0)
    };
    let window_level = w * f_now + (1.0 - w) * f_prev;
    if window_level <= 0.0 {
        return 1.0;
    }
    (f_now / window_level).clamp(0.25, 4.0)
}

/// Log returns on the same 1-per-`sample_dt_s` grid the realized estimator
/// uses (last trade at-or-before each grid instant; stale prices repeat).
fn sampled_log_returns(
    spot: &SpotHistory,
    now_ns: i64,
    lookback_s: u32,
    sample_dt_s: u32,
) -> Vec<f64> {
    let dt_ns = sample_dt_s as i64 * 1_000_000_000;
    let start_ns = now_ns - lookback_s as i64 * 1_000_000_000;
    let window = spot.range(start_ns, now_ns);
    let mut idx = 0usize;
    let mut last_price = spot.price_at_or_before(start_ns);

    let mut returns: Vec<f64> = Vec::with_capacity((lookback_s / sample_dt_s) as usize);
    let mut prev: Option<f64> = None;
    let mut ts = start_ns;
    while ts <= now_ns {
        while idx < window.len() && window[idx].ts_ns <= ts {
            last_price = Some(window[idx].price);
            idx += 1;
        }
        if let Some(price) = last_price
            && price.is_finite()
            && price > 0.0
        {
            if let Some(p0) = prev {
                returns.push((price / p0).ln());
            }
            prev = Some(price);
        }
        ts += dt_ns;
    }
    returns
}

fn scale_to_bar_bps(var_step: f64, sample_dt_s: u32, bar_secs: u32) -> Option<f64> {
    if !var_step.is_finite() || var_step < 0.0 {
        return None;
    }
    let sigma_bar = var_step.sqrt() * ((bar_secs as f64) / (sample_dt_s as f64)).sqrt();
    Some(sigma_bar * 10_000.0)
}

fn ewma_vol_bps_over_bar(
    spot: &SpotHistory,
    now_ns: i64,
    lookback_s: u32,
    sample_dt_s: u32,
    bar_secs: u32,
    halflife_s: f64,
) -> Option<f64> {
    if lookback_s == 0 || sample_dt_s == 0 || bar_secs == 0 {
        return None;
    }
    if !halflife_s.is_finite() || halflife_s <= 0.0 {
        return None;
    }
    let returns = sampled_log_returns(spot, now_ns, lookback_s, sample_dt_s);
    if returns.len() < MIN_SAMPLES {
        return None;
    }
    let lambda = 0.5_f64.powf(sample_dt_s as f64 / halflife_s);
    // Zero-mean EWMA variance, weights normalized over the finite window.
    let mut var = 0.0;
    let mut wsum = 0.0;
    let mut w = 1.0;
    for r in returns.iter().rev() {
        var += w * r * r;
        wsum += w;
        w *= lambda;
    }
    scale_to_bar_bps(var / wsum, sample_dt_s, bar_secs)
}

fn bipower_vol_bps_over_bar(
    spot: &SpotHistory,
    now_ns: i64,
    lookback_s: u32,
    sample_dt_s: u32,
    bar_secs: u32,
) -> Option<f64> {
    if lookback_s == 0 || sample_dt_s == 0 || bar_secs == 0 {
        return None;
    }
    let returns = sampled_log_returns(spot, now_ns, lookback_s, sample_dt_s);
    if returns.len() < MIN_SAMPLES + 1 {
        return None;
    }
    let n = returns.len();
    let sum: f64 = returns.windows(2).map(|w| w[0].abs() * w[1].abs()).sum();
    let var_step = std::f64::consts::FRAC_PI_2 * sum / (n - 1) as f64;
    scale_to_bar_bps(var_step, sample_dt_s, bar_secs)
}

pub fn realized_vol_bps_over_bar(
    spot: &SpotHistory,
    now_ns: i64,
    lookback_s: u32,
    sample_dt_s: u32,
    bar_secs: u32,
) -> Option<f64> {
    if lookback_s == 0 || sample_dt_s == 0 || bar_secs == 0 {
        return None;
    }
    let dt_ns = sample_dt_s as i64 * 1_000_000_000;
    let start_ns = now_ns - lookback_s as i64 * 1_000_000_000;

    // One range scan + pointer walk instead of a binary search per grid
    // point (the estimator runs every decision tick; this is the hot path).
    // Semantics: last trade at-or-before each grid instant; stale prices
    // repeat, contributing zero returns — correct for a no-trade interval.
    let window = spot.range(start_ns, now_ns);
    let mut idx = 0usize;
    let mut last_price = spot.price_at_or_before(start_ns);

    let mut returns: Vec<f64> = Vec::with_capacity((lookback_s / sample_dt_s) as usize);
    let mut prev: Option<f64> = None;
    let mut ts = start_ns;
    while ts <= now_ns {
        while idx < window.len() && window[idx].ts_ns <= ts {
            last_price = Some(window[idx].price);
            idx += 1;
        }
        if let Some(price) = last_price {
            if price.is_finite() && price > 0.0 {
                if let Some(p0) = prev {
                    returns.push((price / p0).ln());
                }
                prev = Some(price);
            }
        }
        ts += dt_ns;
    }

    if returns.len() < MIN_SAMPLES {
        return None;
    }

    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n;
    let var = returns.iter().map(|r| (r - mean) * (r - mean)).sum::<f64>() / (n - 1.0);
    if !var.is_finite() || var < 0.0 {
        return None;
    }
    let sigma_step = var.sqrt();
    let sigma_bar = sigma_step * ((bar_secs as f64) / (sample_dt_s as f64)).sqrt();
    Some(sigma_bar * 10_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::SpotTick;

    fn tick(ts_s: i64, price: f64) -> SpotTick {
        SpotTick {
            ts_ns: ts_s * 1_000_000_000,
            price,
            quantity: 1.0,
            is_buyer_maker: false,
        }
    }

    #[test]
    fn constant_price_yields_zero_vol() {
        let ticks: Vec<SpotTick> = (0..2000).map(|s| tick(s, 100_000.0)).collect();
        let h = SpotHistory::new(ticks);
        let v = realized_vol_bps_over_bar(&h, 1_900 * 1_000_000_000, 1800, 1, 300).unwrap();
        assert!(v.abs() < 1e-9, "got {v}");
    }

    #[test]
    fn alternating_returns_recover_known_sigma() {
        // Price alternates +r / -r each second: per-step sigma ~ r.
        // Expected bar vol = r * sqrt(300) in bps.
        let r = 1e-4;
        let mut price = 100_000.0;
        let mut ticks = Vec::new();
        for s in 0..2000_i64 {
            ticks.push(tick(s, price));
            price *= if s % 2 == 0 { 1.0 + r } else { 1.0 - r };
        }
        let h = SpotHistory::new(ticks);
        let v = realized_vol_bps_over_bar(&h, 1_900 * 1_000_000_000, 1800, 1, 300).unwrap();
        let expected = r * (300.0_f64).sqrt() * 10_000.0;
        assert!(
            (v - expected).abs() / expected < 0.10,
            "got {v}, expected ~{expected}"
        );
    }

    #[test]
    fn sparse_history_returns_none() {
        let h = SpotHistory::new(vec![tick(0, 100.0), tick(10, 101.0)]);
        assert_eq!(
            realized_vol_bps_over_bar(&h, 10_000_000_000, 1800, 1, 300),
            None
        );
    }

    #[test]
    fn empty_history_returns_none() {
        let h = SpotHistory::default();
        assert_eq!(realized_vol_bps_over_bar(&h, 0, 1800, 1, 300), None);
    }

    #[test]
    fn zero_params_return_none() {
        let h = SpotHistory::new((0..100).map(|s| tick(s, 100.0)).collect());
        assert_eq!(realized_vol_bps_over_bar(&h, 0, 0, 1, 300), None);
        assert_eq!(realized_vol_bps_over_bar(&h, 0, 1800, 0, 300), None);
        assert_eq!(realized_vol_bps_over_bar(&h, 0, 1800, 1, 0), None);
    }

    fn alternating_history(r: f64, n_secs: i64) -> SpotHistory {
        let mut price = 100_000.0;
        let mut ticks = Vec::new();
        for s in 0..n_secs {
            ticks.push(tick(s, price));
            price *= if s % 2 == 0 { 1.0 + r } else { 1.0 - r };
        }
        SpotHistory::new(ticks)
    }

    const NOW: i64 = 1_900 * 1_000_000_000;

    #[test]
    fn dispatch_realized_is_bitwise_identical() {
        let h = alternating_history(1e-4, 2000);
        let direct = realized_vol_bps_over_bar(&h, NOW, 1800, 1, 300);
        let via = vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::Realized);
        assert_eq!(direct, via);
        assert!(direct.is_some());
    }

    #[test]
    fn ewma_recovers_constant_sigma() {
        // Constant-magnitude alternating returns: any weighting recovers ~r.
        let r = 1e-4;
        let h = alternating_history(r, 2000);
        let v = vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::Ewma { halflife_s: 600.0 })
            .unwrap();
        let expected = r * (300.0_f64).sqrt() * 10_000.0;
        assert!((v - expected).abs() / expected < 0.05, "got {v}, expected ~{expected}");
    }

    #[test]
    fn ewma_weights_recent_regime_more_than_realized() {
        // Calm first 1500s, violent last 400s: a 120s half-life EWMA should
        // sit far above the equal-weight realized estimate.
        let mut price = 100_000.0;
        let mut ticks = Vec::new();
        for s in 0..1900_i64 {
            ticks.push(tick(s, price));
            let r = if s < 1500 { 1e-5 } else { 5e-4 };
            price *= if s % 2 == 0 { 1.0 + r } else { 1.0 - r };
        }
        let h = SpotHistory::new(ticks);
        let realized = realized_vol_bps_over_bar(&h, NOW, 1800, 1, 300).unwrap();
        let ewma = vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::Ewma { halflife_s: 120.0 })
            .unwrap();
        assert!(ewma > 1.5 * realized, "ewma {ewma} vs realized {realized}");
    }

    #[test]
    fn ewma_invalid_halflife_returns_none() {
        let h = alternating_history(1e-4, 2000);
        assert_eq!(vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::Ewma { halflife_s: 0.0 }), None);
        assert_eq!(
            vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::Ewma { halflife_s: f64::NAN }),
            None
        );
    }

    #[test]
    fn blend_takes_max_of_fast_and_slow() {
        // Vol expansion in the last 300s: fast window must dominate.
        let mut price = 100_000.0;
        let mut ticks = Vec::new();
        for s in 0..1900_i64 {
            ticks.push(tick(s, price));
            let r = if s < 1600 { 1e-5 } else { 5e-4 };
            price *= if s % 2 == 0 { 1.0 + r } else { 1.0 - r };
        }
        let h = SpotHistory::new(ticks);
        let slow = realized_vol_bps_over_bar(&h, NOW, 1800, 1, 300).unwrap();
        let fast = realized_vol_bps_over_bar(&h, NOW, 300, 1, 300).unwrap();
        let blend = vol_bps_over_bar(
            &h,
            NOW,
            1800,
            1,
            300,
            VolEstimator::BlendFastSlow { fast_lookback_s: 300 },
        )
        .unwrap();
        assert_eq!(blend, fast.max(slow));
        assert!(blend > slow);
    }

    #[test]
    fn blend_falls_back_to_slow_when_fast_sparse() {
        let h = alternating_history(1e-4, 2000);
        // 10s fast window at 1s sampling -> < MIN_SAMPLES returns -> None.
        let slow = realized_vol_bps_over_bar(&h, NOW, 1800, 1, 300).unwrap();
        let blend = vol_bps_over_bar(
            &h,
            NOW,
            1800,
            1,
            300,
            VolEstimator::BlendFastSlow { fast_lookback_s: 10 },
        )
        .unwrap();
        assert_eq!(blend, slow);
    }

    #[test]
    fn seasonal_scales_by_lag_corrected_multiplier() {
        let h = alternating_history(1e-4, 2000);
        let base = realized_vol_bps_over_bar(&h, NOW, 1800, 1, 300).unwrap();
        // NOW = 1900s after midnight -> hour 0, 1900s into the hour.
        let mut factors = [1.0_f64; 24];
        factors[0] = 1.5;
        let m = seasonal_multiplier(&factors, NOW, 1800);
        // Window fully inside hour 0 (1900s elapsed >= 1800s lookback):
        // multiplier = f0 / f0 = 1 (no double count deep into the hour).
        assert!((m - 1.0).abs() < 1e-12);
        let v = vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::Seasonal { factors }).unwrap();
        assert!((v - base * m).abs() < 1e-9);

        // 600s into hour 0 with a 1800s window: w = 1/3 of the window in
        // hour 0, 2/3 in hour 23 -> multiplier = 1.5 / (1.5/3 + 2/3).
        let m = seasonal_multiplier(&factors, 600 * 1_000_000_000, 1800);
        let expected = 1.5 / (1.5 / 3.0 + 2.0 / 3.0);
        assert!((m - expected).abs() < 1e-12, "got {m}, expected {expected}");

        // Non-finite factors degrade to identity.
        factors[0] = f64::NAN;
        assert_eq!(seasonal_multiplier(&factors, NOW, 1800), 1.0);
    }

    #[test]
    fn hour_of_day_utc_handles_offsets() {
        assert_eq!(hour_of_day_utc(0), 0);
        assert_eq!(hour_of_day_utc(3_600 * 1_000_000_000), 1);
        assert_eq!(hour_of_day_utc(86_399 * 1_000_000_000), 23);
        assert_eq!(hour_of_day_utc(86_400 * 1_000_000_000), 0);
    }

    #[test]
    fn jump_robust_matches_realized_on_diffusive_tape() {
        // For alternating constant-magnitude returns, |r_i||r_{i-1}| = r^2,
        // so bipower*(pi/2) sits ~(pi/2)x the squared-return variance; check
        // it recovers sigma within the bipower scaling (i.e. same order).
        let r = 1e-4;
        let h = alternating_history(r, 2000);
        let v = vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::JumpRobust).unwrap();
        let expected = r * (std::f64::consts::FRAC_PI_2).sqrt() * (300.0_f64).sqrt() * 10_000.0;
        assert!((v - expected).abs() / expected < 0.05, "got {v}, expected ~{expected}");
    }

    #[test]
    fn jump_robust_downweights_single_print_jump() {
        // Calm tape with one 100bps print: realized var absorbs r^2 fully;
        // bipower only |r_jump|*|r_calm| terms -> far smaller.
        let mut price = 100_000.0;
        let mut ticks = Vec::new();
        for s in 0..1900_i64 {
            ticks.push(tick(s, price));
            let r = if s == 1700 { 1e-2 } else { 1e-5 };
            price *= if s % 2 == 0 { 1.0 + r } else { 1.0 - r };
        }
        let h = SpotHistory::new(ticks);
        let realized = realized_vol_bps_over_bar(&h, NOW, 1800, 1, 300).unwrap();
        let robust = vol_bps_over_bar(&h, NOW, 1800, 1, 300, VolEstimator::JumpRobust).unwrap();
        assert!(robust < 0.25 * realized, "robust {robust} vs realized {realized}");
    }
}
