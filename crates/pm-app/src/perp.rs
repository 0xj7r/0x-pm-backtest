//! Perp-complex loaders, moved to `pm_backtest::portfolio` (extraction map
//! tranche 4, companion move 5). Re-exported here so `crate::perp::...` call
//! sites elsewhere in pm-app keep working.

pub use pm_backtest::portfolio::load_perp_state;
