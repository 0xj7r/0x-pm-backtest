# Live-vs-replay divergence: where the June edge actually leaked

**Date:** 2026-07-02
**Question:** the drawdown study set break-even at 82% win realization; did live
decisions achieve it? Measured directly: shadow-final live stream (frozen
config, LIVE feeds) vs the canonical harness replay (same config, RECORDED
feeds), per market, Jun 13-18, clip-1, identical $50 at-touch scoring.

## Measured realization: 0.34 overall, but bimodal

| day | live-decided $ | replay-decided $ | ratio | side agreement |
|---|---|---|---|---|
| Jun 14 | +658 | +816 | 0.81 | 88.3% |
| Jun 15 | +1,791 | +1,597 | 1.12 | 91.7% |
| Jun 16 | +604 | +1,023 | 0.59 | 59.6% |
| Jun 17 | +41 | +112 | 0.37 | 50.0% |
| Jun 18 | -1,172 | +2,040 | -0.57 | 44.9% |

On calm days live realizes the backtest (0.8-1.1). On fast/chop days side
agreement collapses to a coin flip and realization goes negative. The
strategy is not globally broken; the leak is localized.

## The leak has a decision-time signature

Decomposing live trades by whether the replay chose the same side:

| bucket | n | median \|p-0.5\| | median edge | median entry sec | hit | pnl |
|---|---|---|---|---|---|---|
| replay agrees | 507 | 0.242 | 0.161 | 21s | 62.3% | +$4,918 |
| replay opposite | 250 | 0.379 | 0.319 | **6s** | 39.6% | **-$3,008** |

The coin-flips are the HIGHEST-conviction, EARLIEST entries: a fast move in
the first seconds of a window, live and recorded feeds see different
snapshots, the belief saturates in opposite directions. Claimed edge of 0.32
six seconds after the open is a stale-input artifact, not signal (same
mechanism as the 2026-06-16 postmortem's p=0.947-at-52c orphan).

This also resolves the apparent conflict with the May "adverse windows not
gateable" result: that research tested MARKET-state prediction and correctly
found none. This is INPUT-quality fragility, observable per decision, which
was never tested.

## Candidate fix: entry delay (+ extremity cap), measured on the live set

| gate | n | live pnl | hit |
|---|---|---|---|
| none (baseline) | 1,046 | +$1,922 | 54.7% |
| skip first 10s | 551 | +$2,717 | 57.9% |
| skip first 15s | 510 | +$2,765 | 58.2% |
| 20s + cap \|p-0.5\| <= 0.42 | 366 | +$2,548 | 57.4% |

Jun 18 improves from -$1,172 to -$294 under the combined gate. Cost check on
the idealized side: harness May+June entries under 15s are 17.6% of trades and
10.8% of NET, so the delay gives up ~11% of the recorded-feed edge to remove
the bucket that was NET-NEGATIVE live.

## Discipline and next steps

These gates were selected on Jun 13-18 live tape (n=1,046), the same trap as
the June regime gates. NOT deployed. Validation plan:

1. The July paper soak measures this for free: shadow-final logs every
   would_enter with timestamps; each day, score <15s vs >=15s entries against
   the same-day replay (data/runs/daily_replay). Require the pattern to hold
   across the soak week before any config change.
2. If confirmed, `min_secs_from_open` already exists in the harness config
   surface; add it to the frozen config through the normal deployment gate.
3. Root-cause track (better, slower): first-seconds input quality (strike
   capture, feed latency, belief dwell/stability before entry) so the trades
   do not need to be skipped at all.
4. Any deeper strategy hunt should treat "realization ratio by entry-second
   bucket" as a first-class metric; a strategy that only works on recorded
   feeds is not a strategy.
