# Open questions register

Unresolved research questions carried out of the June 2026 live-trading failure and the July
rebuild that froze on 2026-07-10 before they could be answered. This is the canonical list;
doc-triage banners elsewhere point back here rather than duplicating the detail.

## 1. Cheap-underdog realization (never answered)

`min_entry_ask 0.45` blocks entries that buy the cheap underdog (ask 0.30-0.45, the side the
market prices as unlikely while the model says otherwise). On June 13-30 backtest data
(truthful 1250ms latency, fee-net, $50 clips), adding that gate alone turns the bare config's
+$1,957 into -$149, a -$2,106 swing (`docs/archive/2026-07/deployed-config-negative-2026-07.md`). The gate was
added on a theory that these trades lose live even though they win in backtest, but neither
live stream running at the time (shadow-final, fast_live) ever took them, so the theory was
never tested against data.

A shadow stream with `min_entry_ask=0` (letting the cheap-underdog entries run live) was
deployed 2026-07-09 specifically to measure this. The repo froze the next day, 2026-07-10,
before the comparison against matched-config replay could be run. The Dublin EC2 box that ran
this stream (`i-0e1d441131c50103c`) has since been terminated (verified 2026-08-23), so no
further live data will accrue from it.

**What would answer it:** the Jul 1-12 shadow logs surviving in
`s3://pm-research-data-prod/shadow/pm-alpha/dublin/` may contain enough of the no-gate stream's
live outcomes to compare against the matched-config replay described in
`docs/archive/2026-07/deployed-config-negative-2026-07.md`. Pull that data and run the comparison before
deciding whether to drop `min_entry_ask` in any future deployment.

## 2. v1 stability-gate verdict (never rendered)

The v1 stability gate (`min_secs_from_open=15`, `max_p_side=0.85`) was pre-registered in
`docs/archive/2026-07/stability-gate-preregistration-2026-07.md` with four frozen pass criteria, to be judged
on `twin_agreement_report.jsonl` and `realization.jsonl` after 7 non-Saturday soak days:

1. Twin agreement on the gated subset >= 75% (week aggregate), at least 12 points above the
   ungated remainder.
2. Realization ratio of the gated subset vs same-day replay >= 0.85 (week aggregate).
3. Gated-subset at-touch P&L positive for the week.
4. Gated subset takes >= 15% of baseline entries (sanity check against a gate that trades
   nothing).

The soak clock restarted 2026-07-08 after a config-consistency reset (the prior week's data
was invalidated by a `stop_before_close_s` drift), putting the earliest legitimate judgment
date at ~2026-07-16. The repo froze 2026-07-10, six days before that date, so the verdict was
never rendered.

**What would answer it:** if the Dublin soak kept running past the freeze, the same S3 shadow
archive (`s3://pm-research-data-prod/shadow/pm-alpha/dublin/`) may hold `twin_agreement_report.jsonl`
and `realization.jsonl` covering enough of Jul 8-16 to evaluate the four criteria as originally
specified. If the data doesn't reach 7 full soak days, the gate needs a fresh soak window
before it can be judged at all.

## 3. Past-close resolution marking (idea from d92d08dc)

Commit `d92d08dc` (2026-06-10) implemented resolution marking for pm-app shadow: entries fired
in the last ~30s of a window get a `WOULD_EXIT` with a null book mark, because the timed exit
lands after the market has closed and the book is gone. The patch polled
`polymarket.com/api/crypto/crypto-price` every 3s (giving up at 120s) until the window
resolved, then logged the settled outcome instead of leaving the mark null.

The idea is sound but the patch targeted `crates/pm-app/src/shadow.rs`, which no longer exists
in this repo. The idea itself, marking a late-exit entry to its actual resolution rather than
exiting at a stale (or absent) quote, was never re-implemented against the current codebase.

**What would answer it:** re-evaluate the idea against `pm-backtest` as part of Plan 2 (the
next implementation plan): does the backtest/live exit model currently produce null or stale
marks for entries near window close, and if so, port the resolution-polling approach onto
whatever now owns exit handling.

## 4. Both-sides hold / sell-loser (queued idea)

When sequential fades hold both sides of a market (a whipsaw pattern where the combined entry
cost across both legs stays below 1, i.e. structurally locked in arbitrage), the current
handling is to hold both legs to redemption and let the losing leg settle at zero. The queued
alternative: on a re-reversal, sell the now-out-of-the-money losing leg to recover some
residual value instead of redeeming it at zero.

This was never backtested; it sat as a memory note (`both-sides-hold-sell-loser-idea`) rather
than a scoped experiment.

**What would answer it:** a backtest comparing hold-to-redemption against sell-on-re-reversal
for the subset of trades where both-sides-hold actually occurs, quantifying how often a
re-reversal happens and whether the recovered residual exceeds the transaction/spread cost of
exiting the losing leg early.

## 5. pm-model removal: ANSWERED 2026-08-24 (it stays; the coupling was not the strategies)

**Resolved.** The phase-2 strategy kill removed every strategy that read the canonical model,
which was the premise for expecting `pm-model` to fall out. It did not. The traced answer:
`pm-model` is not strategy-coupled at all, and is not removable without changing engine output.

What was re-traced after `exo_fade` and `MayJuneFade` were deleted
(`crates/pm-strategy/src/exo_fade.rs`, `regime.rs`, the `StratId` variants), leaving `StratId =
{Noop, Fixture}`:

- The `Ctx.model_output` / `Ctx.model_attribution` fields WERE removable and are gone (they were
  write-only workspace-wide; see the phase-2 Ctx slim).
- What remains is not strategy plumbing. `pm-model` backs the meta-calibration walk-forward layer
  (`OnlineMetaCalibrator`, `MetaTrainingSample`, `MetaTrainingConfig`, `MetaTrainingStats`,
  `MetaFeatureWeight` in `crates/pm-backtest/src/{engine,scorecard,accounting,portfolio,config}.rs`),
  the model gate (`enforce_model_gate` and its rejection counters), and the ~40-column decision-log
  feature stack (`crates/pm-backtest/src/fills.rs::DecisionLogRow`). `crates/pm-app/src/main.rs`
  carries the meta CLI surface. None of that is specific to a strategy: it is the research layer
  the engine emits for offline analysis.
- `Strategy::on_event_scored` still returns `Option<pm_model::ModelOutput>` and keeps
  `pm-strategy -> pm-model`. No shipped strategy overrides it; it survives because it is the seam
  six `pm_backtest::fills` tests use to drive a controlled `ModelOutput` through the model gate and
  the decision log. Removing it would delete that coverage with nothing to replace it.

**Why it cannot simply be deleted:** the walk-forward summary JSON carries `meta_calibration`, the
`run_config.shared` meta/model keys, `per_strategy.model_fill_quality`, and the per-market
`orders_rejected_model_gate*` counters. Deleting the eval wholesale changes that shape, which would
diverge the pinned golden anchor (`tests/golden/day-2026-06-25-fixture.sha256`). The anchor is not
re-recordable to accommodate a refactor.

**If someone still wants it gone,** it is a scoped piece of work in its own right, not a byproduct
of removing strategies: retire the meta-calibration research layer and the decision-log feature
stack as a deliberate, separately-reviewed behavior change, re-anchor the golden in that same
change, and only then drop the crate.
