//! Truthful-latency floor: backtests refuse to run below a modeled taker
//! latency unless an explicit `--fantasy` grant is given. Fantasy runs are
//! watermarked so they can never be mistaken for a truthful backtest.

use anyhow::{Result, anyhow};

/// Minimum modeled taker latency (ms) for a run to count as truthful. Below
/// this, the engine is assuming execution faster than is realistically
/// achievable, so the run is rejected unless `--fantasy` opts in.
pub const TRUTHFUL_LATENCY_FLOOR_MS: u64 = 750;

/// Validate the modeled taker latency against the truthful floor.
///
/// Returns `Ok(None)` for truthful runs (latency at or above the floor without
/// a fantasy grant). Returns `Ok(Some("FANTASY"))` when fantasy is explicitly
/// granted (the run proceeds but is watermarked). Returns an `Err` mentioning
/// `--fantasy` when the latency is sub-floor and no grant was given.
pub fn validate_latency(taker_latency_ms: u64, fantasy: bool) -> Result<Option<String>> {
    if fantasy {
        return Ok(Some("FANTASY".to_string()));
    }
    if taker_latency_ms >= TRUTHFUL_LATENCY_FLOOR_MS {
        return Ok(None);
    }
    Err(anyhow!(
        "taker latency {taker_latency_ms}ms is below the truthful floor of \
         {TRUTHFUL_LATENCY_FLOOR_MS}ms; realistic execution cannot be modeled that fast. \
         Pass --fantasy to override and watermark the run as fantasy."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latency_below_floor_without_fantasy_is_rejected() {
        let err = validate_latency(500, false).unwrap_err();
        assert!(err.to_string().contains("--fantasy"));
    }

    #[test]
    fn latency_below_floor_with_fantasy_is_watermarked() {
        assert_eq!(validate_latency(500, true).unwrap().as_deref(), Some("FANTASY"));
    }

    #[test]
    fn truthful_latency_needs_no_flag_and_no_watermark() {
        assert_eq!(validate_latency(1250, false).unwrap(), None);
        assert_eq!(validate_latency(750, false).unwrap(), None);
    }

    #[test]
    fn fantasy_above_floor_is_still_watermarked() {
        // An explicit --fantasy grant watermarks the run regardless of latency.
        assert_eq!(validate_latency(1250, true).unwrap().as_deref(), Some("FANTASY"));
    }
}
