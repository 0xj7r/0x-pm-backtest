# Empirical MM fill/queue model (Phase 1 of the robust MM backtest)

Date: 2026-06-01
Script: `scripts/mm_queue_model_fit.py`
Data: `data/mm_queue.jsonl` (live-captured queue log, scp'd from whale-pair-dublin)

## Purpose

The paired-MM sims (`scripts/mm_paired_sim.py`) fill a resting order at the
pro-rata fraction `clip / (clip + shares_ahead)` on every taker print that
crosses our price. This Phase-1 analysis calibrates the ACTUAL fill behaviour
from the queue log the live MM wrote, to replace that optimistic assumption.

## The data

19,435 records over one ~11h overnight session (2026-05-31 20:33 UTC ->
2026-06-01 07:11), 124 markets:

- post: 4,359 (every order is `pairedmm-paper:...`, clip mostly 5 shares)
- tick: 11,140
- cancel: 3,864 (3,753 `replace`, 111 `pull`)
- fill: 72 (across 70 distinct orders)

Confirmed field semantics from the records:

```
realized_capture                    = fill_cumulative_shares / clip
pro_rata_expected_capture           = clip / (clip + shares_ahead_at_submit)
capture_ratio_realized_over_prorata = realized_capture / pro_rata_expected
```

Leg-to-taker mapping: a `bidyes` (resting YES bid) fills on taker SELL prints; a
`buyno` (resting NO buy = synthetic YES bid) fills on taker BUY prints. The
`taker_*_qty_60s` series is a 60s trailing window, not order-life-scoped, so it
is only usable as a coarse through-flow proxy.

## Headline finding: the two framings invert

This is the load-bearing result. "Do we beat pro-rata?" has opposite answers
depending on whether you condition on having filled:

| Framing | realized fill frac | vs pro-rata |
| --- | --- | --- |
| CONDITIONAL (the 70 orders that filled) | 0.507 | ~4.1x geomean (median 3.8x); 92% of fills exceed pro-rata |
| UNCONDITIONAL (all 4,359 posted orders) | 0.0081 | 0.07x: pro-rata overstates ~14x |

Only 1.6% of posted orders fill at all. We posted 21,075 shares and filled
177.5 (0.84% of posted volume). The logged `capture_ratio_realized_over_prorata`
(mean 9.3, median 3.77, p95 28.7, max 112.6) is real but is a CONDITIONAL
statistic: it describes only the orders that filled, which is why it looks like
we crush pro-rata. The dominant reality is that orders rest ~4s (median 3,994ms,
capped by the 5s replace cycle) and are cancelled/replaced before any taker hits
the level.

## Fill frequency and time-to-fill

- Order fill rate: 70 / 4,359 = 1.6%. Zero orders fully filled.
- Rest time: median 3,994ms, p90 6,433ms, max 30,000ms.
- Time-to-(last)-fill: median 3,052ms, p90 6,951ms.
- Fill rate by time-to-close: >120s 1.8% (69/3,751), 60-120s 0.0% (0/533),
  30-60s 1.3% (1/75). No edge near the close in this sample.
- Front-of-queue: 12/70 fills landed within 1.5s of post.

## Fitted fill function

Model A (multiplier on pro-rata size), fitted on the 70 filled orders:
`realized_fill_frac = K * pro_rata_frac`, K geomean 4.28 (median 3.77, mean
9.37). This is the CONDITIONAL size beat.

Model B (taker through-flow): `realized_fill_frac = alpha * taker_through /
(depth_ahead + clip)`, alpha geomean ~0.06. Order-of-magnitude only: the
through-flow proxy is the max 60s trailing taker volume over the order's life,
not exact through-queue volume.

## Recommended model for the robust backtest

Pro-rata gets the SIZE roughly right when a fill happens (the ~4x conditional
beat is consistent with front-of-queue advantage from joining at the touch), but
it gets the RATE badly wrong: the current sim effectively fills on every
tape-cross, whereas live orders fill only 1.6% of the time because they are
replaced on a 5s cycle before flow arrives. So:

1. Keep `clip / (clip + depth)` as the fill SIZE conditional on a fill, or apply
   the conditional K~4x beat if you want to credit front-of-queue.
2. Gate fills on actual taker through-flow during the order's rest window
   (replace the "fill on any cross" logic), so the order fill RATE collapses to
   the observed ~1.6% rather than ~100%.
3. As a quick directional correction without re-architecting: scale the current
   sim's MM fill volume to ~0.07x (the existing pro-rata-on-every-post sim
   overstates fills ~14x).

## Caveats (do not over-fit)

- The log is PAPER-traded (`pairedmm-paper` prefix). The "fills" are produced by
  the live engine's own tape+queue model, not real exchange fills, so this
  calibrates the engine's internal assumption, not ground-truth exchange queue
  behaviour. A real-money queue log would be the authoritative input.
- Only 72 fills: too thin for a trustworthy regression. The capture-ratio
  distribution and the fill-frequency are the robust takeaways; the K and alpha
  point estimates are directional.
- Tiny clip (5 shares) and a single overnight session skewed toward
  stress/reversal regime. Generalisation to calm regime and to live $1k-cap
  sizing is unproven.

## Full script output

```
LOADED data/mm_queue.jsonl
record counts: {'post': 4359, 'tick': 11140, 'cancel': 3864, 'fill': 72}
distinct orders: posts=4359 with-ticks=4359 with-fills=70 cancels=3864

1) CAPTURE RATIO  (realized_fill_fraction / pro_rata_fraction), per fill
   n fills = 72
   mean=9.299  median=3.768  geomean=4.124
   p5=0.674 p10=1.089 p25=2.059 p50=3.768 p75=7.840 p90=22.592 p95=28.654
   min=0.098  max=112.592
   fraction of fills with capture_ratio > 1 (beat pro-rata): 66/72 = 92%

2) FILL FREQUENCY & TIME-TO-FILL
   posts (orders)      = 4359
   orders with a fill  = 70  (1.6% of orders)
   fully-filled orders = 0  (0.00% of orders)
   rest time (ms): median=3994  p90=6433  max=30000
   time-to-(last)-fill ms: median=3052  p90=6951
   fill rate by secs_to_close bucket: 30-60s 1/75=1.3%  60-120s 0/533=0.0%  >120s 69/3751=1.8%

2.5) CONDITIONAL vs UNCONDITIONAL
   mean pro_rata_expected_frac (all orders) = 0.1148
   mean realized_fill_frac     (filled only) = 0.5071
   mean realized_fill_frac     (ALL orders)  = 0.0081
   CONDITIONAL  ratio (filled / pro_rata) = 4.42x
   UNCONDITIONAL ratio (all / pro_rata)   = 0.071x  (pro-rata overstates ~14x)
   total shares posted=21075  total filled=177.5  (0.84% of posted volume)

3) FITTED FILL FUNCTION
   Model A  realized_fill_frac = K * pro_rata_frac
     K: median=3.77  mean=9.37  geomean=4.28  (n=70)
   Model B  realized_fill_frac = alpha * taker_through / (depth_ahead + clip)
     alpha: median=0.0589  geomean=0.0597  (n=65)  [proxy through-flow, OOM only]
   front-of-queue: 12/70 fills landed within 1.5s of post
```
