use crate::event::Token;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExposureKey {
    pub token: Token,
    /// Groups markets whose open windows overlap. v1: the driver supplies a
    /// bucket id (e.g. the shared close-window). Finer bucketing is a config refinement.
    pub window: i64,
}

#[derive(Debug, Default)]
pub struct ExposureState {
    net_shares: HashMap<ExposureKey, f64>,
}

impl ExposureState {
    /// Signed share delta applied on each fill (+ for net-long YES, - for net-short).
    pub fn apply(&mut self, key: ExposureKey, signed_shares_delta: f64) {
        *self.net_shares.entry(key).or_default() += signed_shares_delta;
    }

    pub fn net(&self, key: ExposureKey) -> f64 {
        self.net_shares.get(&key).copied().unwrap_or(0.0)
    }

    /// Would adding `signed_shares_delta` to `key` exceed `cap_abs_shares`?
    pub fn would_exceed(&self, key: ExposureKey, signed_shares_delta: f64, cap_abs_shares: f64) -> bool {
        (self.net(key) + signed_shares_delta).abs() > cap_abs_shares
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ExposureKey {
        ExposureKey { token: Token::Btc, window: 0 }
    }

    #[test]
    fn net_accumulates_across_markets() {
        let mut e = ExposureState::default();
        e.apply(key(), 100.0);
        e.apply(key(), 50.0);
        assert_eq!(e.net(key()), 150.0);
    }

    #[test]
    fn cap_blocks_when_aggregate_would_exceed() {
        let mut e = ExposureState::default();
        e.apply(key(), 900.0);
        assert!(e.would_exceed(key(), 200.0, 1000.0)); // 900 + 200 > 1000
        assert!(!e.would_exceed(key(), 50.0, 1000.0)); // 900 + 50 <= 1000
    }

    #[test]
    fn opposite_sign_reduces_exposure() {
        let mut e = ExposureState::default();
        e.apply(key(), 900.0);
        assert!(!e.would_exceed(key(), -800.0, 1000.0)); // |900-800| = 100
    }
}
