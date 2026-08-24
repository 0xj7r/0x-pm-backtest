//! Backtest engine: fill/order matching, accounting, and run configuration.
//!
//! Tranche 1 of the pm-app `runner.rs`/`walkforward.rs`/`result_summary.rs`
//! carve-out (see `docs/superpowers/plans/2026-08-23-phase2-extraction-map.md`).
//! This crate currently holds only the `runner.rs` fill/engine core; the
//! walk-forward orchestration and result-summary modules land here in later
//! tasks.

pub mod accounting;
pub mod config;
pub mod engine;
pub mod fills;
