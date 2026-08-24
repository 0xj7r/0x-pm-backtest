//! Deterministic jittered-latency replay: run the walk-forward N times at
//! seeded, perturbed taker latencies and report the P&L spread (p10/p50/p90)
//! instead of a single point estimate.
//!
//! All determinism flows from an explicit seed. The PRNG is an inlined
//! splitmix64 (no new dependencies); no `Date::now()` or other entropy source
//! touches this path. The N runs are serial (they share the spot/perp caches
//! built inside `run_walkforward`), so a 288-market day at N=5 is roughly 5x
//! the single-run wall time.

use serde::Serialize;

use crate::config::WalkForwardConfig;
use crate::validate::TRUTHFUL_LATENCY_FLOOR_MS;

/// Per-run jitter report attached to [`crate::scorecard::WalkForwardSummary`]
/// when `--jitter N` (N > 0) was requested. `None` (and omitted from JSON via
/// `skip_serializing_if`) for ordinary single-run backtests.
#[derive(Debug, Clone, Serialize)]
pub struct JitterReport {
    pub latencies_ms: Vec<u64>,
    pub net_pnls: Vec<f64>,
    pub p10: f64,
    pub p50: f64,
    pub p90: f64,
}

/// Inline splitmix64 PRNG (Vigna; public-domain). Advances `state` and returns
/// the next 64-bit output. Chosen because it is a single multiply-xorshift
/// mix with no state beyond a 64-bit counter, so it needs no new crate.
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// Deterministic per-run latencies: `n` uniform integers in
/// `[base - spread, base + spread]`, seeded by `seed`. Same seed yields the
/// same vec; outputs stay within the inclusive bounds.
pub fn jitter_latencies(base_ms: u64, spread_ms: u64, n: usize, seed: u64) -> Vec<u64> {
    let lo = base_ms.saturating_sub(spread_ms);
    let hi = base_ms.saturating_add(spread_ms);
    let span = hi - lo; // inclusive span: hi - lo possible values above lo
    let mut rng = SplitMix64::new(seed);
    (0..n)
        .map(|_| if span == 0 { lo } else { lo + (rng.next_u64() % (span + 1)) })
        .collect()
}

/// Resolve the actual per-run latencies for a jitter config: jitter around
/// `cfg.taker_latency_ms` by `cfg.jitter_latency_spread_ms`, then clamp each
/// up to the truthful floor ([`TRUTHFUL_LATENCY_FLOOR_MS`]) unless `--fantasy`
/// is granted. Returns an empty vec when `cfg.jitter == 0`.
pub fn jittered_run_latencies(cfg: &WalkForwardConfig) -> Vec<u64> {
    let raw = jitter_latencies(
        cfg.taker_latency_ms,
        cfg.jitter_latency_spread_ms,
        cfg.jitter,
        cfg.jitter_seed,
    );
    raw.into_iter()
        .map(|lat| if cfg.fantasy { lat } else { lat.max(TRUTHFUL_LATENCY_FLOOR_MS) })
        .collect()
}

/// Linear-interpolation percentile of a slice (numpy "linear" method):
/// `rank = q * (n - 1)`, interpolated between the two adjacent order
/// statistics. Returns `0.0` for an empty slice.
fn percentile(sorted: &[f64], q: f64) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        return sorted[0];
    }
    let rank = q * (n - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    let frac = rank - lo as f64;
    sorted[lo] + frac * (sorted[hi] - sorted[lo])
}

/// p10 / p50 / p90 of `values` via linear interpolation. Sorts a copy; the
/// input order is not preserved.
pub fn percentiles(values: &[f64]) -> (f64, f64, f64) {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    (percentile(&sorted, 0.10), percentile(&sorted, 0.50), percentile(&sorted, 0.90))
}

/// Build the jitter report from the per-run net P&Ls and the latencies that
/// produced them. Returns `None` when no jitter was requested
/// (`cfg.jitter == 0`) or no runs were collected, so a non-jitter run leaves
/// `summary.jitter` at `None` and the field is omitted from the summary JSON.
pub fn build_jitter_report(
    cfg: &WalkForwardConfig,
    latencies_ms: Vec<u64>,
    net_pnls: Vec<f64>,
) -> Option<JitterReport> {
    if cfg.jitter == 0 || net_pnls.is_empty() {
        return None;
    }
    let (p10, p50, p90) = percentiles(&net_pnls);
    Some(JitterReport {
        latencies_ms,
        net_pnls,
        p10,
        p50,
        p90,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_latencies_deterministic_and_bounded() {
        let base = 1250u64;
        let spread = 250u64;
        let n = 8usize;
        let seed = 42u64;

        let a = jitter_latencies(base, spread, n, seed);
        let b = jitter_latencies(base, spread, n, seed);

        assert_eq!(a, b, "same seed must produce the same vec");
        assert_eq!(a.len(), n, "length must be n");
        let lo = base - spread;
        let hi = base + spread;
        for v in &a {
            assert!(*v >= lo && *v <= hi, "{v} outside [{lo}, {hi}]");
        }
        // A different seed should (with overwhelming probability) differ.
        let c = jitter_latencies(base, spread, n, seed.wrapping_add(1));
        assert_ne!(a, c);

        // Zero spread collapses to the base for every draw.
        let flat = jitter_latencies(base, 0, n, seed);
        assert!(flat.iter().all(|&v| v == base));

        // n = 0 yields an empty vec.
        assert!(jitter_latencies(base, spread, 0, seed).is_empty());
    }

    #[test]
    fn jitter_zero_attaches_nothing() {
        // cfg.jitter == 0 means no jitter was requested: build_jitter_report
        // returns None so summary.jitter stays None and is omitted from JSON.
        let cfg = WalkForwardConfig {
            jitter: 0,
            jitter_latency_spread_ms: 250,
            jitter_seed: 42,
            ..WalkForwardConfig::default()
        };
        // Even if a caller passed collected P&Ls, the zero-request path drops them.
        assert!(build_jitter_report(&cfg, vec![1250, 1100, 1400], vec![10.0, -5.0, 20.0]).is_none());
    }

    #[test]
    fn percentiles_correct_on_known_vector() {
        // 5-element vector [10, 20, 30, 40, 50]; linear interpolation.
        // p10: rank 0.4 -> 10 + 0.4*(20-10) = 14
        // p50: rank 2.0 -> 30
        // p90: rank 3.6 -> 40 + 0.6*(50-40) = 46
        let (p10, p50, p90) = percentiles(&[10.0, 20.0, 30.0, 40.0, 50.0]);
        assert!((p10 - 14.0).abs() < 1e-9, "p10 {p10}");
        assert!((p50 - 30.0).abs() < 1e-9, "p50 {p50}");
        assert!((p90 - 46.0).abs() < 1e-9, "p90 {p90}");

        // Empty input is safe and yields zeros.
        let (z1, z2, z3) = percentiles(&[]);
        assert_eq!((z1, z2, z3), (0.0, 0.0, 0.0));

        // Single element repeats at every percentile.
        let (s1, s2, s3) = percentiles(&[7.5]);
        assert_eq!((s1, s2, s3), (7.5, 7.5, 7.5));

        // Order-independence: shuffled input gives the same answer.
        let (p10u, p50u, p90u) = percentiles(&[40.0, 10.0, 50.0, 20.0, 30.0]);
        assert!((p10u - 14.0).abs() < 1e-9);
        assert!((p50u - 30.0).abs() < 1e-9);
        assert!((p90u - 46.0).abs() < 1e-9);
    }

    #[test]
    fn jittered_run_latencies_clamps_to_floor_unless_fantasy() {
        // Base below the floor, no fantasy grant: every draw is clamped up to 750.
        let cfg = WalkForwardConfig {
            taker_latency_ms: 600,
            jitter_latency_spread_ms: 100,
            jitter: 5,
            jitter_seed: 7,
            fantasy: false,
            ..WalkForwardConfig::default()
        };
        let lats = jittered_run_latencies(&cfg);
        assert_eq!(lats.len(), 5);
        assert!(lats.iter().all(|&l| l >= TRUTHFUL_LATENCY_FLOOR_MS), "lats {lats:?}");

        // Same base with a fantasy grant: draws stay in the raw sub-floor band.
        let fcfg = WalkForwardConfig {
            taker_latency_ms: 600,
            jitter_latency_spread_ms: 100,
            jitter: 5,
            jitter_seed: 7,
            fantasy: true,
            ..WalkForwardConfig::default()
        };
        let flat = jittered_run_latencies(&fcfg);
        assert!(flat.iter().all(|&l| l >= 500 && l <= 700), "flat {flat:?}");
    }
}
