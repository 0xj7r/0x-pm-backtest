//! Backtest engine: fill/order matching, accounting, run configuration, and
//! walk-forward orchestration.
//!
//! Tranches 1-2 of the pm-app `runner.rs`/`walkforward.rs`/`result_summary.rs`
//! carve-out (see `docs/superpowers/plans/2026-08-23-phase2-extraction-map.md`).
//! This crate holds the `runner.rs` fill/engine core plus `walkforward.rs`'s
//! orchestration, portfolio, and scorecard modules; `result_summary.rs`
//! lands here in a later task.

pub mod accounting;
pub mod config;
pub mod engine;
pub mod fills;
/// Config fingerprinting, re-exported from pm-alpha.
///
/// The implementation lives in `pm_alpha::fingerprint` because pm-shadow needs
/// the same function and depends on pm-alpha but deliberately not on
/// pm-backtest. This module keeps `pm_backtest::fingerprint::config_fingerprint`
/// working for the engine and scorecard call sites.
pub mod fingerprint {
    pub use pm_alpha::fingerprint::config_fingerprint;
}
pub mod jitter;
pub mod portfolio;
pub mod scorecard;
pub mod settlement;
pub mod validate;
