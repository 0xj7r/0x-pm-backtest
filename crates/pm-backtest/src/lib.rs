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
pub mod fingerprint;
pub mod jitter;
pub mod portfolio;
pub mod scorecard;
pub mod validate;
