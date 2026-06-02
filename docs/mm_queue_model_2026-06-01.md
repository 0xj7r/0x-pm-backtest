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

## Review of stranded inventory handling (focused on paired_mm, the only strat run yesterday)

Current in paired_mm (the active one):
- Passive repair via max_leg_imbalance_shares: when |yes - no| exceeds it, skip the heavy leg and only quote the light leg to repair. (In wf grids: hardcoded 0.6 for tight pairing; sims use ~2; old default was loose 30.)
- No spot awareness originally: _spot ignored; relied on flow hitting the repair quote. Runner global imbalance cancel helps a bit.
- Exact problem seen: quoting/fills/rebate worked great (matched the queue log 1.6% fill rate), but could get stranded one-sided on adverse move with insufficient repair fills before resolution. Residual marks directionally.

Top wallets (forensics, 0xb27b, competitor re, queue log):
- Very tight residual (repair band ~clip size or 5%).
- Spot-leading pulls on the *exposed* side (accel, large taker, late) to avoid adding to stranding.
- Conditional lean: allow more imbalance (or skip repair) only if spot/delta confirms the heavy side is "right"; otherwise strict repair.
- Result: ~95% matched + rebate on volume; residual is minimized and the only risk.

Changes (only to paired_mm.rs, the one that was live as "pairedmm-paper"):
- Default max_leg_imbalance_shares tightened to 5.0.
- New config: late_pull_secs (45s), spot_accel_pull_thresh (0.0008), min_abs_spot_ret_30s_for_lean (0.0005), lean_extra_imbalance_shares (8.0).
- Logic added: late gate; compute 30s spot ret; accel pulls force-skip the would-be bad leg; lean widens the imbalance tolerance *only* on the favored heavy side (so you can profit from directionality if stranded "right") vs. strict repair quoting when wrong-sided.
- Test: spot_lean_and_accel_pull_affect_skips (uses proper SpotHistory spans).
- In wf paired grids: the explicit ..default() now pulls in the new lean/accel values automatically (on top of the 0.6 tight band).
- Verified: cargo test -p pm-strategy paired_mm (4/4 pass); full strat tests clean (97 pass after cleaning unrelated archive).

This directly incorporates the "directional signals" + "more robust repair" (via spot-gated lean vs force-repair) you asked for, modeled on what the top paired makers do. When running paired_mm (via main --strategy or walkforward), you now get the behavior.

(The delta archive copy and other strats were cleaned out of this change set per the clarification that only paired_mm was under test.)

### On other signals (RSI, EMA, etc) for paired_mm / this market
The codebase already has (and validates heavily in low_vol models, regime, br2, postfill etc):
- Custom equivalents that are more powerful than off-the-shelf TA for 5m BTC PM binaries + leading Binance spot:
  - WhipsawRiskSnapshot (reversal_pressure, path_efficiency, sign_flip_rate, realized_vol_180s) — reversal detects exhaustion/conflicting moves (RSI-like), path_efficiency + flips = chop vs trend (like ADX/choppiness, not pure EMA trend).
  - BtcRegime (Flat/Whipsaw good for neutral MM per comments).
  - spot_returns_and_accel + multiple return windows (5/15/30/60/120/180s) + flow imbalance from trades.
  - Book micro (ofi, microprice_dev) from signals.rs.
  - YES range_so_far + vol regime + dir std/flip_rate from features.
- Research (low_vol_decision_model, calm mm synthesis) shows these + spot leading are what matter; vanilla momentum was often noise or negative for calm/taker.
- Plain RSI(14) or EMA on spot/YES would be highly correlated with the return windows + reversal we already compute, but noisier on short event-driven windows and less calibrated to the exact adverse selection / stranding problem.

In this latest pass we wired WhipsawRiskSnapshot directly into paired_mm's lean scaling + dynamic repair tick (more aggressive fill on repair leg when rev_p high). This gives "RSI-like" mean-reversion awareness + "EMA-smoothed" chop awareness for deciding "repair stranded if wrong-sided" vs "tolerate/profit if favored", without new state or external deps.

If you want an explicit EMA of 30s spot returns as the lean signal (smoother than raw ret), or a full RSI helper in signals.rs for experimentation, or to gate the whole quote on BtcRegime::Whipsaw/Flat, say the word and we'll add (only touching paired_mm + signals as needed). The current custom ones are likely higher value for your stranded/rebate goal.
