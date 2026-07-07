# "Chop destroyed us" is false at the regime level (2026-07-08)

The recurring June thesis was that choppy/whipsaw tape is what sank the fade.
The Feb 2026 deep-history validation refutes it. February 2026 was the FIFTH
choppiest month since 2019 (68% chop days by the vol x efficiency classifier)
and the ungated fade at truthful latency (1250ms) made +$11,870 (63.8% hit),
+$16,092 at fast latency. Chop was its best month on record here.

## Pooled day-regime economics (Feb + May + Jun, 1250ms, ungated, $50 clips)

| regime | days | mean/day | green | worst day |
|---|---|---|---|---|
| trend | 1 | +$1,381 | 1/1 | +$1,381 |
| chop | 13 | **+$694** | 9/13 | -$853 |
| quiet | 31 | **+$691** | 20/31 | -$1,880 |
| mixed | 3 | -$39 | 1/3 | -$870 |

Chop and quiet BOTH pay ~$690/day. The strategy is broadly profitable across
regimes at truthful latency. The only soft cell is "mixed" (n=3, negligible).

## Why the June intuition was wrong

The June-only chop cell (4 days, -$114/day) was small-n and unlucky. It got
generalized into "chop kills us." Adding February's 9 chop days (+$1,053/day,
8/9 green) flips the pooled mean strongly positive. February chop was HIGH-VOL
chop: big swings, low net direction. That is exactly the fade's food: spot
dislocates, the Polymarket book lags, the fade sells the overshoot, mean
reversion pays. More volatility = more dislocations = more edge, whether the
day trends or chops.

## The corrected mental model

The fade is a VOLATILITY / DISLOCATION harvester, not a trend or mean-reversion
bet. Its P&L scales with how often the book misprices vs exogenous spot/perp,
which scales with realized vol. "Chop" per se is irrelevant; only two things
hurt, and neither is common or large at truthful latency + proper sizing:
1. A handful of specific tight-whipsaw sequences (sequential fades that buy a
   >$1 combined-cost pair). E1 quantified these: rare (7-11% of markets) and
   their leg-2 is right 74% of the time anyway.
2. The ruin that actually happened in June: a config bug (-$1,050), discretionary
   overrides, and flat $50 clips at $850 bankroll (ruin sizing), traded on a
   backtest that assumed 250ms fills. Market regime was NOT the cause.

## Consequence for the plan

This is the all-weather evidence the rebuild was chasing. At 1% fractional
sizing and truthful-latency expectations, the ungated fade has positive
regime-weighted EV that does not depend on avoiding chop. The remaining risk
is purely realization (do live fills capture the replay edge), which the soak
and micro-live phase measure. Regime-timing / chop-avoidance gates are
unnecessary and, as the falsification campaign showed, actively harmful.

Caveat: ~48 day-sample across 3 months. March + April cells (running) and the
live soak extend it. The direction is strong and consistent, not yet a
tight confidence interval.
