# June 2026 backfill: canonical harness verdict

**Date:** 2026-07-01
**Data:** Telonex books/trades re-ingested Jun 18-30 (fresh markets parquet; the
prior gap was a stale local parquet, not missing data at Telonex). Raw days
also synced to the S3 mirror. Binance spot/perp fetched. Canonical manifest
extended to 51,451 rows (288 markets/day through Jun 30).
**Accounting:** canonical per docs/fill-model-calibration-2026-07.md: pm-app
alpha harness, --fee-curve-rate 0.07 --latency-ms 250, $50 clips, frozen prod
config (docs/PROD.md). Runs: data/runs/june_full_daily (vol 3600),
data/runs/june_1800_daily (vol 1800).

## Frozen config (realized/3600), June 13-30, fee-net

| date | trades | net $ | hit % |
|---|---|---|---|
| Jun 14 | 374 | +1,708 | 61.0 |
| Jun 15 | 236 | +2,460 | 66.5 |
| Jun 16 | 233 | +1,223 | 65.2 |
| Jun 17 | 285 | +252 | 54.4 |
| Jun 18 | 276 | +2,764 | 69.9 |
| Jun 19 | 225 | +320 | 62.2 |
| Jun 21 | 300 | +863 | 61.7 |
| Jun 22 | 233 | +1,386 | 66.5 |
| Jun 23 | 265 | +830 | 64.9 |
| Jun 24 | 222 | +335 | 59.5 |
| Jun 25 | 301 | +1,040 | 61.5 |
| Jun 26 | 179 | +22 | 60.9 |
| Jun 28 | 334 | +2,144 | 57.2 |
| Jun 29 | 282 | +1,746 | 61.3 |
| Jun 30 | 295 | +595 | 61.4 |
| **TOTAL** | **4,040** | **+17,689** | **62.1** |

(Jun 13/20/27 are Saturdays: skip-Saturday, zero trades by design.)

**Verdict: the June market was not the problem.** Every trading day green
fee-net, including Jun 17 (+$252) and Jun 18 (+$2,764), the days that ended
the live deployment. The account losses trace to the fade_live bug, $50 clips
on a $1,376 wallet, discretionary trades, and the overfit regime gates
(memory: june-deploy-cycle-postmortem).

## Vol-lookback arbitration (informative only; June is burned for selection)

| config | total | green days | worst day |
|---|---|---|---|
| realized/3600 (frozen) | +$17,689 | 15/15 | +$22 |
| realized/1800 (default) | +$19,504 | 14/15 | -$322 |

1800 makes ~10% more but breaks consistency, same shape as the May W1-W3
finding (3600 wins the consistency rule). **No config change.** The frozen
config stands; any future switch needs all-window evidence plus the July
sealed window untouched.

## Open caveat: replay vs live divergence on chop days

Harness Jun 18 = +$2,764 (hit 69.9%) while the live shadow-final stream scored
Jun 18 at roughly -$1,030 at-touch (hit 52.7%) and the live executor lost
~$300 before the morning kill. Decision logic is equivalence-tested
(GATE B passes), so this gap is INPUT divergence: recorded Telonex/Binance
archives vs live WS feeds (timing, receipt latency, book state) on a fast
whipsaw day. Treat harness whipsaw-day magnitudes with suspicion until the
July paper soak quantifies the live haircut day-by-day against these same
dailies. The month-level sign (solidly positive) is corroborated by both the
harness and the ungated live shadow stream (18/18 green at-touch).
