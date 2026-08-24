//! Live-safe spot regime features for the runner's decision log.
//!
//! Both items here are engine machinery, not strategy logic: the runner builds
//! a [`WhipsawRiskSnapshot`] per event to feed the canonical model's risk score
//! and the decision-log row, and labels each decision with a
//! [`MarketRegimeCluster`]. They outlived the strategies that also read them.

use pm_types::SpotHistory;

#[derive(Debug, Clone, Copy, Default)]
pub struct WhipsawRiskSnapshot {
    pub score: f32,
    pub path_efficiency: f32,
    pub sign_flip_rate: f32,
    pub realized_vol_180s_bps: f32,
    pub reversal_pressure: f32,
    pub sample_count: usize,
}

impl WhipsawRiskSnapshot {
    pub fn from_history(now_ns: i64, spot: &SpotHistory) -> Self {
        const WINDOW_SECS: i64 = 180;
        const STEP_SECS: i64 = 5;
        let start_ns = now_ns - WINDOW_SECS * 1_000_000_000;
        let slice = spot.range(start_ns, now_ns);
        if slice.len() < 12 {
            return Self::default();
        }

        let mut sampled = Vec::with_capacity((WINDOW_SECS / STEP_SECS) as usize + 1);
        let mut next_ns = start_ns;
        while next_ns <= now_ns {
            if let Some(price) = spot.price_at_or_before(next_ns) {
                if price.is_finite() && price > 0.0 {
                    sampled.push((next_ns, price));
                }
            }
            next_ns += STEP_SECS * 1_000_000_000;
        }
        if sampled.len() < 8 {
            return Self::default();
        }

        let first = sampled.first().map(|(_, p)| *p).unwrap_or(0.0);
        let last = sampled.last().map(|(_, p)| *p).unwrap_or(0.0);
        if first <= 0.0 || last <= 0.0 {
            return Self::default();
        }

        let mut path_abs = 0.0f64;
        let mut sumsq = 0.0f64;
        let mut returns = Vec::with_capacity(sampled.len().saturating_sub(1));
        for pair in sampled.windows(2) {
            let prev = pair[0].1;
            let next = pair[1].1;
            if prev <= 0.0 || next <= 0.0 {
                continue;
            }
            let r = (next / prev).ln();
            if r.is_finite() {
                path_abs += r.abs();
                sumsq += r * r;
                returns.push(r);
            }
        }
        if returns.len() < 7 || path_abs <= 0.0 {
            return Self::default();
        }

        let net = (last / first).ln().abs();
        let path_efficiency = (net / path_abs).clamp(0.0, 1.0) as f32;
        let mut flips = 0usize;
        let mut prev_sign = 0i8;
        for r in &returns {
            let sign = if *r > 0.0 {
                1
            } else if *r < 0.0 {
                -1
            } else {
                0
            };
            if sign != 0 {
                if prev_sign != 0 && sign != prev_sign {
                    flips += 1;
                }
                prev_sign = sign;
            }
        }
        let sign_flip_rate =
            (flips as f32 / returns.len().saturating_sub(1).max(1) as f32).clamp(0.0, 1.0);
        let realized_vol_180s_bps = ((sumsq / returns.len() as f64).sqrt() * 10_000.0) as f32;

        let ret_30 = spot
            .simple_return(now_ns, 30 * 1_000_000_000)
            .unwrap_or(0.0);
        let ret_120 = spot
            .simple_return(now_ns, 120 * 1_000_000_000)
            .unwrap_or(0.0);
        let ret_180 = spot
            .simple_return(now_ns, 180 * 1_000_000_000)
            .unwrap_or(0.0);
        let reversal = if ret_30.abs() * 10_000.0 >= 1.0
            && ret_120.abs().max(ret_180.abs()) * 10_000.0 >= 2.0
            && ret_30.signum() != ret_120.signum()
            && ret_30.signum() != ret_180.signum()
        {
            1.0
        } else {
            0.0
        };

        let vol_component = (realized_vol_180s_bps / 7.5).clamp(0.0, 1.0);
        let chop = (1.0 - path_efficiency) * vol_component;
        let reversal_pressure = (0.7 * sign_flip_rate + 0.3 * reversal as f32).clamp(0.0, 1.0);
        let score =
            (0.50 * chop + 0.30 * sign_flip_rate + 0.15 * vol_component + 0.05 * reversal as f32)
                .clamp(0.0, 1.0);

        Self {
            score,
            path_efficiency,
            sign_flip_rate,
            realized_vol_180s_bps,
            reversal_pressure,
            sample_count: sampled.len(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketRegimeCluster {
    ExpandedHighFlip,
    ExpandedReversalPressure,
    FlowAdverseVolCluster,
    LowEfficiencyNonreversal,
    CleanDirectionalPath,
    CalmLowVol,
    EarlyTightRange,
    MixedNeutral,
}

impl MarketRegimeCluster {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExpandedHighFlip => "expanded_high_flip",
            Self::ExpandedReversalPressure => "expanded_reversal_pressure",
            Self::FlowAdverseVolCluster => "flow_adverse_vol_cluster",
            Self::LowEfficiencyNonreversal => "low_efficiency_nonreversal",
            Self::CleanDirectionalPath => "clean_directional_path",
            Self::CalmLowVol => "calm_low_vol",
            Self::EarlyTightRange => "early_tight_range",
            Self::MixedNeutral => "mixed_neutral",
        }
    }
}

impl std::fmt::Display for MarketRegimeCluster {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Classify the live-safe market regime using the same threshold ordering as
/// `scripts/strategy_regime_clusters.py`.
pub fn classify_market_regime_cluster(
    market_yes_range_so_far: f32,
    regime_path_efficiency: f32,
    regime_reversal_pressure: f32,
    regime_sign_flip_rate: f32,
    regime_realized_vol_180s_bps: f32,
    adverse_vol_30s: Option<f32>,
) -> MarketRegimeCluster {
    let observed_range = market_yes_range_so_far.max(0.0);
    let path_efficiency = regime_path_efficiency.max(0.0);
    let reversal = regime_reversal_pressure.max(0.0);
    let sign_flip = regime_sign_flip_rate.max(0.0);
    let realized_vol = regime_realized_vol_180s_bps.max(0.0);
    let adverse_vol = adverse_vol_30s.unwrap_or(0.0).max(0.0);

    if observed_range >= 0.20 && sign_flip >= 0.50 {
        return MarketRegimeCluster::ExpandedHighFlip;
    }
    if observed_range >= 0.20 && reversal >= 0.30 {
        return MarketRegimeCluster::ExpandedReversalPressure;
    }
    if (2.0..=8.0).contains(&realized_vol) && (1.0..=4.0).contains(&adverse_vol) {
        return MarketRegimeCluster::FlowAdverseVolCluster;
    }
    if path_efficiency < 0.10 && reversal < 0.30 {
        return MarketRegimeCluster::LowEfficiencyNonreversal;
    }
    if path_efficiency >= 0.35 && sign_flip < 0.50 && reversal < 0.30 {
        return MarketRegimeCluster::CleanDirectionalPath;
    }
    if realized_vol < 1.0 && adverse_vol < 1.0 && reversal < 0.30 {
        return MarketRegimeCluster::CalmLowVol;
    }
    if observed_range < 0.10 {
        return MarketRegimeCluster::EarlyTightRange;
    }
    MarketRegimeCluster::MixedNeutral
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::SpotTick;

    fn h(samples: Vec<(i64, f64)>) -> SpotHistory {
        SpotHistory::new(
            samples
                .into_iter()
                .map(|(ts_ns, price)| SpotTick {
                    ts_ns,
                    price,
                    quantity: 0.0,
                    is_buyer_maker: false,
                })
                .collect(),
        )
    }

    #[test]
    fn whipsaw_risk_scores_zigzag_higher_than_smooth_trend() {
        let ns = |secs: i64| secs * 1_000_000_000;
        let mut zigzag = Vec::new();
        for i in 0..=36i64 {
            let swing = if i % 2 == 0 { 10.0 } else { -10.0 };
            zigzag.push((ns(i * 5), 80_000.0 + swing));
        }
        let mut smooth = Vec::new();
        for i in 0..=36i64 {
            smooth.push((ns(i * 5), 80_000.0 + (i as f64 * 0.7)));
        }

        let zigzag_snap = WhipsawRiskSnapshot::from_history(ns(180), &h(zigzag));
        let smooth_snap = WhipsawRiskSnapshot::from_history(ns(180), &h(smooth));

        assert!(
            zigzag_snap.score > smooth_snap.score + 0.10,
            "zigzag={:?} smooth={:?}",
            zigzag_snap,
            smooth_snap
        );
        assert!(zigzag_snap.sign_flip_rate > 0.80);
        assert!(smooth_snap.path_efficiency > 0.95);
    }

    #[test]
    fn market_cluster_matches_report_threshold_ordering() {
        assert_eq!(
            classify_market_regime_cluster(0.25, 0.20, 0.60, 0.55, 3.0, Some(2.0)),
            MarketRegimeCluster::ExpandedHighFlip
        );
        assert_eq!(
            classify_market_regime_cluster(0.25, 0.20, 0.35, 0.20, 3.0, Some(2.0)),
            MarketRegimeCluster::ExpandedReversalPressure
        );
        assert_eq!(
            classify_market_regime_cluster(0.12, 0.20, 0.10, 0.20, 3.0, Some(2.0)),
            MarketRegimeCluster::FlowAdverseVolCluster
        );
        assert_eq!(
            classify_market_regime_cluster(0.12, 0.05, 0.10, 0.20, 0.8, Some(0.2)),
            MarketRegimeCluster::LowEfficiencyNonreversal
        );
        assert_eq!(
            classify_market_regime_cluster(0.12, 0.50, 0.10, 0.20, 1.5, Some(0.2)),
            MarketRegimeCluster::CleanDirectionalPath
        );
        assert_eq!(
            classify_market_regime_cluster(0.12, 0.20, 0.10, 0.20, 0.8, Some(0.2)),
            MarketRegimeCluster::CalmLowVol
        );
        assert_eq!(
            classify_market_regime_cluster(0.05, 0.20, 0.40, 0.20, 1.5, Some(0.2)),
            MarketRegimeCluster::EarlyTightRange
        );
    }
}
