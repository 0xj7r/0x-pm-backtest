# Framework reset: strategy-free core, constraints as code, cloud-first runs

Date: 2026-08-23
Status: approved design, pre-implementation
Decided with: Jack (reset depth: keep core / kill all strategies; cloud model:
ephemeral EC2 in us-east-1; data plan: Telonex Pro only; git: in-place hard cut
on main)

## 1. Why

The June 2026 live cycle lost ~$1,500 and the July rebuild froze on 2026-07-10
with its verdicts unrendered. The repo's own forensics (docs/deep-review-2026-07-10.md,
docs/WHY-LIVE-DIVERGED.md) established that the losses came from a latency-fragile
edge, a reimplemented live engine, and config/gate drift, not from the market.
The codebase is now ~50% dead strategy code around a healthy, equivalence-proven
core. This reset deletes every strategy, keeps and extracts the testing/execution
machinery, converts the July lessons from documents into enforced framework
behavior, and moves data and compute off the laptop.

End state: a repo where writing a new strategy means writing one decision module
plus one TOML, and where the framework is physically incapable of reporting the
kinds of numbers that misled us before.

## 2. What survives, what dies

### Survives (the core)

| Component | Content | Notes |
|---|---|---|
| pm-alpha | fair_value (BSM digital belief), vol, fee curve, ExoState feature plumbing, replay harness, equivalence machinery, metrics | strategy-specific decide logic removed; the equivalence *pattern* (one decision fn, proven identical across backtest/live) is kept and generalized |
| pm-types | market/tape/spot types | unchanged |
| pm-risk | Kelly/fractional sizing, PortfolioState | unchanged |
| pm-telonex-loader | S3/parquet loaders for books, trades, Binance | minus nautilus_conv.rs and all nautilus-* deps |
| pm-shadow | the live-twin pattern (log-only engine, JSONL stream, executor tail seam) | generalized over the Strategy trait instead of hardwiring the fade |
| pm-strategy | the Strategy trait + a slimmed Ctx + NoopStrategy | all six strategy impls deleted; Ctx fields that exist only for dead strategies removed |
| Engine logic inside walkforward.rs/runner.rs | event loop, latency-shifted fills, book walking, fee-net accounting, portfolio orchestration | extracted into a new pm-backtest crate; walkforward.rs itself is then deleted |
| scripts/pipeline | Telonex/Binance/Deribit fetchers, manifest builders | retargeted to write to S3, not local disk |
| scripts/ops parity stack | verify_deploy, parity monitors, fingerprint checks | kept; these are the drift gates |
| Docs keep-list | PROD.md, WHY-LIVE-DIVERGED.md, deep-review-2026-07-10.md, latency-truth, fill-model-calibration, realization-baseline-correction, both drawdown docs, strike-basis, chop-is-not-the-enemy, strategy-hunt/05-quant-signals.md, position-management-falsified, CONTEXT.md | plus a new docs/CONSTRAINTS.md distilling them (section 4) |

### Rescued before any branch deletion

1. `fix-1s-spot-backfill` commit d04bdd69: warm_spot_1s bootstrap-vol fix for
   pm-shadow. Cherry-pick to main.
2. From `cross-market-data-foundation`: scripts/telonex_ingest.py,
   scripts/launch_ec2_ingest.sh, docs/telonex-data-api.md,
   scripts/leg_synthesis_distortion.py (files only; never merge the branch,
   its main.rs changes are 290 commits stale).
3. Read worktree-agent-adf8a96's d92d08dc for the past-close resolution-marking
   idea (patch targets a deleted file; take the idea, not the diff).

### Dies

- All strategy implementations: exo_fade, bonereaper_v2, back_to_explore,
  paired_mm, convex/, signals.rs, archive/spot_momentum.rs, and the
  MayJuneFade config alias.
- crates/pm-engine (parallel abandoned engine) and pm-app/engine_driver.rs.
- crates/pm-model, crates/pm-copytrade.
- nautilus_conv.rs, the QuotesS3 subcommand, and all 15 nautilus-* workspace deps.
- walkforward.rs (6,530 LOC) and main.rs legacy CLI surface, after engine
  extraction.
- All merged branches (9 feature + 10 worktree-agent) and their worktrees;
  the two unmerged branches after rescue above.
- scripts/archive/ (160 files; git history keeps them).
- ~25 stale docs moved to docs/archive/2026-06/ and docs/archive/2026-07/;
  banners added to the actively-misleading ones (any doc quoting 250ms-era
  P&L, the 2026-06-19 handoff config block).
- Local data/: the ~42GB cache is deleted after confirming S3 has everything
  local-only (the Jul 1-10 books/trades and daily_replay outputs get synced up
  first). data/ becomes a gitignored bounded scratch dir.

"Kill" means delete from the working tree on main. Git history keeps everything
reachable; nothing is force-pushed or rewritten.

### Explicitly out of scope

- polymarket-agent changes, except one landmine fix: fast_live.rs:65 compiles
  in min_entry_ask 0.45 (proven -$459/June). That constant is removed or the
  bin gated off in a single small PR there.
- Any new strategy. The reset ends with zero strategies by design.
- Live/paper deployment infra beyond keeping pm-shadow and the parity scripts
  compiling. Re-deployment is a later project, gated by docs/PROD.md rules.

## 3. Target workspace shape

```
crates/
├── pm-types/        # unchanged
├── pm-risk/         # unchanged
├── pm-telonex-loader/  # nautilus-free
├── pm-alpha/        # belief, vol, fees, features, metrics, equivalence
├── pm-strategy/     # trait + slim Ctx + Noop (no strategies)
├── pm-backtest/     # NEW: engine extracted from walkforward.rs/runner.rs
│                    #   replay loop, fill model, accounting, scorecard, gates
├── pm-shadow/       # live twin, generalized over the trait
└── pm-app/          # thin CLI: backtest | sweep | prep | discover | equivalence
```

A strategy, when one exists again, is: one module implementing
`pm_strategy::Strategy`, one TOML profile, zero CLI flags. The CLI takes
`--strategy <name> --config <toml>` and nothing strategy-specific.

## 4. Constraints as code (docs/CONSTRAINTS.md + enforcement)

Each rule is enforced by the framework, not by discipline. The doc states the
rule, the evidence, and where the enforcement lives.

1. **Truthful latency.** Backtests run at 750ms and 1250ms. A run below 750ms
   requires `--fantasy`, and every output artifact (JSON summary, scorecard,
   filename) is watermarked `FANTASY`. Evidence: June +$17.7k@250ms was
   +$2.5k@1250ms with identical hit rate.
2. **Fee-net always.** The 0.07·p·(1-p) taker curve is on unconditionally;
   there is no flag to disable it. At-touch gross accounting does not exist in
   the Rust engine.
3. **Observer-noise stress.** The scorecard's headline is the spread over N
   jittered replays (perturbed decision timing/latency), not a single-tape
   point estimate. Near-threshold first-passage entries are two draws of a
   coin flip; a strategy whose P&L collapses under jitter is rejected by the
   report itself. Evidence: identical twins agreed 57.6% on sub-15s entries.
4. **Regime-window validation.** The standard scorecard runs all validated
   month windows (Feb, Mar, Apr, May, Jun 2026 initially; extended as data
   accrues) and reports per-window. Single-window results are labeled
   UNVALIDATED in the output.
5. **Sizing realism.** Reports are at fractional sizing on the live bankroll
   (currently ~$2,800), with the 0.82 realization haircut shown alongside raw,
   plus the ruin/floor check (5-share venue minimum, $550 floor logic).
6. **One config fingerprint.** The engine computes a fingerprint over the full
   resolved config (defaults included) and stamps it into every output and
   every shadow/live log line. Parity tooling compares fingerprints, not
   field-by-field prose. No clap defaults that differ from frozen defaults:
   a config struct has exactly one source of default values.
7. **No P&L circuit breakers, no predictive gates in the framework.** Sizing
   is the loss control. Gates a strategy wants are strategy code, subject to
   the same validation, never framework features.
8. **Strike basis discipline.** Belief and strike must come from the same
   price basis; the loader refuses mixed-basis configurations
   (binance_proxy is canonical; official strikes are resolution-verification
   only).

## 5. Cloud runtime

Goal: zero data and zero heavy compute on the Mac; near-zero idle cost.

- **Runner:** `scripts/cloud/run_backtest.sh` launches a spot instance
  (c7i/c7a family, us-east-1, same region as pm-research-data-prod), passes a
  cloud-init that clones the repo at a given ref, restores a cached release
  binary from `s3://pm-research-data-prod/build-cache/<commit>` (builds and
  uploads on cache miss), runs the requested backtest/sweep streaming data
  from S3, writes results + config fingerprint + manifest to
  `s3://pm-research-data-prod/experiments/<date>-<name>/`, and terminates
  itself. A hard self-terminate timer (default 4h) guards against orphaned
  instances regardless of job outcome.
- **Local feel:** `scripts/cloud/results.sh` lists/pulls experiment summaries;
  a run's stdout tail is mirrored to S3 so progress is visible without SSH.
- **Cost envelope:** spot c7i.2xlarge ≈ $0.15/h; typical run well under $1,
  heavy sweep a few dollars, idle cost zero. No always-on instances.
- **Engine data access:** pm-telonex-loader's S3 mode is the default; the
  local-cache mode remains for the instance's ephemeral disk (the runner
  pre-syncs the date range to instance storage when a sweep re-reads the same
  days repeatedly, which is cheaper than repeated S3 range reads).

## 6. Data plan (Telonex Pro only)

- Upgrade the Telonex key to Pro. Backfill 2026-07-01 through present for
  book_snapshot_25 and trades (BTC and ETH updown families, ~32GB+) directly into the S3
  mirror layout via the rescued telonex_ingest.py / launch_ec2_ingest.sh
  running in-region (an ephemeral instance, same pattern as section 5, so the
  laptop never holds the data).
- Refill the free gaps in the same pass: Binance spot/perp agg_trades and
  klines (data.binance.vision), Deribit DVOL, from their last dates to present,
  writing to S3.
- Ongoing: a scheduled ephemeral job (EventBridge-triggered or manually run
  weekly) tops up the mirror from Telonex. Telonex is the ongoing source;
  no self-hosted recorder for now (accepted risk: vendor quota/retention).
- One-time: sync local-only data up before deleting data/ (Jul 1-10 raw days,
  daily_replay outputs, golden/research/signals files).

## 7. Testing and verification

- Every deletion phase must leave the workspace green: `cargo build --release`,
  `cargo test --workspace`, and the equivalence gate binary passing.
- Engine extraction is verified by a pinned-tape regression: one recorded day
  replayed through the old walkforward path (pre-deletion commit) and the new
  pm-backtest crate must produce identical fills and P&L to the cent before
  walkforward.rs is deleted. The comparison artifact is committed.
- The generalized pm-shadow is verified the same way: byte-identical decision
  stream on a recorded tape versus the pre-refactor binary running NoopStrategy
  and a test strategy.
- Cloud runner verified end-to-end: one real backtest launched, results in S3,
  instance confirmed terminated, cost recorded in the run manifest.
- CONSTRAINTS enforcement gets unit tests (fantasy watermark present below
  750ms, fee curve not disableable, fingerprint stable across CLI/TOML paths,
  mixed-basis refusal).

## 8. Phasing (each phase is a separate commit series, verified before the next)

1. **Rescue + hygiene:** cherry-pick d04bdd69; copy the four files from
   cross-market-data-foundation; delete merged branches + worktrees; remove
   scripts/archive, pycache, stray root files.
2. **Dead-crate removal:** pm-engine, pm-copytrade, pm-model (after tracing its
   6 call sites), nautilus tree, legacy strategies except exo_fade (which is
   the extraction reference), doc triage + banners.
3. **Engine extraction:** pm-backtest crate carved out of walkforward.rs/
   runner.rs with the pinned-tape regression; exo_fade temporarily retained as
   the regression strategy.
4. **Constraint enforcement:** truthful-latency default + watermark, jittered
   replay, scorecard, fingerprint, CONSTRAINTS.md.
5. **The kill:** delete exo_fade and MayJuneFade, slim Ctx, generalize
   pm-shadow, thin the CLI. Zero strategies remain; NoopStrategy keeps the
   pipeline testable.
6. **Cloud runtime + data:** S3 sync-up of local-only data, cloud runner,
   Telonex Pro backfill jobs, delete local data/, retarget pipeline scripts.
   (6a data sync/backfill can start any time after phase 1; the runner script
   depends on the phase 3-5 CLI.)
7. **polymarket-agent landmine:** remove the compiled-in min_entry_ask from
   fast_live (single small PR in that repo).

Risks called out: walkforward extraction is the only genuinely risky step
(mitigated by the pinned-tape regression and doing it before deleting the
reference implementation); Telonex retention for July is unverified until Pro
access exists (checked as the first act after upgrade, before deleting any
local raw data); pm-model's live/dead status is verified by call-site trace
before deletion.
