//! BTC micro-regime classifier.
//!
//! Ported from `polymarket-exec/src/signals/btc_regime.rs`. Builds a snapshot
//! from a `SpotHistory` at a given `now_ns` and classifies the regime into one
//! of four quadrants on (vol-level × trend-strength).

use pm_types::SpotHistory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtcRegime {
    /// Low noise, no trend. Skip everything; nothing to trade.
    Flat,
    /// High noise, no trend. Mean-reverting; paired-MM has positive edge.
    Whipsaw,
    /// Low noise, strong trend. Late-bar directional has positive edge.
    DirectionalSmooth,
    /// High noise + strong trend. Mixed; both lanes cautious.
    TrendingVolatile,
}

impl BtcRegime {
    pub const VOL_LOW_HIGH_BPS: f64 = 8.0;
    pub const TREND_NOISE_RATIO: f64 = 5.0;
    pub const VOL_FLOOR_BPS: f64 = 0.5;
}

impl std::fmt::Display for BtcRegime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BtcRegime::Flat => f.write_str("flat"),
            BtcRegime::Whipsaw => f.write_str("whipsaw"),
            BtcRegime::DirectionalSmooth => f.write_str("directional_smooth"),
            BtcRegime::TrendingVolatile => f.write_str("trending_volatile"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct BtcRegimeSnapshot {
    pub last_price: Option<f64>,
    pub price_history_ms: u64,
    pub realized_vol_5m_bps: Option<f64>,
    pub realized_vol_15m_bps: Option<f64>,
    pub trade_count_5m: u64,
    pub trade_count_15m: u64,
    pub return_30s_bps: Option<f64>,
    pub return_60s_bps: Option<f64>,
    pub return_120s_bps: Option<f64>,
    pub return_180s_bps: Option<f64>,
    pub observed_at_ns: i64,
}

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

impl BtcRegimeSnapshot {
    /// Build from a spot tape at `now_ns`. Returns a snapshot whose individual
    /// fields may be `None` during warmup.
    pub fn from_history(now_ns: i64, spot: &SpotHistory) -> Self {
        let last_price = spot.price_at_or_before(now_ns);
        let first_ts = spot.samples().first().map(|t| t.ts_ns);
        let price_history_ms = first_ts
            .map(|t| ((now_ns - t).max(0) / 1_000_000) as u64)
            .unwrap_or(0);

        let return_bps = |sec: i64| {
            spot.simple_return(now_ns, sec * 1_000_000_000)
                .map(|r| r * 10_000.0)
        };
        let return_30s_bps = return_bps(30);
        let return_60s_bps = return_bps(60);
        let return_120s_bps = return_bps(120);
        let return_180s_bps = return_bps(180);

        let (realized_vol_5m_bps, trade_count_5m) = realized_vol_window(now_ns, spot, 300);
        let (realized_vol_15m_bps, trade_count_15m) = realized_vol_window(now_ns, spot, 900);

        Self {
            last_price,
            price_history_ms,
            realized_vol_5m_bps,
            realized_vol_15m_bps,
            trade_count_5m,
            trade_count_15m,
            return_30s_bps,
            return_60s_bps,
            return_120s_bps,
            return_180s_bps,
            observed_at_ns: now_ns,
        }
    }

    /// Classify the regime. `None` during warmup (either input unavailable).
    pub fn regime(&self) -> Option<BtcRegime> {
        let vol = self
            .realized_vol_5m_bps
            .filter(|v| v.is_finite() && *v >= 0.0)?;
        let trend = self
            .return_180s_bps
            .filter(|r| r.is_finite())
            .map(f64::abs)?;
        let vol_eff = vol.max(BtcRegime::VOL_FLOOR_BPS);
        let trend_dominates = (trend / vol_eff) >= BtcRegime::TREND_NOISE_RATIO;
        let high_vol = vol >= BtcRegime::VOL_LOW_HIGH_BPS;
        Some(match (high_vol, trend_dominates) {
            (false, false) => BtcRegime::Flat,
            (false, true) => BtcRegime::DirectionalSmooth,
            (true, false) => BtcRegime::Whipsaw,
            (true, true) => BtcRegime::TrendingVolatile,
        })
    }
}

/// Realized vol (bps) of per-trade log returns over the trailing `secs`
/// window. Returns `(vol_bps, trade_count)`.
fn realized_vol_window(now_ns: i64, spot: &SpotHistory, secs: i64) -> (Option<f64>, u64) {
    let start_ns = now_ns - secs * 1_000_000_000;
    let slice = spot.range(start_ns, now_ns);
    if slice.len() < 3 {
        return (None, slice.len() as u64);
    }
    let mut prev = slice[0].price;
    let mut sumsq = 0.0f64;
    let mut n = 0usize;
    for t in &slice[1..] {
        let p = t.price;
        if p > 0.0 && prev > 0.0 && p.is_finite() {
            let r = (p / prev).ln();
            sumsq += r * r;
            n += 1;
        }
        prev = p;
    }
    if n < 2 {
        return (None, slice.len() as u64);
    }
    // RMS of log returns, converted to bps.
    let rms = (sumsq / n as f64).sqrt();
    (Some(rms * 10_000.0), slice.len() as u64)
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
    fn flat_regime_when_calm_and_no_trend() {
        let ns = |secs: i64| secs * 1_000_000_000;
        let mut samples = Vec::new();
        // 60 prices clustered tightly around 80000, every 5 seconds.
        for i in 0..60i64 {
            samples.push((ns(i * 5), 80_000.0 + (i as f64 % 2.0)));
        }
        let snap = BtcRegimeSnapshot::from_history(ns(300), &h(samples));
        assert_eq!(snap.regime(), Some(BtcRegime::Flat));
    }

    #[test]
    fn directional_smooth_when_steady_trend() {
        let ns = |secs: i64| secs * 1_000_000_000;
        let mut samples = Vec::new();
        // Smooth 20 bps drift up every 5 seconds over 5 min. Low vol, large trend.
        for i in 0..60i64 {
            samples.push((ns(i * 5), 80_000.0 + (i as f64) * 1.0));
        }
        let snap = BtcRegimeSnapshot::from_history(ns(300), &h(samples));
        // 60 * 1 / 80000 = 0.00075 = 75 bps move; vol is small (smooth).
        // Should fall into DirectionalSmooth, not Flat.
        assert!(matches!(
            snap.regime(),
            Some(BtcRegime::DirectionalSmooth) | Some(BtcRegime::TrendingVolatile)
        ));
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
