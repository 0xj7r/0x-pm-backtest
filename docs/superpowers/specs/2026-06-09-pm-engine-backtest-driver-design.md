# Design: pm-engine Phase 2 — faithful recorded-data backtest driver

Date: 2026-06-09
Status: DESIGN (approved in brainstorming 2026-06-09)

## Problem

Phase 1 built the `pm-engine` core (sync, single-threaded, deterministic; hosts
`BonereaperV2` unchanged via one `build_ctx`; risk/portfolio/exposure; multi-market
scheduler) but proved it only on **mock seams** (`InstantExchange`, `ScriptedFeed`,
empty histories, `..Ctx::default()`). Phase 2 turns the engine onto **real recorded
data**: a backtest driver that replaces `pm-app`'s bespoke `run_backtest`/walk-forward
loop, fed by the ingested both-legs parquet, with a faithful both-book fill model.

This is the run that finally drives the engine on real markets and produces br2 results
under realistic execution.

## Locked decisions (brainstorming 2026-06-09)

1. **Standalone validation** (no champion byte-reproduction). We do NOT build a
   synthetic-NO compatibility mode. Validation = (a) golden-trace determinism on the
   real cache, (b) a br2 *plausibility tripwire* (trade-count/behavior sane, no crashes
   — NOT a P&L match), (c) measure and report the P&L delta vs the old synthetic-NO
   result as the both-book correction.
2. **BTC-5m first, then all cells.** Build + validate the driver on the single BTC-5m
   cell (anchored to the known ~+4.34% May reference as a bug tripwire). Multi-cell
   turn-on is a later Feed/classifier change, not new engine work.
3. **Shared `CtxEnricher` in `pm-engine`.** Regime scores, model evaluation, and
   prior-range are computed in ONE place used by the single engine loop, so the backtest
   and live drivers produce identical `Ctx`. Model *evaluation* lives in the engine;
   calibrator *training* stays a backtest-driver step (live loads a frozen snapshot).
4. **Real both-book fill model** (from the engine spec): real opposing-ladder VWAP,
   real fees, latency, trade-tape-driven maker fills, no queue-jump. No fitted queue
   model.

## Expected outcome and the br2 re-formulation follow-on

Under faithful fills, br2's edge will very likely shift — possibly materially — because
the champion (062901) was tuned on synthetic-NO idealized fills running ~0.5–1¢
optimistic. **That shift is the finding, not a regression.** The planned immediate
follow-on (its own cycle, gated on this driver) is a **br2 re-formulation**: re-sweep
config, re-tune the edge gate + sizing, and/or re-train the meta-calibrator against the
faithful backtests. This design keeps strategy config-driven and the in-driver
calibrator-training path reusable so re-calibration is first-class.

## Architecture

Five components; the first four are built, the fifth is the validation harness.

### 1. Leg-pairing + recorded Feed (`pm-engine` recorded Feed impl + `pm-app` loading)

Today the walk-forward treats each market's YES and NO legs as **separate**
`MarketHandle`/`MarketId` runs (synthetic NO). Phase 2 **pairs** them: using the
`outcome` label (`walkforward.rs:1590` `outcome_label_resolved_yes`, "Up"/"Yes" → YES,
"Down"/"No" → NO), the two legs of a market become ONE engine market. The recorded Feed:

- Loads each leg's `book_snapshot_25` parquet via the existing
  `load_book_snapshot_async` (`pm-telonex-loader/src/book_snapshot.rs:26`) → YES leg
  becomes the `ReplayEvent` (with `bids`/`asks`, `yes_mid`), NO leg's ladder becomes the
  `NoBook` carried alongside in `EngineEvent::Market`. The two legs are aligned by
  `ts_ns` (forward-fill the most recent NO snapshot at/just-before each YES event ts; and
  vice-versa — emit an event on either leg's update, carrying the latest of the other).
- Loads `SpotHistory` (`load_binance_agg_trades_async` + `SpotHistory::new`,
  `walkforward.rs:2970`) and `TradeHistory` (`load_pm_trades_async`,
  `walkforward.rs:4355`) — provided to the engine for the `CtxEnricher` and fill model.
- k-way-merges all markets' `EngineEvent`s into one `ts_ns`-ordered stream (single-cell
  = BTC-5m for now; the merge generalizes to all cells unchanged).

Note: `ReplayEvent.spot_price` is left 0.0 by the loader (`book_snapshot.rs:139`); spot
enters via `SpotHistory`, not the event. The `CtxEnricher` and regime use `SpotHistory`.

### 2. Real both-book sim Exchange (`pm-engine`, `Exchange` impl)

Replaces the synthetic NO=1−yes pricing (`runner.rs:2486-2527`). Holds resting orders,
the latest `BothBook` (YES `bids`/`asks` + the `NoBook`), and the recent trade tape.

- **Taker** (`limit_price = None`): on submit, queue with `submit_ts`; realize the fill
  when `clock.now() >= submit_ts + latency` (ports `PendingTakerOrder` semantics,
  `runner.rs:693-706`). Price = VWAP sweeping the **real opposing ladder** — YES `asks`
  for BuyYes, the **real NO `asks`** for BuyNo (NOT `1 - yes`) — using the book at fill
  time (latency-aware re-read), capped at `max_depth`. Real taker fee bps. Partial if
  depth insufficient.
- **Maker** (`limit_price = Some`): rest in book; fill on (i) book-cross
  (`check_resting_fills`, `runner.rs:2020`) and (ii) the **real PM trade tape** crossing
  the level with queue-priority `trade.ts_ns > submit_ts` (`check_trade_driven_resting_fills`,
  `runner.rs:1948`). No queue-jump. Maker rebate bps.
- Config carried via the existing fields (`taker_latency_ms`, `taker_fee_bps`,
  `maker_rebate_bps`); champion uses `--taker-latency-ms 500`, `--replay-sample-ms 1000`.

### 3. `CtxEnricher` (`pm-engine`, shared by both drivers)

Computes per-event, before the strategy call, the `Ctx` fields Phase 1 left default:

- **Regime scores** via `WhipsawRiskSnapshot::from_history(event.ts_ns, spot)`
  (`pm-strategy/src/regime.rs:64`): `whipsaw_score`, `path_efficiency`,
  `reversal_pressure`, `sign_flip_rate`, `realized_vol_180s_bps` — a trailing 180 s
  look-back over `SpotHistory`, sampled at 5 s. **Live-safe by construction** (reads only
  ticks at/before the event ts).
- **Model output** via `ModelState::evaluate_detailed_with_market_context(event, spot,
  secs_since_open, &model_cfg, market_context)` (`runner.rs:730-747`) →
  `model_output`/`model_attribution`. The `ModelState`/`OnlineMetaCalibrator` is loaded
  from a snapshot (`read_meta_snapshot` → `OnlineMetaCalibratorSnapshot`,
  `walkforward.rs:3440`); for BTC-5m validation we load `data/snap062901.json`.
- **Prior-range** (`prior_market_range_{1d,3d,7d}`) via `prior_market_range_mean`
  (`walkforward.rs:1614`) over already-closed prior markets (288/864/2016 5-min markets).
- **Cross-market net exposure**: filled from the engine's `ExposureState` (Phase 1).
  Note: today only BackToExplore feeds `asset_net` (`walkforward.rs:4507`); br2 does not,
  so for BTC-5m-only br2 these stay ~0 — acceptable for this cell.

**Evaluation vs training split:** the `CtxEnricher` only *evaluates* the loaded model
(shared, live-identical). Calibrator *training/update* (the champion trained inline,
`--meta-epochs 10`) is a **backtest-driver** responsibility, invoked across markets via
the `on_market_resolved` hook + a training pass — never in the live path.

The engine holds handles to `SpotHistory`/`TradeHistory` supplied by the driver
(backtest: preloaded per the existing 2-day `SpotDayCache` window; live: a rolling
buffer). Exact per-market swap vs global handle is a plan-level detail.

### 4. Thin `pm-app` backtest driver

Replaces the bespoke `run_backtest`/walk-forward loop with a driver of `Engine`:
discover BTC-5m markets (existing discovery), pair legs, assemble the recorded Feed + the
real-both-book sim Exchange + a sim Clock + the `CtxEnricher` (loading `snap062901`), run
the engine over the merged stream, collect per-market + portfolio results. The
`(token, window)` classifier is trivial for single-cell (`Token::Btc`, window = the 5-min
settlement bucket); the real cross-cell classifier (from market metadata, overlapping
windows) lands with the multi-cell turn-on. Existing CLI flags (champion command) map
onto the driver config.

### 5. Validation harness

- **Determinism (hard gate):** golden-trace on the real BTC-5m cache — run twice, assert
  byte-identical order/fill/equity trace.
- **Plausibility tripwire:** br2 trades roughly the known number of markets (~93/3561 in
  the May reference) with no crashes and a sane equity path. NOT a P&L match.
- **Both-book correction (report):** the P&L delta vs the old synthetic-NO May result,
  reported as a measured number — the headline output that motivates re-formulation.

## What changes in the codebase

- `pm-engine`: add the recorded Feed impl, the real-both-book sim Exchange, and the
  `CtxEnricher` (engine grows to hold spot/trade handles + a `ModelState` and compute
  regime/model/prior-range). All decision logic, consistent with lean-core.
- `pm-app`: the bespoke `run_backtest`/walk-forward loop is replaced by the thin driver.
  Leg-pairing is added to discovery/loading. Calibrator-training pass moves to the driver.
- `pm-types`/`pm-strategy`/`pm-model`/`pm-risk`: unchanged (the regime/model functions are
  reused, not rewritten).

## Open items / watchlist

- **Leg time-alignment policy:** forward-fill the counter-leg's latest snapshot at each
  event ts; confirm both legs' `book_snapshot_25` cadence and handle gaps. (Plan detail.)
- **Spot/trade history plumbing:** per-market swap vs a global handle the engine queries
  by ts. (Plan detail.)
- **Calibrator training fidelity:** reproduce the champion's inline training (`--meta-epochs`,
  `--min-train-markets 4500`) in the driver before the snapshot is trusted; for validation
  we load the frozen `snap062901` rather than re-train.
- **Carried from Phase 1 (still deferred):** reservation lifecycle, oversell guard, explicit
  `MarketClose{resolved_yes}` event + `MARKET_OPEN`→`close_ns`, `on_market_resolved` lacking
  a `MarketId`.

## Sequence

This (Phase 2, BTC-5m) → multi-cell turn-on (Feed k-way-merge across all 12 cells + real
`(token,window)` classifier) → br2 re-formulation cycle (re-sweep/re-train on faithful
backtests) → Phase 3 live driver → step-3 cross-market calibration + allocator. The
faithful backtest this delivers is the prerequisite for everything after it.
