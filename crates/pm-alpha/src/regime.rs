//! Exogenous regime classification (v1: fixed thresholds, reporting-grade).
//!
//! Taxonomy follows docs/global_regime_classifier_router.md but uses ONLY
//! CEX spot inputs so the label is exogenous and live-safe: trailing realized
//! vol plus path shape (efficiency, sign-flip rate) over the last 30 minutes.
//! v1 is for per-regime result cells, not capital routing; thresholds are
//! fixed and documented rather than fitted.

use pm_types::SpotHistory;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Regime {
    CalmLowVol,
    CleanDirectional,
    ExpandedHighFlip,
    ExpandedMixed,
}

impl Regime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CalmLowVol => "calm_low_vol",
            Self::CleanDirectional => "clean_directional",
            Self::ExpandedHighFlip => "expanded_high_flip",
            Self::ExpandedMixed => "expanded_mixed",
        }
    }
}

/// Trailing path stats sampled at `step_s` over `lookback_s`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathStats {
    /// |net move| / sum(|step moves|), in [0, 1]; high = trending.
    pub efficiency: f64,
    /// Fraction of consecutive steps whose returns flip sign, in [0, 1].
    pub sign_flip_rate: f64,
    /// Realized vol over the trailing 180s, bps of a 180s horizon.
    pub vol_180s_bps: f64,
}

pub fn path_stats(spot: &SpotHistory, now_ns: i64, lookback_s: u32, step_s: u32) -> Option<PathStats> {
    let step_ns = step_s.max(1) as i64 * 1_000_000_000;
    let start_ns = now_ns - lookback_s as i64 * 1_000_000_000;
    // A near-empty tape repeats one stale price into every sample; require
    // real trades in the window before classifying.
    if spot.range(start_ns, now_ns).len() < 30 {
        return None;
    }
    let mut prices = Vec::with_capacity((lookback_s / step_s.max(1)) as usize + 1);
    let mut ts = start_ns;
    while ts <= now_ns {
        if let Some(p) = spot.price_at_or_before(ts)
            && p.is_finite()
            && p > 0.0
        {
            prices.push(p);
        }
        ts += step_ns;
    }
    if prices.len() < 12 {
        return None;
    }
    let mut sum_abs = 0.0;
    let mut flips = 0usize;
    let mut steps = 0usize;
    let mut prev_sign = 0i8;
    for w in prices.windows(2) {
        let d = w[1] - w[0];
        sum_abs += d.abs();
        let sign = if d > 0.0 {
            1
        } else if d < 0.0 {
            -1
        } else {
            0
        };
        if sign != 0 {
            if prev_sign != 0 && sign != prev_sign {
                flips += 1;
            }
            prev_sign = sign;
            steps += 1;
        }
    }
    let net = (prices[prices.len() - 1] - prices[0]).abs();
    let efficiency = if sum_abs > 0.0 { net / sum_abs } else { 0.0 };
    let sign_flip_rate = if steps > 1 {
        flips as f64 / (steps - 1) as f64
    } else {
        0.0
    };
    let vol_180s_bps = crate::vol::realized_vol_bps_over_bar(spot, now_ns, 1800, 1, 180)?;
    Some(PathStats {
        efficiency,
        sign_flip_rate,
        vol_180s_bps,
    })
}

/// Fixed v1 thresholds. Calm when 180s vol is under ~4.5 bps; otherwise the
/// path shape splits trending from choppy expansion.
pub fn classify(spot: &SpotHistory, now_ns: i64) -> Option<Regime> {
    let s = path_stats(spot, now_ns, 1800, 10)?;
    const CALM_VOL_BPS: f64 = 4.5;
    const TREND_EFFICIENCY: f64 = 0.30;
    const HIGH_FLIP: f64 = 0.55;
    Some(if s.vol_180s_bps < CALM_VOL_BPS {
        Regime::CalmLowVol
    } else if s.efficiency >= TREND_EFFICIENCY {
        Regime::CleanDirectional
    } else if s.sign_flip_rate >= HIGH_FLIP {
        Regime::ExpandedHighFlip
    } else {
        Regime::ExpandedMixed
    })
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
    fn flat_tape_is_calm() {
        let h = SpotHistory::new((0..2000).map(|s| tick(s, 100_000.0)).collect());
        assert_eq!(classify(&h, 1_900_000_000_000), Some(Regime::CalmLowVol));
    }

    #[test]
    fn noisy_climb_is_clean_directional() {
        // Strong drift with enough noise that realized vol clears the calm
        // floor (a noiseless ramp has zero return variance).
        let h = SpotHistory::new(
            (0..2000)
                .map(|s| {
                    let noise = if s % 2 == 0 { 10.0 } else { -10.0 };
                    tick(s, 100_000.0 + 20.0 * s as f64 + noise)
                })
                .collect(),
        );
        assert_eq!(classify(&h, 1_900_000_000_000), Some(Regime::CleanDirectional));
    }

    #[test]
    fn violent_alternation_is_high_flip() {
        let h = SpotHistory::new(
            (0..2000)
                .map(|s| {
                    let z = if (s / 10) % 2 == 0 { 1.0015 } else { 0.9985 };
                    tick(s, 100_000.0 * z)
                })
                .collect(),
        );
        assert_eq!(classify(&h, 1_900_000_000_000), Some(Regime::ExpandedHighFlip));
    }

    #[test]
    fn sparse_history_yields_none() {
        let h = SpotHistory::new(vec![tick(0, 100.0)]);
        assert_eq!(classify(&h, 1_000_000_000_000), None);
    }
}
