//! Multi-timeframe spot momentum.
//!
//! Ported with light modifications from `polymarket-exec/src/signals/momentum.rs`.
//! Drives the `direction_score` component of the 4-score model.
//!
//! Inputs are `SpotHistory` samples (ns timestamps + USD price). Outputs a
//! signed score in `[-1, 1]` plus per-window returns in basis points and an
//! acceleration term (latest minus prior window).

use pm_types::SpotHistory;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignalDirection {
    Up,
    Down,
    #[default]
    Neutral,
}

impl SignalDirection {
    pub fn from_signed(value: f64, deadband: f64) -> Self {
        if !value.is_finite() || value.abs() <= deadband {
            Self::Neutral
        } else if value > 0.0 {
            Self::Up
        } else {
            Self::Down
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct MomentumConfig {
    pub lookback_windows: usize,
    pub window_ns: i64,
    pub decay_factor: f64,
    pub directional_deadband: f64,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            lookback_windows: 5,
            window_ns: 30 * 1_000_000_000, // 30 seconds
            decay_factor: 0.6,
            directional_deadband: 0.10,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MomentumSignal {
    pub direction: SignalDirection,
    /// Signed weighted score in roughly `[-1, 1]` (sum of decayed signs).
    pub score: f64,
    /// `|score|` clipped to `[0, 1]`.
    pub strength: f64,
    pub latest_window_return_bps: Option<f64>,
    pub acceleration_bps: Option<f64>,
    pub window_returns_bps: Vec<f64>,
}

impl MomentumSignal {
    pub fn is_ready(&self) -> bool {
        !self.window_returns_bps.is_empty()
    }
}

/// Compute multi-timeframe momentum over the trailing windows ending at `now_ns`.
pub fn compute_momentum(now_ns: i64, spot: &SpotHistory, cfg: &MomentumConfig) -> MomentumSignal {
    if cfg.window_ns <= 0 || cfg.lookback_windows == 0 || spot.is_empty() {
        return MomentumSignal::default();
    }
    let mut returns = Vec::with_capacity(cfg.lookback_windows);
    for window_idx in 0..cfg.lookback_windows {
        let end_ns = now_ns - (window_idx as i64) * cfg.window_ns;
        let start_ns = end_ns - cfg.window_ns;
        let Some(start_price) = spot.price_at_or_after(start_ns) else {
            break;
        };
        let Some(end_price) = spot.price_at_or_before(end_ns) else {
            break;
        };
        if start_price <= 0.0 || end_price <= 0.0 {
            break;
        }
        returns.push(((end_price / start_price) - 1.0) * 10_000.0);
    }
    if returns.is_empty() {
        return MomentumSignal::default();
    }
    let mut weighted = 0.0f64;
    for (i, ret) in returns.iter().enumerate() {
        let w = cfg.decay_factor.clamp(0.0, 1.0).powi(i as i32);
        weighted += ret.signum() * w;
    }
    let latest = returns.first().copied();
    let accel = returns.first().zip(returns.get(1)).map(|(a, b)| a - b);
    MomentumSignal {
        direction: SignalDirection::from_signed(weighted, cfg.directional_deadband),
        score: weighted,
        strength: weighted.abs().clamp(0.0, 1.0),
        latest_window_return_bps: latest,
        acceleration_bps: accel,
        window_returns_bps: returns,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SpotMomentumStack {
    pub fast_return: Option<f64>,
    pub broad_return: Option<f64>,
    pub fast_score: f32,
    pub broad_score: f32,
    pub blended_score: f32,
    pub fast_broad_alignment: f32,
    pub acceleration_score: f32,
}

const FAST_MOMENTUM_WINDOWS: [(i64, f64); 5] =
    [(10, 0.45), (30, 0.25), (60, 0.15), (120, 0.10), (300, 0.05)];
const BROAD_MOMENTUM_WINDOWS: [(i64, f64); 4] =
    [(600, 0.35), (900, 0.25), (1800, 0.25), (3600, 0.15)];
const FAST_SCORE_SCALE: f64 = 300.0;
const BROAD_SCORE_SCALE: f64 = 90.0;

fn weighted_return(now_ns: i64, spot: &SpotHistory, windows: &[(i64, f64)]) -> Option<f64> {
    let mut sum = 0.0f64;
    let mut wsum = 0.0f64;
    for &(secs, weight) in windows {
        if let Some(r) = spot.simple_return(now_ns, secs * 1_000_000_000) {
            sum += weight * r;
            wsum += weight;
        }
    }
    (wsum > 0.0).then_some(sum / wsum)
}

fn score_return(r: Option<f64>, scale: f64) -> f32 {
    r.map(|value| (value * scale).clamp(-1.0, 1.0) as f32)
        .unwrap_or(0.0)
}

/// Weighted multi-timeframe return for entry timing. Returns are raw fractional
/// returns (e.g. 0.001 = 10 bps).
pub fn weighted_multi_tf_return(now_ns: i64, spot: &SpotHistory) -> Option<f64> {
    weighted_return(now_ns, spot, &FAST_MOMENTUM_WINDOWS)
}

/// Larger-context BTC momentum. Intended for regime bias and anti-fighting
/// filters rather than tick-level entry timing.
pub fn weighted_broad_multi_tf_return(now_ns: i64, spot: &SpotHistory) -> Option<f64> {
    weighted_return(now_ns, spot, &BROAD_MOMENTUM_WINDOWS)
}

pub fn spot_momentum_stack(now_ns: i64, spot: &SpotHistory) -> SpotMomentumStack {
    let fast_return = weighted_multi_tf_return(now_ns, spot);
    let broad_return = weighted_broad_multi_tf_return(now_ns, spot);
    let fast_score = score_return(fast_return, FAST_SCORE_SCALE);
    let broad_score = score_return(broad_return, BROAD_SCORE_SCALE);
    let aligned = if fast_score.abs() < 0.02 || broad_score.abs() < 0.02 {
        0.0
    } else if fast_score.signum() == broad_score.signum() {
        1.0
    } else {
        -1.0
    };
    let acceleration_score = (fast_score - broad_score).clamp(-1.0, 1.0);
    let blended_score = if aligned < 0.0 {
        // A short impulse fighting the broader trend is more fragile.
        (0.65 * fast_score + 0.35 * broad_score).clamp(-1.0, 1.0)
    } else {
        (0.80 * fast_score + 0.20 * broad_score).clamp(-1.0, 1.0)
    };
    SpotMomentumStack {
        fast_return,
        broad_return,
        fast_score,
        broad_score,
        blended_score,
        fast_broad_alignment: aligned,
        acceleration_score,
    }
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
    fn downtrend_produces_down_signal() {
        let ns = |secs: i64| secs * 1_000_000_000;
        let spot = h(vec![
            (ns(0), 100.0),
            (ns(30), 99.0),
            (ns(60), 98.0),
            (ns(90), 97.0),
        ]);
        let sig = compute_momentum(
            ns(90),
            &spot,
            &MomentumConfig {
                lookback_windows: 3,
                window_ns: ns(30),
                decay_factor: 0.6,
                directional_deadband: 0.05,
            },
        );
        assert_eq!(sig.direction, SignalDirection::Down);
        assert!(sig.score < -0.5, "score {}", sig.score);
    }

    #[test]
    fn weighted_multi_tf_uptrend_positive() {
        let ns = |secs: i64| secs * 1_000_000_000;
        // 1% rise over 5 min, mostly in the last 60s.
        let spot = h(vec![
            (ns(0), 80_000.0),
            (ns(60), 80_100.0),
            (ns(120), 80_200.0),
            (ns(240), 80_300.0),
            (ns(290), 80_500.0),
            (ns(300), 80_800.0),
        ]);
        let r = weighted_multi_tf_return(ns(300), &spot).unwrap();
        assert!(r > 0.0, "got {r}");
    }

    #[test]
    fn broad_momentum_sees_larger_trend() {
        let ns = |secs: i64| secs * 1_000_000_000;
        let spot = h(vec![
            (ns(0), 80_000.0),
            (ns(600), 80_400.0),
            (ns(900), 80_700.0),
            (ns(1800), 81_200.0),
            (ns(3600), 82_000.0),
        ]);
        let r = weighted_broad_multi_tf_return(ns(3600), &spot).unwrap();
        assert!(r > 0.0, "got {r}");
        let stack = spot_momentum_stack(ns(3600), &spot);
        assert!(stack.broad_score > 0.0);
    }
}
