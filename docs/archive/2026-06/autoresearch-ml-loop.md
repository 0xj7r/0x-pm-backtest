# Autoresearch ML Loop

This repo should run ML research as a fixed-budget loop around the existing
`pm-app walk-forward` and AWS portfolio-grid outputs. The loop is intentionally
small: one candidate change per attempt, one primary promotion gate, and no
manual cherry-picking from PnL-only wins.

## Goal

Improve out-of-sample model calibration before using PnL as evidence. A
candidate is only interesting if the walk-forward `summary.json` shows better
OOS calibrated log loss and no Brier regression. PnL, drawdown, Sharpe, and
fill-tag attribution are secondary diagnostics after the ML gate passes.

## Inputs

- Baseline `summary.json` from the current best walk-forward or AWS profile.
- Candidate `summary.json` from the exact same market set, strategy set, sizing,
  and fill assumptions.
- Optional frozen model artifacts from an AWS run:
  - `artifacts/meta-training-samples.json`
  - `artifacts/meta-calibrator-snapshot.json`

The relevant summary fields already exist:

```text
meta_calibration.oos.calibrated_log_loss
meta_calibration.oos.calibrated_brier
per_strategy.<strategy>.compounded_return_pct
per_strategy.<strategy>.path_max_drawdown_pct
per_strategy.<strategy>.sharpe_ratio
per_strategy.<strategy>.by_fill_tag
```

## Fixed-Budget Loop

Use a budget of 3 to 6 experiments per research pass.

1. Record the baseline summary and git SHA.
2. Write one candidate hypothesis with the smallest possible code/config delta.
3. Run the same walk-forward command as the baseline.
4. Compare only the candidate summary against the baseline summary.
5. Keep the candidate only if the ML gate passes.
6. Stop when the budget is spent or when the best candidate is good enough to
   hand to the main agent for a larger AWS validation.

Do not change market windows, strategy set, capital, fee/rebate assumptions, or
local-vs-S3 data mode while comparing candidates. Those are separate
experiments.

## Promotion Gate

Primary gate:

```text
candidate.meta_calibration.oos.calibrated_log_loss
  < baseline.meta_calibration.oos.calibrated_log_loss

candidate.meta_calibration.oos.calibrated_brier
  <= baseline.meta_calibration.oos.calibrated_brier + tolerance
```

Default tolerance is `0.0`. If the sample is noisy, set an explicit tolerance in
the script environment, for example `AUTORESEARCH_BRIER_TOL=0.0002`.

Secondary checks, only after the primary gate passes:

- Compounded return should not obviously deteriorate.
- Path max drawdown should stay inside the intended envelope.
- Sharpe and fill-tag attribution should explain where the return comes from.
- No candidate is promoted from total PnL alone.

## Local Command Template

Build once:

```bash
cargo build --release -p pm-app
```

Run a baseline or candidate with identical inputs:

```bash
PM_TELONEX_REGION=us-east-1 ./target/release/pm-app walk-forward \
  --markets /tmp/markets.jsonl \
  --strategies bonereaper_v2 \
  --starting-cash 1000 \
  --max-clip-usdc 100 \
  --max-order-clip-multiplier 6.0 \
  --max-per-market-exposure-usdc 500 \
  --kelly-fraction 0.25 \
  --spot-symbol BTCUSDT \
  --use-outcome-label \
  --portfolio-mode \
  --clip-fraction-of-equity 0.02 \
  --min-train-markets 4500 \
  --meta-epochs 24 \
  --meta-learning-rate 0.04 \
  --meta-l2 0.001 \
  --meta-weight-clip 1.50 \
  --max-concurrent-fetches 32 \
  --out-markets /tmp/autoresearch-candidate/markets.jsonl \
  --out-summary /tmp/autoresearch-candidate/summary.json
```

Use `--local-cache-dir <dir>` only when the cache is already prepared and disk
headroom is sufficient. Local runs are smoke tests unless they cover the same
historical window as the target AWS run.

## AWS Output Commands

Fetch a baseline summary:

```bash
AWS_PROFILE=visumlabs aws s3 cp \
  s3://pm-research-backtest-prod/results/<RUN_ID>/<PROFILE>/summary.json \
  /tmp/autoresearch-baseline-summary.json
```

Inspect the ML and portfolio metrics:

```bash
jq '{
  oos: .meta_calibration.oos,
  bonereaper_v2: .per_strategy.bonereaper_v2
}' /tmp/autoresearch-baseline-summary.json
```

Launch a larger candidate once a local/small gate passes:

```bash
AWS_PROFILE=visumlabs ./scripts/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 \
  --end-date 2026-05-20 \
  --train-markets 4500 \
  --strategies bonereaper_v2 \
  --starting-cash 1000 \
  --max-clip 100 \
  --max-order-clip-multiplier 6.0 \
  --gross-caps 500 \
  --clip-fractions 0.02 \
  --kelly 0.25 \
  --max-concurrent-fetches 32 \
  --portfolio-checkpoint-every-markets 250
```

For sizing/execution-only sweeps, reuse a frozen model from a previous AWS run
instead of retraining:

```bash
AWS_PROFILE=visumlabs ./scripts/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 \
  --end-date 2026-05-20 \
  --reuse-artifacts-run-id 20260528T103440Z-portfolio-grid-4432 \
  --forbid-meta-training \
  --strategies bonereaper_v2 \
  --starting-cash 1000 \
  --gross-caps 500 \
  --clip-fractions 0.02
```

## Git Keep/Rollback Semantics

Run experiments in isolated git worktrees. The active repo may have concurrent
edits from the main agent; do not reset or restore this checkout.

- `PASS`: leave the worktree in place, record the candidate command and summary,
  and hand the worktree path to the main agent for review.
- `FAIL`: remove the isolated worktree. The candidate is rolled back by deleting
  the worktree, not by touching the active checkout.
- `ERROR`: remove the isolated worktree unless debugging was explicitly
  requested.

If a candidate needs a code patch, the candidate command should apply that patch
inside the isolated worktree before running `pm-app`. Keep the patch minimal and
reviewable.

## Skeleton Harness

The helper is safe by default:

```bash
# Print loop semantics and examples.
./scripts/autoresearch_ml_loop.sh plan

# Compare two completed summaries.
./scripts/autoresearch_ml_loop.sh gate \
  /tmp/autoresearch-baseline-summary.json \
  /tmp/autoresearch-candidate/summary.json \
  bonereaper_v2

# Dry-run a fixed-budget command file.
./scripts/autoresearch_ml_loop.sh run \
  --budget 3 \
  --baseline /tmp/autoresearch-baseline-summary.json \
  --commands /tmp/autoresearch-candidates.txt \
  --strategy bonereaper_v2

# Actually run the same budget in detached worktrees.
./scripts/autoresearch_ml_loop.sh run \
  --budget 3 \
  --baseline /tmp/autoresearch-baseline-summary.json \
  --commands /tmp/autoresearch-candidates.txt \
  --strategy bonereaper_v2 \
  --execute
```

Each non-comment line in the command file is run from an isolated worktree with
these environment variables:

```text
AUTORESEARCH_LABEL
AUTORESEARCH_OUT_DIR
AUTORESEARCH_CANDIDATE_SUMMARY
```

The command must write its summary to `$AUTORESEARCH_CANDIDATE_SUMMARY`.

Example command-file line:

```bash
cargo build --release -p pm-app && PM_TELONEX_REGION=us-east-1 ./target/release/pm-app walk-forward --markets /tmp/markets.jsonl --strategies bonereaper_v2 --starting-cash 1000 --max-clip-usdc 100 --max-order-clip-multiplier 6.0 --max-per-market-exposure-usdc 500 --kelly-fraction 0.25 --spot-symbol BTCUSDT --use-outcome-label --portfolio-mode --clip-fraction-of-equity 0.02 --min-train-markets 4500 --meta-epochs 20 --meta-learning-rate 0.03 --meta-l2 0.002 --meta-weight-clip 1.25 --max-concurrent-fetches 32 --out-markets "$AUTORESEARCH_OUT_DIR/markets.jsonl" --out-summary "$AUTORESEARCH_CANDIDATE_SUMMARY"
```
