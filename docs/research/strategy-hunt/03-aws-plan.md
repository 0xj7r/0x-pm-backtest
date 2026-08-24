# Strategy Hunt — AWS Full-History Plan

Date: 2026-06-16  
Bankroll: **$1,000**  
Validation window: **2026-02-12 → 2026-05-20** (Feb–May; HOLDOUT 2026-05-19+ stays sealed)  
Credentials: `AWS_PROFILE=visumlabs`  
Runbook: archived 2026-08-24, see [`docs/archive/2026-07/aws-backtest-runbook.md`](../../archive/2026-07/aws-backtest-runbook.md) (historical record; the cloud-runner design that replaces it is in the framework-reset spec)

## Scorecard status (VERIFY+TUNE not complete)

Local matrix output: `data/runs/strategy_hunt/` (see [`scorecard.md`](scorecard.md)).

| family | market | TUNE NET | VERIFY NET | VERIFY Sharpe | VERIFY worst day | verdict |
|---|---|---:|---:|---:|---:|---|
| F1 | fade / btc5m | — | +$11,841 | 46.7 | +$415 | **INCOMPLETE** (TUNE missing) |
| F2 | aligned / btc5m | — | −$1,025 | −5.9 | −$432 | **INCOMPLETE** / REJECT |
| F7 | hold / btc5m | — | +$12,079 | 22.3 | −$499 | **INCOMPLETE** (fails −5% worst-day gate) |

**No strategy is VIABLE yet.** Do **not** launch AWS promotion runs until TUNE completes and
`python3 scripts/score_strategy_hunt.py` prints `VIABLE`.

**Provisional top candidates** (pending TUNE):

1. **F1** (exo fade, `pm-app alpha`) — VERIFY all 12 days positive; needs TUNE confirmation.
2. **W3** (`bonereaper_v2` walk-forward) — separate lane; champion already validated on AWS at $1K
   (+$8,990 on 23,705 markets). Strategy-hunt matrix uses lighter sizing (2.5% clip).

---

## Launcher compatibility

| Family | Harness | `launch_ec2_portfolio_grid.sh` | Notes |
|---|---|---|---|
| F1–F7 | `pm-app alpha` | **No** — walk-forward only | No alpha EC2 launcher exists. Use local matrix or template below. |
| W1 | `back_to_explore` | **Yes** | `--strategies back_to_explore` |
| W2 | `paired_mm` | **Yes** | `--strategies paired_mm` |
| W3 | `bonereaper_v2` | **Yes** | Use this launcher (or `launch_ec2_fullhist_configB.sh` for champion TOML/arms). |

`launch_ec2_portfolio_grid.sh` **cannot** express alpha-equivalent configs (`--notional-usdc`,
`--edge-thresholds`, fade/aligned/hold flags). For bonereaper, map strategy-hunt sizing to
walk-forward flags:

| Protocol (alpha / matrix) | AWS walk-forward equivalent |
|---|---|
| `--notional-usdc 25` | `--clip-fraction-of-equity 0.025 --max-clip-usdc 30` |
| `--starting-cash 1000` | `--starting-cash 1000` |
| `--latency-ms 150` (alpha) | `--taker-latency-ms 150` (walk-forward; champion uses 500) |
| `--replay-sample-ms 1000` (matrix) | `--replay-sample-ms 1000` |

---

## Which strategies need AWS vs local cache

| Strategy | Need AWS for full Feb–May? | Local cache gap | Rationale |
|---|---|---|---|
| **F1 fade btc5m** | **Optional** | 5 tick days missing (2026-05-02…06); 7 tape load errors in VERIFY | Alpha is fast (~35 s / 3.5k markets). Finish **TUNE locally** first; AWS only if promoting after VIABLE or validating alt markets. |
| **F2 aligned** | No (REJECT path) | same 5 days | VERIFY already negative. |
| **F7 hold** | No | same | Fails worst-day gate (−$499 vs −$50 limit). |
| **F3–F6** | TBD | same 5 days + thin alt coverage | Not run yet in matrix. |
| **W3 bonereaper btc5m** | **Yes** | Local smoke only (`replay_sample_ms=1000`); full history ~23.7k markets | Meta-calibrator + portfolio replay is hours-scale; prior OOM on `c7i.4xlarge` without `max-concurrent-fetches 8`. |
| **W1 / W2** | **Yes** (if W-local passes) | Same as W3 | Same walk-forward engine. |
| **eth5m / sol5m alpha** | **Yes** if promoted | Identical 5-day gap + fewer manifest days | Cross-asset needs Telonex on EC2. |

Local tick inventory: 113 days (2025-11-26 → 2026-06-10). Feb-12–May-20 window: **93/98** manifest
days cached for btc/eth/sol 5m.

---

## Ready-to-run AWS commands ($1K)

**Do not execute until scorecard shows VIABLE.** Commands are documented for copy-paste.

### 1. W3 — Strategy-hunt matrix equivalent (bonereaper, 2.5% clip)

Single-cell grid (no sizing sweep). Trains meta-calibrator once, then runs one variant.

```bash
AWS_PROFILE=visumlabs \
INSTANCE_TYPE=r7g.4xlarge \
ARCH=arm64 \
USE_SPOT=1 \
ROOT_VOLUME_GB=250 \
./scripts/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 \
  --end-date 2026-05-20 \
  --train-markets 4500 \
  --meta-epochs 24 \
  --meta-learning-rate 0.04 \
  --meta-l2 0.001 \
  --meta-weight-clip 1.50 \
  --strategies bonereaper_v2 \
  --starting-cash 1000 \
  --max-clip 30 \
  --max-order-clip-multiplier 6.0 \
  --max-per-market-exposure-frac 0.25 \
  --gross-caps 250 \
  --clip-fractions 0.025 \
  --kelly 0.25 \
  --max-concurrent-fetches 8 \
  --replay-sample-ms 1000 \
  --portfolio-checkpoint-every-markets 250 \
  --label-suffix strategy-hunt-w3-1k
```

**Expected runtime:** 8–14 h (cache build + meta-train ~1–2 h + ~23k markets at 1 s replay sample).  
**Expected cost:** ~$2–5 Spot (`r7g.4xlarge`); ~$8–14 on-demand (`r7i.4xlarge`).

**Monitor:**

```bash
RUN_ID=<printed-at-launch>
AWS_PROFILE=visumlabs aws s3 ls s3://pm-research-backtest-prod/results/$RUN_ID/ --recursive
```

---

### 2. W3 — Champion bonereaper @ $1K (frozen meta, proven knobs)

Reuses the completed May-28 training run. No retrain. Matches
`configs/bonereaper_v2_favourite_062901.command.txt` (deleted in the
2026-08-23 configs purge; bonereaper_v2 is a removed legacy strategy)
(+899% on prior full history).

```bash
AWS_PROFILE=visumlabs \
INSTANCE_TYPE=r7g.4xlarge \
ARCH=arm64 \
USE_SPOT=1 \
ROOT_VOLUME_GB=250 \
./scripts/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 \
  --end-date 2026-05-20 \
  --reuse-artifacts-run-id 20260528T225810Z-portfolio-grid-52322 \
  --forbid-meta-training \
  --strategies bonereaper_v2 \
  --starting-cash 1000 \
  --max-clip 30 \
  --max-order-clip-multiplier 10 \
  --max-per-market-exposure-frac 0.12 \
  --gross-caps 250 \
  --clip-fractions 0.015 \
  --kelly 0.5 \
  --max-concurrent-fetches 8 \
  --replay-sample-ms 1000 \
  --taker-latency-ms 500 \
  --clip-drawdown-soft-pct 0.20 \
  --clip-drawdown-hard-pct 0.40 \
  --clip-drawdown-min-multiplier 0.10 \
  --br2-late-confirm-min-realized-vol-180s-bps 1.25 \
  --br2-late-confirm-max-observed-range 0.50 \
  --br2-high-skew-min-realized-vol-180s-bps 1.25 \
  --br2-late-favourite-high-cert-full-clip-edge 0.09 \
  --br2-late-favourite-min-model-edge 0.09 \
  --br2-late-favourite-high-cert-min-model-edge 0.06 \
  --br2-late-favourite-max-reversal-pressure 0.85 \
  --br2-late-favourite-max-observed-range 0.70 \
  --br2-late-favourite-range-soft-throttle 0.55 \
  --br2-late-favourite-range-hard-throttle 0.70 \
  --br2-late-favourite-range-extra-edge 0.08 \
  --br2-late-favourite-range-extra-confidence 0.12 \
  --br2-tail-max-clips 6 \
  --br2-tail-max-ask 0.08 \
  --portfolio-checkpoint-every-markets 250 \
  --label-suffix strategy-hunt-champion-1k
```

**Expected runtime:** 6–12 h (no meta-train; eval only).  
**Expected cost:** ~$1.50–4 Spot.

**Cost-saver** (unchanged Rust): add
`--pm-app-binary-s3-uri s3://pm-research-backtest-prod/artifacts/binaries/pm-app-al2023-x86_64-607c3156`
and drop `ARCH=arm64` (x86_64 only).

---

### 3. F1 fade — full Feb–May (alpha harness, **no EC2 launcher**)

**Gate:** run TUNE locally first:

```bash
caffeinate -dimsu &
WINDOWS=TUNE STRATEGIES=F1 MARKETS=btc5m ./scripts/strategy_hunt_matrix.sh
python3 scripts/score_strategy_hunt.py
```

If VIABLE, full-history validation is **fast locally** (~5–8 min for ~28k btc5m markets).
Prewarm missing tick days before launch:

```bash
./target/release/pm-app prep-cache \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
  --cache-dir data/cache \
  --spot-symbol BTCUSDT \
  --date-start 2026-02-12 \
  --date-end 2026-05-20 \
  --max-concurrent 32
```

Then re-run matrix on full TUNE+VERIFY windows locally, or use this **EC2 user-data template**
(until `scripts/launch_ec2_alpha.sh` exists — mirror `launch_ec2.sh` with `pm-app alpha`):

```bash
# TEMPLATE ONLY — build scripts/launch_ec2_alpha.sh before production use
AWS_PROFILE=visumlabs INSTANCE_TYPE=c7g.4xlarge ARCH=arm64 USE_SPOT=1 \
  ./scripts/launch_ec2.sh  # does NOT support alpha today; extend or run manually
```

**Manual alpha command** (on any EC2 runner with repo + cache):

```bash
PM_TELONEX_REGION=us-east-1 ./target/release/pm-app alpha \
  --local-cache-dir /opt/pm/cache \
  --markets /opt/pm/markets.jsonl \
  --slug-prefix btc-updown-5m- \
  --down-assets /opt/pm/manifests/canonical/down_all.jsonl \
  --tick-cache-dir /opt/pm/cache/ticks \
  --latency-ms 150 \
  --vol-lookback-s 3600 \
  --stop-before-close-s 90 \
  --fee-curve-rate 0.07 \
  --notional-usdc 25 \
  --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08 \
  --edge-thresholds 0.16 \
  --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 \
  --date-start 2026-02-12 --date-end 2026-05-20 \
  --out-json /opt/pm/results/f1_fade_fullhist.json \
  --trades-out /opt/pm/results/f1_fade_fullhist.trades.jsonl
```

**Expected runtime:** 30–90 min (mostly `prep-cache` for ~28k markets) + 5–10 min alpha replay.  
**Expected cost:** ~$0.50–2 Spot (`c7g.4xlarge`).

---

### 4. W2 paired_mm @ $1K (conditional on local matrix pass)

```bash
AWS_PROFILE=visumlabs \
INSTANCE_TYPE=r7g.4xlarge ARCH=arm64 USE_SPOT=1 ROOT_VOLUME_GB=250 \
./scripts/launch_ec2_portfolio_grid.sh \
  --start-date 2026-02-12 --end-date 2026-05-20 \
  --train-markets 4500 --meta-epochs 24 \
  --strategies paired_mm \
  --starting-cash 1000 \
  --max-clip 30 \
  --gross-caps 250 \
  --clip-fractions 0.025 \
  --max-per-market-exposure-frac 0.25 \
  --kelly 0.25 \
  --max-concurrent-fetches 8 \
  --replay-sample-ms 1000 \
  --label-suffix strategy-hunt-w2-1k
```

**Expected runtime / cost:** same order as W3 matrix run.

---

## Promotion checklist (after VIABLE)

1. `python3 scripts/score_strategy_hunt.py` → verdict `VIABLE` for TUNE **and** VERIFY.
2. One AWS full-history run (command above) — do **not** re-tune on Feb–May.
3. Check S3 `summary.json`: `compounded_return_pct`, `path_max_drawdown_pct`, `sharpe_ratio`,
   `by_fill_tag` lane attribution.
4. Exactly **one** HOLDOUT (2026-05-19 → 2026-05-28) locally; June remains sealed.
5. Stop rules per runbook: equity below start after 2k OOS markets, calibration regression, stale checkpoints.

---

## Immediate next steps (local, before AWS spend)

```bash
# Complete TUNE for provisional leaders
WINDOWS=TUNE STRATEGIES=F1,F7,W3 MARKETS=btc5m ./scripts/strategy_hunt_matrix.sh
python3 scripts/score_strategy_hunt.py

# Optional: finish VERIFY sweep
WINDOWS=VERIFY STRATEGIES=all MARKETS=btc5m,eth5m,sol5m,xrp5m ./scripts/strategy_hunt_matrix.sh
```

Only strategies with `verdict: VIABLE` advance to the AWS commands in sections 1–4.