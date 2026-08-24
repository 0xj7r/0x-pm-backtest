//! Backtest runner: fill/engine core, moved to the `pm-backtest` crate.
//!
//! Re-exported here so existing `crate::runner::...` paths in this crate
//! keep working; see `docs/superpowers/plans/2026-08-23-phase2-extraction-map.md`
//! (tranche 1).

pub use pm_backtest::accounting::pretty_print;
pub use pm_backtest::config::RunnerConfig;
pub use pm_backtest::engine::run_backtest;
pub use pm_backtest::fills::Fill;
