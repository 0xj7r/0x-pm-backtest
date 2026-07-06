# Latency truth: the backtest reconciles with live at measured latency

**Date:** 2026-07-06
**The question:** why does live not align with backtesting?
**The answer:** the backtests modeled 250ms signal-to-fill; the measured live
chain is ~875ms mean / ~1.4s p90. Re-running June at truthful latency
reproduces the live experience. The backtest was right about the market and
wrong about our reflexes by ~1 second.

## The pricing table (June 13-30, frozen config, fee-net, $50 clips)

| fill latency | 5m NET | 5m profile | 15m NET |
|---|---|---|---|
| 250ms (old assumption) | +$17,689 | 15/15 green | +$4,586 |
| 750ms (engineered-fast us) | +$7,507 | | (pending single run) |
| 1250ms (current us) | **+$2,506** | 6/15 green, mean +$167/day | **+$1,889** |
| 2000ms | -$597 | | +$1,345 |

Reconciliation check: the 1250ms replay's per-day profile (6/15 green, mean
+$167/day at $50) matches the live soak week's observed numbers. Hit rate is
IDENTICAL (62.0%) at every latency: we win as often, we get paid worse,
because the book repriced before our fill. The entire gap is price
degradation plus the side-flip instability documented separately.

## Where the 1 second lives (and what is fixable)

| leg | time | fixable? |
|---|---|---|
| Binance geography (Tokyo matching engine) | 100ms | no (moving hurts the 9ms book leg) |
| decision timer phase (1s cadence) | ~500ms mean | YES: event-driven / 100ms cadence |
| executor JSONL-tail hop | ~100-150ms | YES: in-process execution |
| sign + POST + venue match | ~275ms | marginal |

Engineering target: ~450ms signal-to-fill, i.e. between the 250ms and 750ms
rows. Both fixes preserve the decision SSOT (decide_entry unchanged; only
WHEN it runs and HOW its output travels change) and are in build as of today
(fast-engine workflow: pm-shadow decide-interval-ms + polymarket-exec
fast_live in-process binary), to be deployed as a PARALLEL paper stream,
never touching the measuring soak.

## Strategic consequences

1. **The 15m book is latency-robust**: it keeps 41% of its edge at 1250ms
   and 29% even at 2s. It is deployable on TODAY'S architecture.
2. **The 5m book requires the fast engine** to be worth running (+$2.5k ->
   +$7.5k+ per June-month at telemetry scale).
3. **Every historical backtest number in this repo is a 250ms upper bound.**
   Any strategy decision from here uses truthful-latency runs (750ms for the
   fast architecture, 1250ms for current).
4. Combined honest expectation at 1% sizing of $850, June-like month:
   current architecture ~$750/month (5m+15m); fast architecture
   ~$1.7-2.7k/month. Scales linearly with bankroll.
5. The soak's pre-registered gate verdict continues untouched, but its
   question narrows to: does the gate still add value AT the deployable
   latency (750ms run with gate: pending, data/runs/latency_truth/).
