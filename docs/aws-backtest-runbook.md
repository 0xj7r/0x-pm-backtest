# AWS backtest runbook

This repo should treat local runs as smoke tests only. Full training, sizing
grids, and long walk-forward runs belong on AWS against S3 data.

## Keep the Mac awake

```bash
caffeinate -dimsu
```

Only active in-flight strategy profiles are kept at repo root in `configs/`.
Historical Bonreaper V2 profiles are intentionally archived under
`configs/archive/2026-06-02` and should be referenced explicitly when replaying
legacy experiments.

The current long-running local keep-awake process can be checked with:

```bash
ps -ef | rg '[c]affeinate -dimsu'
```

## Launch a portfolio grid

Use `$1,000` starting capital and clip fractions of equity. `max-clip-usdc`
is a safety ceiling; `clip-fraction-of-equity` is the primary compounding
control.

## Fast local research vs fidelity runs

Use `configs/archive/2026-06-02/bonereaper_v2_fast_research.toml` for local iteration and broad
AWS grids where the goal is ranking variants quickly. It sets
`replay_sample_ms = 1000`, preserving the first/last event and one latest book
event per second. On the local 2026-05-14 BTC 5m day, raw replay took 581s and
ran 15.3M BonereaperV2 gate checks. The sampled profile reduced the same day
to tens of thousands of gate checks; wall-clock time varies heavily with local
machine load, so use AWS for reliable grid timing.

Use `configs/archive/2026-06-02/bonereaper_v2_leader.toml` or `--replay-sample-ms 0` for final
high-fidelity validation. Do not compare raw PnL and sampled PnL as identical
execution evidence; use sampled runs to rank candidates, then promote only
after raw replay confirms the lane attribution and drawdown profile.

Example local one-day fast research run:

```bash
target/fast/pm-app walk-forward \
  --markets /private/tmp/markets-2026-05-14-fixed-input.jsonl \
  --local-cache-dir data/cache \
  --profile configs/archive/2026-06-02/bonereaper_v2_fast_research.toml \
  --strategies bonereaper_v2 \
  --portfolio-mode \
  --use-outcome-label \
  --starting-cash 1000 \
  --clip-fraction-of-equity 0.03 \
  --max-clip-usdc 30 \
  --max-order-clip-multiplier 10 \
  --max-per-market-exposure-frac 0.25 \
  --min-train-markets 30 \
  --meta-epochs 10 \
  --out-markets /tmp/pm-fast/markets.jsonl \
  --out-summary /tmp/pm-fast/summary.json
```

```bash
AWS_PROFILE=visumlabs \
INSTANCE_TYPE=c7i.4xlarge \
ROOT_VOLUME_GB=250 \
./scripts/pipeline/ec2/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 \
  --end-date 2026-05-20 \
  --train-markets 4500 \
  --meta-epochs 24 \
  --meta-learning-rate 0.04 \
  --meta-l2 0.001 \
  --meta-weight-clip 1.50 \
  --strategies bonereaper_v2 \
  --starting-cash 1000 \
  --max-clip 100 \
  --max-order-clip-multiplier 6.0 \
  --gross-caps 250,500,750 \
  --clip-fractions 0.015,0.02,0.03 \
  --kelly 0.25 \
  --max-concurrent-fetches 32 \
  --br2-late-favourite-max-ask 0.93 \
  --br2-tail-min-ask 0.01 \
  --br2-tail-max-ask 0.10 \
  --br2-tail-min-skew-step 0.02 \
  --br2-tail-budget-favourite-spend-frac 0.05 \
  --br2-tail-budget-favourite-upside-frac 0.25 \
  --portfolio-checkpoint-every-markets 250
```

The launcher now writes partial `markets.jsonl` and `summary.json` every 250
evaluated markets and syncs them to S3 every 180 seconds while each variant is
running.

When prewarming the local Telonex cache, keep `--prep-cache-max-concurrent`
moderate. The launcher defaults to `64`; higher values can trigger S3 503
retries and make parallel runners slower rather than faster.

Use `--label-suffix` whenever two sweeps share the same clip/gross/latency
settings but differ in strategy gates. The suffix is appended to the S3 result
directory, which keeps full-history comparisons readable:

```bash
--label-suffix vol1p0
```

The EC2 launcher intentionally does not pass `--profile` through to
`pm-app walk-forward`. Profile files are useful for direct local named-variant
runs, but `pm-app` applies profile values after CLI defaults. For AWS sweeps,
the launcher keeps CLI knobs authoritative and passes `--replay-sample-ms`
explicitly.

The meta-calibrator is validation-gated. The first variant trains on the
configured train window, holds out the last 20% of training samples for
chronological validation, and only writes a non-empty snapshot if calibrated
log loss improves and Brier score does not regress. For overfit runs, use
lower epochs, lower learning rate, higher L2, or lower weight clip.

Do not retrain the meta-calibrator for every sizing or execution sweep. After
one training run has uploaded artifacts, reuse them:

```bash
AWS_PROFILE=visumlabs ./scripts/pipeline/ec2/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 \
  --end-date 2026-05-20 \
  --reuse-artifacts-run-id 20260528T103440Z-portfolio-grid-4432 \
  --forbid-meta-training \
  --clip-fractions 0.015,0.02 \
  --gross-caps 250,500
```

This expands to the prior run's
`artifacts/meta-calibrator-snapshot.json` and
`artifacts/meta-training-samples.json`. Use a fresh training run only when the
training window, model features, or meta-calibrator hyperparameters changed.
For sizing, fill, latency, drawdown, and strategy-rule sweeps, pass
`--forbid-meta-training`; the launcher will fail before creating an instance
unless a frozen snapshot is supplied.

For fast follow-up sweeps that do not change Rust code, reuse a prebuilt Linux
`pm-app` binary to avoid spending several minutes compiling on every EC2
instance:

```bash
--pm-app-binary-s3-uri s3://pm-research-backtest-prod/artifacts/binaries/pm-app-al2023-x86_64-607c3156
```

Only reuse a binary when the Rust crates are unchanged from the binary's source
revision. Strategy-rule and sizing sweeps are fine; model or execution code
changes require a fresh build and a new uploaded binary.

## Cost controls

Default launches are still on-demand x86_64 for compatibility, but interruptible
research sweeps should normally use Spot. The launcher writes partial checkpoints
every 250 markets and syncs them every 180 seconds, so a Spot interruption loses
bounded work rather than the whole run.

Large on-demand runners now require an explicit acknowledgement. If a
`*.4xlarge`, `*.8xlarge`, or larger launch really must be on-demand, pass
`--allow-on-demand-large` or set `ALLOW_ON_DEMAND_LARGE=1`; otherwise use Spot
for research.

Use a memory-safe Graviton runner when the run does not depend on a prebuilt
x86_64 binary:

```bash
AWS_PROFILE=visumlabs \
INSTANCE_TYPE=r7g.4xlarge \
ARCH=arm64 \
USE_SPOT=1 \
./scripts/pipeline/ec2/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 \
  --end-date 2026-05-20 \
  --reuse-artifacts-run-id 20260528T225810Z-portfolio-grid-52322 \
  --forbid-meta-training \
  --clip-fractions 0.015 \
  --gross-caps 250 \
  --max-concurrent-fetches 8
```

Do not pass the existing `pm-app-al2023-x86_64-*` binary URI with `ARCH=arm64`;
let the instance build natively, or upload a separate ARM binary artifact. For
short CPU-bound smoke/ranking jobs, test `c7g.4xlarge`; for the full-history
BTC5m profile that previously OOM-killed around half history, prefer `r7g.4xlarge`
or another 128 GiB class until memory is reduced in the engine.

For repeated sweeps at the same source revision, combine frozen artifacts,
`--pm-app-binary-s3-uri`, and `--no-source-sync` so each launch does not delete
and re-upload the repo source tree:

```bash
--reuse-artifacts-run-id <training-run-id> \
--forbid-meta-training \
--pm-app-binary-s3-uri s3://pm-research-backtest-prod/artifacts/binaries/<arch-specific-pm-app> \
--no-source-sync
```

For drawdown-controlled sweeps, avoid a permanent hard freeze unless that is
the explicit test. `--clip-drawdown-min-multiplier` keeps a small recovery lane
open after the hard threshold:

```bash
--clip-drawdown-soft-pct 0.20 \
--clip-drawdown-hard-pct 0.40 \
--clip-drawdown-min-multiplier 0.10
```

## Tune late favourite loading

These flags control Bonereaper v2's late/favourite lanes without code edits:

```bash
--br2-late-clip-frac 1.0
--br2-late-max-fires 3
--br2-high-skew-clip-frac 0.60
--br2-high-skew-max-clips 5
--br2-late-favourite-threshold 0.22
--br2-late-favourite-min-ask 0.70
--br2-late-favourite-max-ask 0.93
--br2-late-favourite-clip-frac 1.00
--br2-late-favourite-max-clips 12
--br2-late-favourite-sweep-depth 7
--br2-late-favourite-min-model-confidence 0.68
--br2-late-favourite-min-model-side-p 0.62
--br2-late-favourite-min-model-edge 0.00
--model-gate-min-edge 0.00
--br2-tail-min-ask 0.01
--br2-tail-max-ask 0.10
--br2-tail-min-skew-step 0.02
--br2-tail-budget-favourite-spend-frac 0.05
--br2-tail-budget-favourite-upside-frac 0.25
```

Late-favourite loading is now a high-price favourite ladder, not a near-mid
single clip. The strategy requires a native favourite ask of at least 70c and
can enforce a native maximum ask cap so backtests do not silently fill above
the intended ladder price. Cheap-tail buys use the same limit-aware execution
path and should be capped explicitly.

Depth scales by price band: 70-75c = 1 level, 75-80c = 2, 80-90c = 3, 90c+ =
4, and 90c+ in the final 120s = 5. For 90c+ high-cert favourites, ML
side-support can carry the entry even when short-term spot/composite alignment
is stale. Cheap-tail buys are only allowed after a late-favourite anchor
exists, with spend capped by a fraction of favourite spend and remaining
favourite upside.

The first sizing sweep should compare conservative vs heavier late-favourite
loading with the same frozen meta-calibrator snapshot:

```bash
# Conservative gate
--br2-late-favourite-clip-frac 1.25 \
--br2-late-favourite-max-clips 8 \
--br2-late-favourite-min-model-confidence 0.72 \
--br2-late-favourite-min-model-side-p 0.74

# Heavier gate
--br2-late-favourite-clip-frac 1.50 \
--br2-late-favourite-max-clips 16 \
--br2-late-favourite-min-model-confidence 0.70 \
--br2-late-favourite-min-model-side-p 0.72
```

## Monitor a run

```bash
RUN_ID=20260524T063135Z-portfolio-grid-87192

AWS_PROFILE=visumlabs aws s3 ls \
  s3://pm-research-backtest-prod/results/$RUN_ID/ --recursive

AWS_PROFILE=visumlabs aws ec2 describe-instances \
  --region us-east-1 \
  --filters "Name=tag:run_id,Values=$RUN_ID" \
  --query 'Reservations[].Instances[].{id:InstanceId,state:State.Name,type:InstanceType,ip:PublicIpAddress}' \
  --output table
```

If SSH is needed, temporarily authorize this Mac's current IP on the runner
security group, inspect the log, then revoke the rule.

## Result checks

For each completed profile:

```bash
AWS_PROFILE=visumlabs aws s3 cp \
  s3://pm-research-backtest-prod/results/$RUN_ID/clip_0p003_gross_500/summary.json - \
  | jq '{
      markets_attempted,
      markets_succeeded,
      meta_calibration,
      bonereaper_v2: .per_strategy.bonereaper_v2
    }'
```

Do not judge a strategy from total PnL alone. Check:

- `compounded_return_pct`
- `path_max_drawdown_pct`
- `sharpe_ratio`
- `markets_with_orders`
- `by_fill_tag` lane attribution
- `meta_calibration.oos.calibrated_log_loss`
- `meta_calibration.oos.calibrated_ece`
- `meta_calibration.oos.calibration_bins`

## Stop rules

Stop or supersede a run when:

- the first profile is below starting equity after more than 2,000 OOS markets;
- path drawdown is larger than the intended production drawdown envelope;
- OOS calibrated log loss is worse than base log loss by more than noise;
- S3 checkpoint timestamps stop changing while the EC2 process is still alive;
- ECS backfill finishes and materially expands market count, making the run's
  dataset stale.

Do not promote a profile without a larger historical sample, OOS ML calibration
evidence, and fill-tag attribution showing which execution lane is producing
the return.
