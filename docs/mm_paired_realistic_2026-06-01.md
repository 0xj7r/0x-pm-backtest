# Realistic-fill paired-MM backtest (Phase 2 of the robust MM backtest)

Date: 2026-06-01
Script: `scripts/mm_paired_realistic_sim.py`
Full output: `docs/mm_paired_realistic_results_2026-06-01.txt`
Builds on Phase 1: `scripts/mm_queue_model_fit.py` + `docs/mm_queue_model_2026-06-01.md`

## Purpose

The optimistic paired-MM sim (`scripts/mm_paired_sim.py`) fills a resting leg on
EVERY taker print that crosses our price, pro-rata by `clip/(clip+depth)`. The
Phase-1 queue-model fit (calibrated on 19,435 live queue records, 72 fills)
showed that overstates the live fill RATE: live orders rest ~4s on a 5s replace
cycle and only **1.6% of posted orders ever fill**, because flow rarely arrives
in the short window the order is actually resting at the touch. CONDITIONAL on a
fill we capture ~4x pro-rata (front-of-queue from joining at the touch). This
Phase-2 sim replaces the fill logic with that empirically-calibrated model and
reports how much the optimism inflated per-day P&L.

## The realistic fill model

Three mechanisms replace "fill on every cross":

1. **Rest-and-hold.** We post a two-sided quote, hold it for `REST_WINDOW`
   seconds, then requote on a `REQUOTE_CADENCE` grid. An order can fill ONLY from
   taker prints arriving DURING its own rest window. The remainder of each
   cadence bucket is the idle/replace gap where nothing can fill (mirrors the
   live 5s replace cycle).
2. **Locked-price quoting + through-depth gate.** The quoted prices are LOCKED
   at the cycle start and held for the whole rest window. A print fills us only
   if it crosses the price WE POSTED (not the live touch) AND its size exceeds
   the depth that was ahead of us at post (`tsz > post_depth`). When mid drifts
   during the rest window our locked quote sits behind the touch and never
   fills. This price-following lag plus the through-depth requirement is what
   collapses the order fill RATE to the live ~1.6%.
3. **Conditional front-of-queue capture.** When the gate fires, the realized
   size is `min(clip, K * pro_rata * clip, cycle_room)` with `pro_rata =
   clip/(clip+post_depth)`, `K~4` (the live geomean ~4.3, median 3.8). Cumulative
   fills across a rest window cannot exceed the clip.

Everything else (strict pairing, residual mark-to-outcome, the regime quote-gate,
the dynamic tick gates: late-pull, spot-accel, large-taker, repair band) is
reused UNCHANGED from `mm_paired_sim` via `import mm_paired_sim as S`.

## Tunable params (top-level, for Phase 3 sweeps)

| Param | Default | Meaning | Live anchor |
| --- | --- | --- | --- |
| `CAPTURE_K` | 4.0 | conditional front-of-queue beat over pro-rata | geomean 4.28, median 3.77 |
| `REST_WINDOW` | 4.0s | seconds an order actually rests at the touch | live median rest 3,994ms |
| `REQUOTE_CADENCE` | 5.0s | seconds between posts (the replace cycle) | live 5s replace cadence |

CLI: `--K`, `--rest`, `--cadence`, plus `--clips` and `--limit`. These three are
the primary fill-model dimensions Phase 3 should sweep.

## Validation: implied fill-rate + capture vs live

Full May dataset, GATED, 121,866 post-cycles:

| clip | order-fill-rate | uncond share frac | realized/pro-rata |
| --- | --- | --- | --- |
| 5  | 1.31% | 0.53% | 1.8x geomean (2.1x median) |
| 10 | 1.23% | 0.30% | 0.7x |
| 20 | 1.23% | 0.15% | 0.2x |
| **live (Phase 1)** | **~1.6%** | **0.84%** | **~4x (median 3.8)** |

The **fill RATE is reproduced accurately** (1.2-1.3% vs live 1.6%, squarely in
the target 1-5% band) and is robust across clip. The unconditional share
fraction is the same order of magnitude (0.15-0.53% vs 0.84%).

The realized/pro-rata multiple lands ~1.8x at clip 5 (the live calibration clip)
and degrades at larger clips. This is NOT the fill model failing: instrumentation
shows **82% of fills are capped at ~2 shares by the strict-pairing repair band**
(`REPAIR_DELTA_SHARES = 2`, inherited unchanged from `mm_paired_sim`). The K=4
front-of-queue beat IS applied per-print, but the repair band deliberately limits
each fill to keep the YES and NO legs paired, so the effective order-level
multiple is suppressed below 4x, and more so as clip grows (a 2-share cap is a
larger fraction of a 5-clip than a 20-clip). The 4x conditional capture and the
2-share strict-pairing cap are two real, independently-calibrated mechanisms that
interact; the live 72-fill sample was at clip 5 where they partially offset.

## Realistic vs optimistic net P&L (how much the optimism inflated it)

Full May dataset (14 days). `$/day` is total net over the 14 days / 14.

| Config | Realistic $/day | Optimistic $/day | Inflation |
| --- | --- | --- | --- |
| clip5  GATED   norebate | 8.54  | 20.77 | 2.4x |
| clip5  ungated norebate | 13.64 | 28.64 | 2.1x |
| clip5  GATED   rebate   | 8.90  | 23.09 | 2.6x |
| clip10 GATED   norebate | 9.10  | 22.55 | 2.5x |
| clip10 ungated norebate | 13.78 | 33.06 | 2.4x |
| clip20 GATED   norebate | 9.10  | 24.52 | 2.7x |
| clip20 ungated rebate   | 14.51 | 48.19 | 3.3x |

The optimistic sim inflated net P&L by **~2.1-3.3x** across the grid; the
inflation widens with clip (optimistic P&L scales with clip via more fill volume,
realistic P&L saturates because the through-depth gate and repair band bind). The
GATED clip-5 config (closest to the live edge06 deployment) goes from $20.77/day
optimistic to **$8.54/day realistic** (2.4x).

### Composition shift: realistic P&L is residual-dominated

The most important finding is qualitative, not the headline multiple. Under
realistic fills the round-trip PAIR P&L nearly vanishes (clip5 GATED: pairs
$1.9 vs optimistic $65.1) and net is almost entirely **residual** (res $117.7).
Because fills are so rare (~1.3%), the two legs almost never accumulate enough to
pair, so the strategy is no longer capturing the spread on matched pairs: it is
holding a small unmatched directional residual that resolves at the BTC up/down
outcome. The positive residual P&L over May is a coin-flip that happened to land
favourably this month, NOT durable spread-capture edge. `pct_markets_pos` drops
from ~54% (optimistic) to ~20-27% (realistic), consistent with a thin residual
book rather than a steadily-earning maker. This is the honest takeaway: at the
live fill rate, the calm-regime paired-MM is barely transacting, and what little
P&L there is comes from residual exposure, not the spread.

## Caveats (do not over-fit)

- **The calibration is thin and directional.** Phase 1 fit from only 72 fills,
  in ONE ~11h overnight session skewed toward a stress/reversal regime, at a tiny
  5-share clip. The K~4, rate~1.6%, and the 5s cadence are directional anchors,
  not precise constants. The model will sharpen as more live queue data accrues.
- **The queue log was paper-tagged** (`pairedmm-paper` client-order-id prefix),
  so Phase-1 "fills" are the live engine's own tape+queue model, not ground-truth
  exchange queue behaviour. A real-money queue log would be the authoritative
  input. (The records are real venue tape; "paper" is a naming carryover for the
  order tagging, not a paper-trading mode.)
- **The no-chase fix just deployed should RAISE the live fill rate** (holding
  quotes instead of chasing mid means orders rest longer and catch more flow), so
  the ~1.6% rate is likely a floor; Phase 3 should re-anchor `REST_WINDOW` /
  `REQUOTE_CADENCE` once post-fix queue data is available.
- **Regime generalisation is unproven.** The Phase-1 session was stress-regime;
  this sim runs on May calm-regime tape. Fill dynamics may differ.
- **Inherited mechanisms dominate the conditional capture.** As shown above, the
  strict-pairing 2-share repair band caps most fills, so the K=4 front-of-queue
  beat is largely masked at the order level. Phase 3 should sweep
  `REPAIR_DELTA_SHARES` jointly with `K` to separate the two.

## What Phase 3 inherits

`scripts/mm_paired_realistic_sim.py` is the harness. Phase 3 sweeps the eval
dimensions on it: `--K`, `--rest`, `--cadence`, `--clips`, gated/ungated,
rebate/norebate, and (recommended) `REPAIR_DELTA_SHARES`. The realistic numbers
above are the baseline to beat; the key open question Phase 3 must answer is
whether ANY parameterisation produces durable pair-spread P&L rather than the
residual coin-flip the live-calibrated fill rate currently implies.
