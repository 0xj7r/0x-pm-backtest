# Vol-conditioned paired-MM backtest (May BTC-5m)

Script: `scripts/mm_paired_vol_conditioned.py` (imports `scripts/mm_paired_sim.py`).
Run: `python3 scripts/mm_paired_vol_conditioned.py`.

Question: does the two-sided paired-MM make POSITIVE P&L in LOW-vol markets (its
intended calm regime), and where does it turn negative as realized vol rises?

Setup: clip 10, no rebate, both UNGATED and GATED. Per market we compute net P&L
(from the queue-aware paired sim) and realized vol = stdev of 5s spot returns over
the 5m window, in bps. 14 days (2026-05-07..05-20), 3767 parsed markets, 3753
ungated-quotable / 3220 gated-quotable.

## Net P&L by realized-vol bucket (UNGATED, clip 10, no rebate)

| vol (bps) |    n | mean_net | tot_net | $/day | pos% | mean_resid% | worst_net |
|-----------|-----:|---------:|--------:|------:|-----:|------------:|----------:|
| 0-0.5     |  913 |    0.097 |   88.23 |  6.30 |   55 |        25.1 |     -1.64 |
| 0.5-1     | 1751 |    0.113 |  198.66 | 14.19 |   54 |        21.6 |     -1.42 |
| 1-1.5     |  648 |    0.155 |  100.70 |  7.19 |   55 |        18.0 |     -1.40 |
| 1.5-2     |  265 |    0.215 |   57.02 |  4.07 |   60 |        14.8 |     -1.21 |
| 2-4       |  165 |    0.081 |   13.39 |  0.96 |   49 |        11.2 |     -1.16 |
| 4-6       |   10 |    0.354 |    3.54 |  0.25 |   60 |         8.5 |     -1.30 |
| 6-8       |    1 |    1.288 |    1.29 |  0.09 |  100 |        10.5 |      1.29 |
| 8-12      |    0 | (none)   |         |       |      |             |           |
| >12       |    0 | (none)   |         |       |      |             |           |

## Net P&L by realized-vol bucket (GATED, clip 10, no rebate)

| vol (bps) |    n | mean_net | tot_net | $/day | pos% | mean_resid% | worst_net |
|-----------|-----:|---------:|--------:|------:|-----:|------------:|----------:|
| 0-0.5     |  897 |    0.085 |   76.04 |  5.43 |   55 |        30.9 |     -1.11 |
| 0.5-1     | 1728 |    0.091 |  156.62 | 11.19 |   53 |        26.2 |     -1.25 |
| 1-1.5     |  507 |    0.128 |   65.14 |  4.65 |   56 |        37.7 |     -1.30 |
| 1.5-2     |   62 |    0.208 |   12.91 |  0.92 |   61 |        58.9 |     -1.18 |
| 2-4       |   26 |    0.190 |    4.94 |  0.35 |   58 |        56.5 |     -1.04 |

## Headline (UNGATED, clip 10), median realized vol = 0.74 bps

- LOW-vol (<= median): n=1877, total=$180.75, $12.91/day, pos=54%
- HIGH-vol (> median): n=1876, total=$282.09, $20.15/day, pos=55%

## Oscillating vs drifting within LOW-vol (<= median vol), split at median flip frac = 0.60

- OSCILLATING (flip >= 0.60): n=965, total=$105.70, $7.55/day, mean=$0.1095, pos=55%
- DRIFTING    (flip <  0.60): n=912, total=$ 75.05, $5.36/day, mean=$0.0823, pos=53%

## Findings

Net P&L is POSITIVE in every populated vol bucket; the sim never produces a
negative-mean bucket on this May sample. Per-market mean net actually RISES with
vol across the dense 0-2 bps range (0.097 -> 0.113 -> 0.155 -> 0.215) while the
residual fraction FALLS (25.1% -> 14.8%): more two-sided flow fills both legs, so
pairing completes more cleanly. Positivity comes from many small wins (pos% only
~54-60%) against a bounded per-market downside (worst single market ~ -$1.6).

The degradation as vol climbs is in OPPORTUNITY/VOLUME, not in per-market sign. By
2-4 bps there are only 165 markets ($0.96/day) and beyond 4 bps the sample is
essentially empty (10, then 1 market). On BTC-5m the realized 5m vol is uniformly
tiny (median 0.74 bps); the regime never gets genuinely high-vol in this window, so
we cannot observe a per-market sign-flip to negative. The "high-vol > median" half
out-earns the "low-vol <= median" half ($20.15 vs $12.91/day) precisely because the
split sits at 0.74 bps and the upper half is still calm but has more flow.

Oscillating low-vol markets out-earn drifting ones ($7.55 vs $5.36/day; mean $0.110
vs $0.082), consistent with the thesis that pairing completes when price oscillates
back through the resting quotes. The gated variant is uniformly lower (its quote
gate skips ~14% of markets and trims fills) but tracks the same shape.

## Honest caveats

- PRO-RATA queue fill model. It does NOT model the favourite-side stranding /
  reversal-day -EV losses we observe LIVE (the passive flatten failing). Absolute
  P&L here is an UPPER BOUND / optimistic; the live gap is the stranding (being
  fixed via an active flatten). The VALUE of this study is the RELATIVE gradient
  across vol buckets, not the absolute level.
- Because the live -EV tail is unmodelled and realized 5m vol never gets large in
  this sample, the sim does not show a bucket turning negative. Do not read that as
  "high vol is safe"; it is the regime where stranding bites LIVE and is exactly
  what this sim cannot see.
- May calm sample only; no cross-regime OOS.
