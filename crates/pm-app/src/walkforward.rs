//! Walk-forward orchestration, moved to the `pm-backtest` crate.
//!
//! Re-exported here so existing `crate::walkforward::...` paths in this
//! crate keep working; see
//! `docs/superpowers/plans/2026-08-23-phase2-extraction-map.md` (tranche 2).

pub use pm_backtest::accounting::{
    market_close_ns, market_duration_secs_from_slug, market_open_ns, outcome_label_resolved_yes,
    validate_outcome_labels,
};
pub use pm_backtest::config::WalkForwardConfig;
pub use pm_backtest::engine::{StratId, load_replay_events_for_market, run_walkforward};
pub use pm_backtest::portfolio::SpotCache;
pub use pm_backtest::scorecard::{
    print_summary, write_market_results_jsonl_atomic, write_summary_json_atomic,
};
