pub fn proportional_stake(
    leader_usdc: f64,
    leader_equity: f64,
    our_equity: f64,
    max_clip_usdc: f64,
) -> f64 {
    if leader_equity <= 0.0 || our_equity <= 0.0 { return 0.0; }
    let fraction = (leader_usdc / leader_equity).clamp(0.0, 1.0);
    (fraction * our_equity).min(max_clip_usdc).min(our_equity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mirrors_fraction_of_our_equity() {
        assert!((proportional_stake(50.0, 1000.0, 100.0, 1000.0) - 5.0).abs() < 1e-9);
    }
    #[test]
    fn clamped_by_max_clip() {
        assert!((proportional_stake(500.0, 1000.0, 100.0, 5.0) - 5.0).abs() < 1e-9);
    }
    #[test]
    fn zero_when_equity_nonpositive() {
        assert_eq!(proportional_stake(50.0, 0.0, 100.0, 10.0), 0.0);
        assert_eq!(proportional_stake(50.0, 1000.0, 0.0, 10.0), 0.0);
    }
}
