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

    let mut returns: Vec<f64> = Vec::with_capacity((lookback_s / sample_dt_s) as usize);
    let mut prev: Option<f64> = None;
    let mut ts = start_ns;
    while ts <= now_ns {
        // Last trade at-or-before the sample instant. Stale prices repeat,
        // contributing zero returns — correct for a no-trade interval.
        if let Some(price) = spot.price_at_or_before(ts) {
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
}
