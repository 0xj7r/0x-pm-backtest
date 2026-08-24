# Phase 2 extraction map: pm-backtest carve-out

Authority for Tasks 3, 4, 5 of `docs/superpowers/plans/2026-08-23-reset-phase2-engine.md`.
Every top-level item in `crates/pm-app/src/{walkforward.rs,runner.rs,result_summary.rs}`
is classified into a destination (`pm-backtest::{engine,fills,accounting,portfolio,scorecard,config}`,
stays in pm-app, or delete) and a tranche (3, 4, or 5, matching the plan's task numbers).

Inventory grep (broader than the brief's; the brief's literal pattern misses
`pub async fn` / `pub(crate) fn` / bare `async fn`, undercounting
`walkforward.rs` by 14 items including `run_walkforward` itself):

```
grep -nE "^(pub(\(crate\))? )?(async )?fn |^(pub(\(crate\))? )?struct |^(pub(\(crate\))? )?enum |^impl(<[^>]*>)? |^(pub(\(crate\))? )?const |^(pub(\(crate\))? )?mod |^(pub(\(crate\))? )?trait |^(pub(\(crate\))? )?type |^(pub(\(crate\))? )?static " <file>
```

Counts: `walkforward.rs` 107 real items + `mod tests`; `runner.rs` 35 + `mod tests`;
`result_summary.rs` 16 + `mod tests`. `mod tests` blocks move verbatim with the
code they test (not enumerated as separate rows) per the "no logic edits" rule.

## Step 2: seams

**(a) Where pm_model is evaluated, and the pluggability boundary.**
Inline in `run_backtest`'s per-event loop, `runner.rs:730-747` (matches the
brief's line numbers almost exactly; current HEAD has only drifted a few
lines from whatever state the brief was written against):

```rust
let canonical_model_eval = if let Some(shared) = &cfg.shared_model_state {
    let mut state = shared.lock().expect("shared model mutex poisoned");
    state.evaluate_detailed_with_market_context(event, spot, secs_since_open as f32, &model_cfg, cfg.model_market_context)
} else {
    model_state.evaluate_detailed_with_market_context(event, spot, secs_since_open as f32, &model_cfg, cfg.model_market_context)
};
```

Result feeds `Ctx.model_output` / `Ctx.model_attribution` at `runner.rs:766-794`
(the `Ctx` construction the brief points at, `~780-795`) before
`strategy.on_event_scored(event, &ctx, ...)` is called at `runner.rs:795`.
`run_backtest` calls `pm_model::ModelState::evaluate_detailed_with_market_context`
directly (hard dependency, not through a trait), and also calls
`pm_model::side_edge_vs_mid` at the model-gate check (`runner.rs:819`).

Pluggability boundary for later (Task 12 Step 3, not this task): replace the
`if let Some(shared) = &cfg.shared_model_state { ... } else { model_state... }`
block with a call through a trait, e.g.
`cfg.model_evaluator.evaluate(event, spot, secs_since_open, market_context) -> ModelEvalDetailed`,
implemented by a `PmModelEvaluator` adapter that wraps today's `ModelState`
calls. `run_backtest` (pm-backtest::engine) would then depend on the trait
only; pm-app or a small adapter crate supplies the pm_model-backed impl. Not
changed in this task; the map just records where the seam goes.

**(b) What runner.rs and walkforward.rs share.**
One-directional: `walkforward.rs` imports `crate::runner::{RunnerConfig, run_backtest}`
and uses `crate::runner::Fill` inline (5 call sites: `walkforward.rs:343,1188,1295,4293-4294,4341`
plus the two accounting helpers `fill_resolution_pnl`/`buy_fill_won` at
`3747,3769`). `runner.rs` has zero references to anything in `walkforward.rs`
(`grep -n "walkforward" crates/pm-app/src/runner.rs` returns nothing). This
one-directional dependency is exactly why tranche 1 (runner.rs) can be
self-contained: it has no forward references into tranche 2.

**(c) What pm-shadow imports from pm-alpha that must not break.**
`pm-shadow`'s `Cargo.toml` depends only on `pm-types` and `pm-alpha`, not on
`pm-app`/`runner`/`walkforward`/`result_summary` at all
(`grep -n "pm_app\|use crate::runner\|use crate::walkforward" crates/pm-shadow/src/*.rs`
returns nothing). So this extraction has **zero** direct effect on pm-shadow's
compile graph. What it imports from pm-alpha, which must keep its current
public shape through Tasks 3-5: `pm_alpha::harness::{EntryMode, Side}`,
`pm_alpha::PerpState`, `pm_alpha::Belief`, `pm_alpha::harness::spot_ret_bps`,
`pm_alpha::frozen_fade_decide_config` (test-only import). None of these live
in the three files being moved, so nothing here forces ordering; it only
matters as a "don't touch pm-alpha's public surface incidentally" guardrail
during the moves. It becomes directly relevant later at Task 12 Step 5
(pm-shadow generalized over `pm_strategy::Strategy`), which will likely also
pull in pm-backtest, but that's out of this task's scope.

**(d) The exo_fade dispatch path that must survive until Task 12.**
`walkforward.rs`'s `run_one_strategy` (line 3040-3136) is the entire seam:

```rust
let report = match strat {
    StratId::ExoFade | StratId::MayJuneFade => {
        // ... token/window_secs resolution, ExoFadeConfig::champion_1k() or
        // ::mayjune_btc5m(), ExoFadeStrategy::new(...), .with_perp(...)
        run_backtest(events, spot, trades, &mut s, runner_cfg)?
    }
    StratId::Noop => {
        let mut s = NoopStrategy;
        run_backtest(events, spot, trades, &mut s, runner_cfg)?
    }
};
```

Upstream of this: the `StratId` enum itself (`walkforward.rs:43-49`, `impl StratId` at `64-91`,
with `ACTIVE`/`ALL` arrays naming `[ExoFade, MayJuneFade, Noop]`), CLI parsing
in `main.rs::parse_strategies` (not one of the 3 files, stays pm-app,
consumes `StratId::from_name`/`StratId::all_names`), and `pm_strategy::{ExoFadeStrategy, NoopStrategy, exo_fade::ExoFadeConfig}`
imports (`walkforward.rs:18`). All of `run_one_strategy`, `StratId`, and its
`use pm_strategy::{ExoFade...}` import must move into pm-backtest intact in
tranche 2 and stay unmodified through Tasks 6-11; Task 12 Step 4 is what
finally deletes the `ExoFade`/`MayJuneFade` match arms and the `pm_strategy::exo_fade`
import, leaving only the `Noop` (and later fixture) arm.

**Two additional hard seams found while mapping (not among the brief's 4, but
compile-blocking for tranche 2):**

1. `walkforward.rs:34` imports `crate::discovery::{MarketHandle, parse_close_ts, spot_cache_key, spot_symbol_for_market}`.
   `MarketHandle` is `run_walkforward`'s market-list element type
   (`markets: &[MarketHandle]`). `discovery.rs` (688 lines) is otherwise pure
   pm-app manifest/local-cache-scan glue also used by `alpha.rs`, `main.rs`,
   `prep_cache.rs`, and correctly stays in pm-app. But these 4 items
   (`MarketHandle` struct at `discovery.rs:21`, `spot_symbol_for_market` at `:32`,
   `spot_cache_key` at `:44`, `parse_close_ts` at `:450`) must move into
   pm-backtest (proposed: `pm-backtest::config`) alongside tranche 2, or
   tranche 2 will not compile. `alpha.rs`, `main.rs`, `prep_cache.rs` then
   import `MarketHandle` etc. from `pm_backtest` instead of `crate::discovery`
   (small import-line changes outside the 3-file scope, but necessary).
2. `walkforward.rs:660` calls `crate::perp::load_perp_state` inside
   `load_walkforward_perp` (`walkforward.rs:645-667`), which `run_walkforward`
   calls directly at `walkforward.rs:1599`. `perp.rs` (186 lines) is pure
   local-cache I/O returning `pm_alpha::PerpState`, also called directly by
   `alpha.rs:952` (pm-app, stays). The whole file should move into
   pm-backtest (proposed: `pm-backtest::portfolio`, since it feeds
   run-level shared state the same way `SpotCache` does) alongside tranche 2;
   `alpha.rs`'s call site needs its import updated to `pm_backtest::perp::load_perp_state`.

Neither of these is in the plan's stated "Files: Modify" list for Task 3/4,
so flagging them here is the point of this map: without this, Task 4's
implementer hits a wall of unresolved-import errors mid-tranche and either
guesses wrong or burns a review round.

## Step 1 + 3: the item table

Columns: item, file:line, destination, tranche, notes. "Tranche" is when the
item physically moves (forced by compile-order, see rationale below), which
is not always the same axis as which pm-backtest submodule it lands in.

### Tranche 3 (Task 3): runner.rs, all 35 items + `mod tests`

runner.rs has zero `crate::` references to walkforward.rs or result_summary.rs
(verified: `grep -oE "crate::[a-z_]+" crates/pm-app/src/runner.rs` returns
nothing), so it is fully self-contained and moves first, in its entirety.

| Item | Location | Destination | Notes |
|---|---|---|---|
| `Fill` (struct) | runner.rs:37 | pm-backtest::fills | |
| `PostFillPath` (struct) | runner.rs:114 | pm-backtest::fills | |
| `FillModelContext` (struct) | runner.rs:125 | pm-backtest::fills | decision-log feature snapshot, "Logging only" per its own doc comment |
| `BinanceFlowFeatures` (struct) | runner.rs:149 | pm-backtest::fills | order-flow features, logging only |
| `impl BinanceFlowFeatures` | runner.rs:165 | pm-backtest::fills | `.compute()` |
| `impl FillModelContext` | runner.rs:191 | pm-backtest::fills | `.from_event()` |
| `StrategyCounters` (struct) | runner.rs:238 | pm-backtest::accounting | order/fill counters feeding P&L reporting |
| `BacktestReport` (struct) | runner.rs:256 | pm-backtest::accounting | `run_backtest`'s return type; consumed cross-tranche by `run_one_strategy` (tranche 4/walkforward) |
| `DecisionLogRow` (struct) | runner.rs:280 | pm-backtest::fills | per-order audit log row, populated inline in the fill/order path |
| `RunnerConfig` (struct) | runner.rs:375 | pm-backtest::config | consumed cross-tranche by `run_one_strategy`'s `runner_cfg: &RunnerConfig` param |
| `impl Default for RunnerConfig` | runner.rs:464 | pm-backtest::config | |
| `RestingOrder` (struct) | runner.rs:510 | pm-backtest::fills | |
| `PendingTakerOrder` (struct) | runner.rs:522 | pm-backtest::fills | |
| `run_backtest<S: Strategy>` (fn) | runner.rs:528-1268 | pm-backtest::engine | the event loop itself; pm_model eval seam lives inside this fn (see Step 2a) |
| `write_decision_rows_parquet` (fn) | runner.rs:1269 | pm-backtest::fills | called from inside `run_backtest` (runner.rs:1242), not from pm-app; travels with the decision-log feature |
| `order_request_notional_usdc` (fn) | runner.rs:1653 | pm-backtest::fills | |
| `side_model_probability` (fn) | runner.rs:1722 | pm-backtest::fills | reads `ModelOutput`, does not itself call pm_model |
| `model_gate_edge_for_order` (fn) | runner.rs:1731 | pm-backtest::fills | part of the model-gate check alongside the pm_model seam |
| `mark_to_market` (fn) | runner.rs:1751 | pm-backtest::accounting | |
| `annotate_post_fill_paths` (fn) | runner.rs:1756 | pm-backtest::fills | |
| `side_mid_for_fill` (fn) | runner.rs:1797 | pm-backtest::fills | |
| `limit_to_yes_terms` (fn) | runner.rs:1808 | pm-backtest::fills | |
| `order_adds_yes_exposure` (fn) | runner.rs:1815 | pm-backtest::fills | also used by `FillModelContext::from_event` |
| `would_reduce_imbalance` (fn) | runner.rs:1822 | pm-backtest::fills | |
| `apply_maker_fill` (fn) | runner.rs:1833 | pm-backtest::fills | |
| `check_trade_driven_resting_fills` (fn) | runner.rs:1951 | pm-backtest::fills | |
| `check_resting_fills` (fn) | runner.rs:2023 | pm-backtest::fills | |
| `submit_maker_order` (fn) | runner.rs:2178 | pm-backtest::fills | |
| `submit_taker_order` (fn) | runner.rs:2265 | pm-backtest::fills | |
| `process_pending_takers` (fn) | runner.rs:2305 | pm-backtest::fills | |
| `apply_taker_order` (fn) | runner.rs:2342 | pm-backtest::fills | |
| `depth_weighted_fill` (fn) | runner.rs:2489 | pm-backtest::fills | the book-walk fill model |
| `order_requires_model_gate` (fn) | runner.rs:2532 | pm-backtest::fills | |
| `fill_respects_limit` (fn) | runner.rs:2536 | pm-backtest::fills | |
| `pretty_print(&BacktestReport)` (fn) | runner.rs:2546 | pm-backtest::accounting | pure presentation; travels with `BacktestReport` for consistency (see ambiguous-items ruling below) rather than staying in pm-app |
| `mod tests` | runner.rs:2626 | pm-backtest (module split TBD by Task 3's implementer) | moves verbatim |

### Tranche 4 (Task 4): walkforward.rs, all 107 items + `mod tests`, plus the two companion files

`run_walkforward` (walkforward.rs:1565) directly calls `aggregate`, `train_validated_meta_calibrator`,
`evaluate_meta_calibration`, `run_markets`, `run_portfolio`, `build_fold_plan`,
`load_walkforward_perp` in its own body, and constructs `SpotCache`. That
means essentially the entire walkforward.rs call graph is reachable from
`run_walkforward` and must move together, in this tranche, or `run_walkforward`
won't compile. Only `print_summary` is genuinely detachable (it's a pure leaf
consumer of `WalkForwardSummary`, called once from main.rs) but it is kept
here too, for the same "presentation travels with its data type" rule as
`pretty_print` above.

| Item | Location | Destination | Notes |
|---|---|---|---|
| `DEFAULT_META_MAX_FIT_SAMPLES` (const) | walkforward.rs:37 | pm-backtest::engine | |
| `DEFAULT_META_MAX_VALIDATION_SAMPLES` (const) | walkforward.rs:38 | pm-backtest::engine | |
| `DEFAULT_META_MAX_OOS_EVALUATION_SAMPLES` (const) | walkforward.rs:39 | pm-backtest::engine | |
| `DEFAULT_META_MAX_SAMPLES_PER_MARKET` (const) | walkforward.rs:40 | pm-backtest::engine | |
| `StratId` (enum) | walkforward.rs:43 | pm-backtest::engine | see Step 2d |
| `VolatilityBand` (enum) | walkforward.rs:50 | pm-backtest::portfolio | |
| `impl VolatilityBand` | walkforward.rs:55 | pm-backtest::portfolio | |
| `impl StratId` | walkforward.rs:64 | pm-backtest::engine | `from_name`/`all_names`/`name()`, consumed by pm-app's `parse_strategies` (main.rs) |
| `WalkForwardConfig` (struct) | walkforward.rs:92 | pm-backtest::config | |
| `impl Default for WalkForwardConfig` | walkforward.rs:235 | pm-backtest::config | |
| `MarketResult` (struct) | walkforward.rs:305 | pm-backtest::accounting | |
| `StrategyMarketResult` (struct) | walkforward.rs:316 | pm-backtest::accounting | |
| `market_duration_secs_from_slug` (fn, `pub(crate)`) | walkforward.rs:348 | pm-backtest::accounting | used by `run_one_strategy`'s exo_fade dispatch (Step 2d) |
| `market_open_ts` (fn) | walkforward.rs:359 | pm-backtest::accounting | |
| `market_close_ts` (fn) | walkforward.rs:366 | pm-backtest::accounting | |
| `market_open_ns` (fn, `pub(crate)`) | walkforward.rs:376 | pm-backtest::accounting | |
| `market_close_ns` (fn, `pub(crate)`) | walkforward.rs:380 | pm-backtest::accounting | |
| `outcome_label_resolved_yes` (fn, `pub(crate)`) | walkforward.rs:384 | pm-backtest::accounting | |
| `validate_outcome_labels` (fn, `pub(crate)`) | walkforward.rs:394 | pm-backtest::accounting | called from main.rs at `crate::walkforward::validate_outcome_labels` too; stays `pub(crate)` inside pm-backtest, main.rs calls the crate-public path |
| `prior_market_range_mean` (fn) | walkforward.rs:408 | pm-backtest::accounting | |
| `WalkForwardSummary` (struct) | walkforward.rs:425 | pm-backtest::scorecard | `run_walkforward`'s own return type; see tranche-2-not-3 correction above |
| `SummaryRunConfig` (struct) | walkforward.rs:442 | pm-backtest::scorecard | |
| `SharedRunConfig` (struct) | walkforward.rs:452 | pm-backtest::portfolio | |
| `impl From<&WalkForwardConfig> for SharedRunConfig` | walkforward.rs:519 | pm-backtest::portfolio | |
| `spot_symbol_mode` (fn) | walkforward.rs:607 | pm-backtest::portfolio | |
| `spot_symbol_override` (fn) | walkforward.rs:617 | pm-backtest::portfolio | |
| `needs_perp_for_strategies` (fn) | walkforward.rs:625 | pm-backtest::portfolio | |
| `resolve_perp_symbol` (fn) | walkforward.rs:632 | pm-backtest::portfolio | |
| `load_walkforward_perp` (async fn) | walkforward.rs:645 | pm-backtest::portfolio | calls `crate::perp::load_perp_state` (companion move, see Step 2 extra seam 2) |
| `StrategyRunConfig` (struct) | walkforward.rs:668 | pm-backtest::scorecard | embedded in `SummaryRunConfig` |
| `MetaCalibrationReport` (struct) | walkforward.rs:675 | pm-backtest::scorecard | |
| `MetaCandidateEvaluation` (struct) | walkforward.rs:697 | pm-backtest::scorecard | |
| `WalkForwardFoldSummary` (struct) | walkforward.rs:712 | pm-backtest::scorecard | |
| `MetaEvaluationSummary` (struct) | walkforward.rs:725 | pm-backtest::scorecard | |
| `PredictionDistribution` (struct) | walkforward.rs:755 | pm-backtest::scorecard | |
| `CalibrationBin` (struct) | walkforward.rs:767 | pm-backtest::scorecard | |
| `market_volatility_range` (fn) | walkforward.rs:775 | pm-backtest::portfolio | |
| `volatility_band` (fn) | walkforward.rs:799 | pm-backtest::portfolio | |
| `sample_replay_events` (fn) | walkforward.rs:810 | pm-backtest::engine | |
| `read_replay_event_cache` (fn) | walkforward.rs:850 | pm-backtest::engine | |
| `write_replay_event_cache` (fn) | walkforward.rs:868 | pm-backtest::engine | |
| `rebind_replay_event_market_ids` (fn) | walkforward.rs:889 | pm-backtest::engine | |
| `replay_event_cache_path` (fn) | walkforward.rs:895 | pm-backtest::engine | |
| `load_replay_events_for_market` (async fn, `pub(crate)`) | walkforward.rs:902 | pm-backtest::engine | |
| `compounded_clip` (fn) | walkforward.rs:959 | pm-backtest::portfolio | |
| `per_market_exposure_cap` (fn) | walkforward.rs:972 | pm-backtest::portfolio | |
| `drawdown_clip_multiplier` (fn) | walkforward.rs:981 | pm-backtest::portfolio | |
| `daily_remaining_loss_budget_usdc` (fn) | walkforward.rs:1009 | pm-backtest::portfolio | |
| `LossStreakCooldownState` (struct) | walkforward.rs:1023 | pm-backtest::portfolio | |
| `impl LossStreakCooldownState` | walkforward.rs:1028 | pm-backtest::portfolio | |
| `StrategyAggregate` (struct) | walkforward.rs:1061 | pm-backtest::scorecard | |
| `ModelFillQualitySummary` (struct) | walkforward.rs:1098 | pm-backtest::scorecard | |
| `ModelFillQuality` (struct) | walkforward.rs:1109 | pm-backtest::scorecard | |
| `FillTagAggregate` (struct) | walkforward.rs:1118 | pm-backtest::scorecard | |
| `FillTagAccumulator` (struct, private) | walkforward.rs:1142 | pm-backtest::scorecard | name collision with result_summary.rs's own `FillTagAccumulator` (tranche 5); different field sets (this one has an extra `wins` field), both module-private so no compile collision once each lands in its own submodule, flagged as pre-existing near-duplicate tech debt, not touched (verbatim-move rule) |
| `ModelFillQualityAccumulator` (struct) | walkforward.rs:1168 | pm-backtest::scorecard | |
| `ModelFillQualityBucket` (struct) | walkforward.rs:1179 | pm-backtest::scorecard | |
| `impl ModelFillQualityAccumulator` | walkforward.rs:1187 | pm-backtest::scorecard | |
| `impl ModelFillQualityBucket` | walkforward.rs:1253 | pm-backtest::scorecard | |
| `impl FillTagAccumulator` | walkforward.rs:1294 | pm-backtest::scorecard | includes `into_aggregate` (walkforward.rs:1345) |
| `SpotCache` (struct, `pub(crate)`) | walkforward.rs:1433 | pm-backtest::portfolio | |
| `impl SpotCache` | walkforward.rs:1438 | pm-backtest::portfolio | |
| `previous_date` (fn) | walkforward.rs:1506 | pm-backtest::portfolio | |
| `spot_history_for_market` (fn) | walkforward.rs:1514 | pm-backtest::portfolio | |
| `model_market_context_for_slug` (fn) | walkforward.rs:1532 | pm-backtest::portfolio | |
| `model_market_context_for_cfg` (fn) | walkforward.rs:1554 | pm-backtest::portfolio | |
| `run_walkforward` (async fn, pub) | walkforward.rs:1565 | pm-backtest::engine | the top-level orchestration entry; root of the whole tranche-4 call graph |
| `build_fold_plan` (fn) | walkforward.rs:1819 | pm-backtest::engine | |
| `load_or_collect_training_samples` (async fn) | walkforward.rs:1886 | pm-backtest::engine | |
| `write_meta_training_samples` (fn) | walkforward.rs:1923 | pm-backtest::engine | |
| `read_meta_snapshot` (fn) | walkforward.rs:1940 | pm-backtest::engine | |
| `write_meta_snapshot` (fn) | walkforward.rs:1952 | pm-backtest::engine | |
| `ensure_parent_dir` (fn) | walkforward.rs:1969 | pm-backtest::engine | |
| `SelectedMetaCalibrator` (struct) | walkforward.rs:1977 | pm-backtest::engine | |
| `META_FEATURE_EARLY_MARKET_PENALTY` (const) | walkforward.rs:1982 | pm-backtest::engine | |
| `META_FEATURE_MID_DISTANCE_FROM_HALF` (const) | walkforward.rs:1983 | pm-backtest::engine | |
| `MetaSampleLimits` (struct) | walkforward.rs:1986 | pm-backtest::engine | |
| `impl MetaSampleLimits` | walkforward.rs:1995 | pm-backtest::engine | |
| `filter_meta_samples_for_training` (fn) | walkforward.rs:2008 | pm-backtest::engine | |
| `train_validated_meta_calibrator` (fn) | walkforward.rs:2029 | pm-backtest::engine | called directly by `run_walkforward` |
| `split_meta_samples_by_market` (fn) | walkforward.rs:2230 | pm-backtest::engine | |
| `market_balanced_meta_samples` (fn) | walkforward.rs:2253 | pm-backtest::engine | |
| `group_meta_samples_by_market` (fn) | walkforward.rs:2295 | pm-backtest::engine | |
| `extend_evenly_sampled<T>` (fn) | walkforward.rs:2305 | pm-backtest::engine | |
| `meta_training_candidates` (fn) | walkforward.rs:2325 | pm-backtest::engine | |
| `meta_calibration_report` (fn) | walkforward.rs:2367 | pm-backtest::scorecard | builds `MetaCalibrationReport` |
| `evaluate_meta_calibration` (fn) | walkforward.rs:2403 | pm-backtest::engine | called directly by `run_walkforward` |
| `prediction_distribution` (fn) | walkforward.rs:2578 | pm-backtest::scorecard | |
| `CalibrationBinAccumulator` (struct) | walkforward.rs:2606 | pm-backtest::scorecard | |
| `MarketCalibrationAccumulator` (struct) | walkforward.rs:2613 | pm-backtest::scorecard | |
| `binary_log_loss` (fn) | walkforward.rs:2622 | pm-backtest::scorecard | |
| `collect_training_samples` (async fn) | walkforward.rs:2627 | pm-backtest::engine | |
| `collect_training_samples_for_market` (async fn) | walkforward.rs:2688 | pm-backtest::engine | |
| `run_markets` (async fn) | walkforward.rs:2794 | pm-backtest::engine | called directly by `run_walkforward` |
| `run_one_strategy` (fn) | walkforward.rs:3040 | pm-backtest::engine | the exo_fade dispatch path, see Step 2d |
| `run_portfolio` (async fn) | walkforward.rs:3141 | pm-backtest::engine | called directly by `run_walkforward`; sequential/compounding portfolio-mode path |
| `model_state_with_snapshot` (fn) | walkforward.rs:3478 | pm-backtest::engine | constructs `pm_model::ModelState` from a training snapshot for the loop |
| `write_portfolio_checkpoint` (fn) | walkforward.rs:3486 | pm-backtest::scorecard | calls `write_market_results_jsonl_atomic`/`write_summary_json_atomic` internally, see ambiguous-items ruling |
| `write_market_results_jsonl_atomic` (fn, pub) | walkforward.rs:3532 | pm-backtest::scorecard | called both by main.rs (CLI output) and internally by `write_portfolio_checkpoint`; internal reuse rules out "stays pm-app" |
| `write_summary_json_atomic` (fn, pub) | walkforward.rs:3556 | pm-backtest::scorecard | same reasoning |
| `temp_sibling_path` (fn) | walkforward.rs:3572 | pm-backtest::scorecard | |
| `aggregate_for_strategy` (fn) | walkforward.rs:3584 | pm-backtest::scorecard | |
| `fill_resolution_pnl` (fn) | walkforward.rs:3747 | pm-backtest::accounting | takes `crate::runner::Fill`, i.e. `pm_backtest::fills::Fill` post-move |
| `buy_fill_won` (fn) | walkforward.rs:3769 | pm-backtest::accounting | same |
| `aggregate` (fn) | walkforward.rs:3777 | pm-backtest::scorecard | called directly by `run_walkforward`, see tranche correction above |
| `summary_run_config` (fn) | walkforward.rs:3821 | pm-backtest::scorecard | |
| `print_summary` (fn, pub) | walkforward.rs:3835 | pm-backtest::scorecard | presentation, travels with `WalkForwardSummary` (see ruling) |
| `mod tests` | walkforward.rs:3945 | pm-backtest (module split TBD) | moves verbatim |
| **Companion move 1:** `MarketHandle` (struct) | discovery.rs:21 | pm-backtest::config | required for `run_walkforward`'s signature; rest of discovery.rs stays pm-app |
| **Companion move 2:** `spot_symbol_for_market` (fn) | discovery.rs:32 | pm-backtest::config | |
| **Companion move 3:** `spot_cache_key` (fn) | discovery.rs:44 | pm-backtest::config | |
| **Companion move 4:** `parse_close_ts` (fn) | discovery.rs:450 | pm-backtest::config | |
| **Companion move 5:** all of perp.rs (`load_perp_state` pub async fn + 4 private helpers, 186 lines) | perp.rs:1-186 | pm-backtest::portfolio | called directly by `run_walkforward` via `load_walkforward_perp`; `alpha.rs:952` (pm-app) updates its import to the new path |

### Tranche 5 (Task 5): result_summary.rs, all 16 items + `mod tests`, plus collapsing the walkforward.rs shell

result_summary.rs has zero `crate::` references to either of the other two
files and is never referenced from them (`grep -oE "crate::[a-z_]+" crates/pm-app/src/result_summary.rs`
returns nothing; `grep -rn "result_summary" crates/pm-app/src/{walkforward,runner}.rs`
returns nothing). It is called only from one main.rs site
(`main.rs:1625-1628`, the standalone `summarize_markets_jsonl` /
`print_result_summary` / `write_result_summary_json` post-processing path).
Fully independent, moves last per the plan's explicit Task 5 file list.

| Item | Location | Destination | Notes |
|---|---|---|---|
| `MARKETS_PER_DAY` (const) | result_summary.rs:10 | pm-backtest::scorecard | |
| `ResultSummary` (struct) | result_summary.rs:13 | pm-backtest::scorecard | |
| `DailyResultSummary` (struct) | result_summary.rs:48 | pm-backtest::scorecard | |
| `FillTagSummary` (struct) | result_summary.rs:65 | pm-backtest::scorecard | |
| `FillTagAccumulator` (struct, private) | result_summary.rs:86 | pm-backtest::scorecard | see the tranche-4 collision note; different module, no compile clash |
| `impl FillTagAccumulator` | result_summary.rs:109 | pm-backtest::scorecard | |
| `MarketResultRow` (struct) | result_summary.rs:232 | pm-backtest::scorecard | |
| `StrategyResultRow` (struct) | result_summary.rs:240 | pm-backtest::scorecard | |
| `FillRow` (struct) | result_summary.rs:270 | pm-backtest::scorecard | |
| `PostFillPathRow` (struct) | result_summary.rs:302 | pm-backtest::scorecard | |
| `DailyAccumulator` (struct) | result_summary.rs:309 | pm-backtest::scorecard | |
| `impl DailyAccumulator` | result_summary.rs:324 | pm-backtest::scorecard | |
| `close_day_key` (fn) | result_summary.rs:377 | pm-backtest::scorecard | |
| `summarize_markets_jsonl` (fn, pub) | result_summary.rs:384 | pm-backtest::scorecard | reads a markets JSONL file from disk and re-derives the summary; this is I/O but of pm-backtest's own output format, not CLI-arg-driven glue |
| `write_result_summary_json` (fn, pub) | result_summary.rs:601 | pm-backtest::scorecard | |
| `print_result_summary` (fn, pub) | result_summary.rs:610 | pm-backtest::scorecard | presentation; plan explicitly commits the whole file to scorecard |
| `mod tests` | result_summary.rs:700 | pm-backtest::scorecard (module split TBD) | moves verbatim |

**Collapsing walkforward.rs in pm-app (also tranche 5):** after tranche 4,
walkforward.rs's entire logical content has moved to pm-backtest; nothing
walkforward.rs-specific is left to classify. Task 5's job here is
mechanical: delete the emptied file (or leave a thin `pub use pm_backtest::engine::run_walkforward;`
style re-export if main.rs's import ergonomics call for it), and fold
`main.rs`'s CLI-facing bits (arg-struct-to-`WalkForwardConfig` construction,
`parse_strategies`, the final `run_walkforward(...).await?` call site, output
writing) to call `pm_backtest::` paths directly. No new items to classify;
this is a "delete/fold" step, not a "move" step, since the map already routed
every real item out in tranche 4.

## Delete

None of the 158 items are unreachable/dead. (Distinct from Task 12's later
kill of `exo_fade`/`MayJuneFade`/`pm-model`, which is out of scope here per
the plan: this task's gate is "the map Tasks 3-5 execute," and Tasks 3-5 are
verbatim-move-only, explicitly forbidden from behavior changes.)

## Ambiguous items and rulings

- **`write_market_results_jsonl_atomic` / `write_summary_json_atomic`**: called both from main.rs (CLI output) and internally by `write_portfolio_checkpoint` mid-run. Ruled pm-backtest::scorecard (engine-internal reuse rules out "stays pm-app" under the plan's own taxonomy, which reserves "stays pm-app" for pure CLI-arg/manifest/output-path glue that the engine itself never calls).
- **`print_summary` / `pretty_print` / `print_result_summary`**: pure `println!` presentation, each called from exactly one main.rs site, which would superficially argue for "stays pm-app." Ruled: they travel with the data type they print, into pm-backtest, for consistency with the plan's own explicit instruction that all of result_summary.rs (including its `print_result_summary`) moves to `pm-backtest::scorecard`. Treating the other two print functions differently would be an arbitrary inconsistency for no compile-order reason.
- **`FillTagAccumulator` name collision**: two distinct private structs, same name, different field sets (walkforward.rs:1142 has an extra `wins: usize` walkforward.rs:1142 lacks in result_summary.rs:86). Both move to pm-backtest::scorecard but land in different submodules/files, so there's no actual Rust name collision (both are module-private); flagged as pre-existing near-duplicate tech debt for a future cleanup, not touched now (verbatim-move rule for Tasks 3-5 forbids logic edits, and a rename/dedup would be a logic edit to reviewers' eyes even though it's just naming).
- **Meta-calibration training pipeline (~24 items) and `aggregate`/`WalkForwardSummary`/`StrategyAggregate`**: first pass (by analogy to "result_summary.rs is later") assumed these could wait for tranche 5. Wrong: `run_walkforward` calls `train_validated_meta_calibrator`, `evaluate_meta_calibration`, and `aggregate` directly in its own body (walkforward.rs:1794, 1811, plus the fold-training call around line 1665-ish inside the fold-count>1 branch). Corrected to tranche 4; documented above so a future map doesn't repeat the mistake.
- **Two companion files (discovery.rs's `MarketHandle`+3 fns, all of perp.rs)**: not part of the brief's named 3 files, but hard compile dependencies of tranche 4 discovered by grepping `crate::` in walkforward.rs. Included explicitly rather than left for Task 4's implementer to discover mid-tranche.

## Tranche ordering rationale

1. **Tranche 3 (Task 3) = runner.rs, in full.** Zero forward references to walkforward.rs or result_summary.rs (confirmed by grep), so it is provably self-contained and safe to move first. `run_backtest` and everything under it (fills, the model-gate seam, `BacktestReport`/`StrategyCounters` accounting types) lands in pm-backtest. walkforward.rs (still in pm-app at this point) keeps compiling by importing `pm_backtest::{...}` instead of `crate::runner::{...}`.
2. **Tranche 4 (Task 4) = walkforward.rs, in full, plus the two companion files.** Everything in walkforward.rs is reachable, directly or transitively, from `run_walkforward`'s own function body (fold planning, market iteration, portfolio mode, meta-calibrator training, checkpoint writing, and the final `aggregate()` call that produces its own return type). Splitting this into "orchestration+accounting now, scorecard-shaped stuff later" (as the plan's prose loosely suggests) does not survive contact with the actual call graph: `run_walkforward` calls `aggregate` and the meta-training functions itself, so they must move in the same tranche or tranche 4 does not compile. The two companion files (discovery.rs's `MarketHandle` slice, all of perp.rs) are pulled in for the same reason. This is the biggest, riskiest tranche; the "no logic edits, only path/visibility" discipline matters most here given its size.
3. **Tranche 5 (Task 5) = result_summary.rs, in full, plus collapsing the (by then nearly empty) walkforward.rs file in pm-app.** Provably independent of the other two files in both directions (no `crate::` references either way), so it can move whenever; the plan fixes it last. After tranche 4, walkforward.rs has nothing left to classify: it becomes pure deletion/CLI-rewiring in main.rs, matching the plan's own "reduced to thin CLI glue... or deleted with its glue folded into main.rs" framing, and the `wc -l crates/pm-app/src/walkforward.rs` under-~400-lines (or absent) gate should be easy to hit since essentially the entire file left with tranche 4.

Each tranche's gate (`cargo build --release -p pm-app`, `golden_replay.sh check` = `GOLDEN: IDENTICAL`, `cargo test --workspace`, `exo_fade_equivalence` PASS) is unaffected by this map; the map only decides *where* code goes and *when*, not what the code does.
