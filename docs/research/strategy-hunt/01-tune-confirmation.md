# Strategy Hunt — TUNE Confirmation (2026-06-16)

Cross-window check after VERIFY completed for the initial btc5m screen. TUNE was run
**only** for strategy×market cells that were fee-net positive on VERIFY.

## Setup

| Parameter | Value |
|---|---|
| Bankroll | $1,000 |
| Clip | $25 (`--notional-usdc 25`) |
| Fees | `--fee-curve-rate 0.07` |
| Latency | 150 ms |
| TUNE window | 2026-02-12 → 2026-04-30 (78 trading days) |
| VERIFY window | 2026-05-07 → 2026-05-18 (12 trading days) |

Results: `data/runs/strategy_hunt/`

## VERIFY screen (completed)

VERIFY matrix finished with three btc5m alpha families (F1 fade, F2 aligned, F7
hold-to-resolution). Fee-net survivors forwarded to TUNE:

| Cell | VERIFY NET | Forward to TUNE? |
|---|---:|---|
| F1 × btc5m (fade) | **+$11,841** | yes |
| F2 × btc5m (aligned) | −$1,025 | no — fee-net negative |
| F7 × btc5m (hold) | **+$12,079** | yes |

Scored via `python3 scripts/score_strategy_hunt.py`.

## TUNE runs (survivors only)

```bash
WINDOWS=TUNE STRATEGIES=F1,F7 MARKETS=btc5m NOTIONAL=25 ./scripts/strategy_hunt_matrix.sh
```

Both jobs completed (~11 min wall time, 22,453 markets each).

## Cross-window scorecard

| family | market | TUNE NET | VERIFY NET | TUNE Sharpe | VERIFY Sharpe | VERIFY worst day | hit (TUNE / VERIFY) | verdict |
|---|---|---:|---:|---:|---:|---:|---|---|
| F1 | btc5m | +$33,334 | +$11,841 | 22.3 | 46.7 | +$415 | 61.2% / 63.0% | **VIABLE** |
| F7 | btc5m | +$53,671 | +$12,079 | 22.8 | 22.3 | −$499 | 61.3% / 56.6% | REJECT |

## Cross-window consistency

Scaling check: VERIFY NET should exceed `0.9 × TUNE NET × (VERIFY days / TUNE days)`.

| Cell | TUNE $/day | VERIFY $/day | VERIFY ÷ scaled-TUNE | Consistency gate | Worst-day gate (−5%) |
|---|---:|---:|---:|---|---|
| F1 × btc5m | $427 | $987 | **2.31×** expected | PASS (need >$4,616) | PASS (worst +$415) |
| F7 × btc5m | $688 | $1,007 | **1.46×** expected | PASS (need >$7,431) | **FAIL** (worst −$499) |

Observations:

1. **F1 fade** — Strong agreement across windows. VERIFY daily NET ($987) exceeds TUNE
   ($427) by 2.3× after day-count scaling. Per-trade edge rises from $2.45 (TUNE) to
   $3.72 (VERIFY). Hit rate stable (~61–63%). Every VERIFY day was profitable (worst day
   +$415). Meets all adoption gates.

2. **F7 hold** — Raw NET is higher on TUNE ($53.7k) than F1, and VERIFY NET ($12.1k) is
   comparable to F1. Cross-window *scaling* passes (1.46× expected). However VERIFY worst
   day (−$499) breaches the −5% bankroll floor (−$50). TUNE also shows a −$790 worst day,
   indicating tail risk is structural to hold-to-resolution, not a one-off VERIFY artifact.
   Hit rate drops to 56.6% on VERIFY (below 50% gate threshold for directional families,
   though F7 is not an aligned strategy).

3. **F2 aligned** — Fee-net negative on VERIFY (−$1,025, Sharpe −5.9). No TUNE run.
   Aligned continuation on btc5m 5m windows does not survive fees in the May VERIFY slice.

## Viable list (passes TUNE **and** VERIFY)

| Rank | Cell | TUNE NET | VERIFY NET | Notes |
|---:|---|---:|---:|---|
| 1 | **F1 × btc5m** (exo fade) | +$33,334 | +$11,841 | Holdout candidate |

**One cell viable.** F7 is NET-positive on both windows but fails worst-day risk gate.

## Holdout recommendation

Run exactly one HOLDOUT for the sole viable cell:

```bash
WINDOWS=HOLDOUT STRATEGIES=F1 MARKETS=btc5m NOTIONAL=25 ./scripts/strategy_hunt_matrix.sh
```

HOLDOUT window: 2026-05-19 → 2026-05-28 (sealed; no re-tuning).

## Next matrix work

VERIFY covered only 3/35 planned alpha cells (btc5m subset). Remaining VERIFY cells
(F3–F6, other markets, walk-forward W1–W3) still need to run before broader TUNE
confirmation. Any new VERIFY fee-net positives should get the same survivor-only TUNE
treatment.

## Disk note

TUNE trade logs are ~11 MB each. Delete `*.trades.jsonl` after scoring per protocol
once HOLDOUT trades are captured for F1.