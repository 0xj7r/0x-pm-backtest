# Drawdown realism at $850: sizing policy and the thin-margin finding

**Date:** 2026-07-02
**Method:** trade-by-trade equity replay of the harness per-trade dumps
(May 7-28 + Jun 13-30, 11,097 trades, fee-net, latency 250ms) with a cash
constraint: capital is tied up from fill to market close, entries skip when
cash runs out. Policy = min(frac x running equity, $50 ceiling), the executor's
PM_SHADOW_CLIP_FRAC implementation. Tool: scripts/research/equity_drawdown_sim.py.

## Base case (harness P&L as-is), start $850

| policy | final | max DD | max DD % | worst day | note |
|---|---|---|---|---|---|
| frac 0.5% | $39,895 | $2,163 | 10.8 | -$63 | |
| frac 1% | $50,777 | $2,163 | 7.0 | -$132 | deployed config |
| frac 2% | $54,077 | $2,163 | 6.3 | -$270 | |
| flat $10 | $11,954 | $442 | 44.2 | -$139 | DD% hit early, no cushion |
| **flat $50** | **$0** | $1,605 | **100 (RUIN)** | -$984 | 10,773 entries skipped broke |

**Flat $50 clips on an $850 bankroll is certain ruin in replay**, even though
the strategy is profitable: one adverse run exhausts cash before the recovery
arrives. This is the quantified version of what June felt like live, and why
the aggregate "+$17.7k at $50 clips" tables cannot be read as achievable at
this bankroll. Fractional sizing removes the ruin mode structurally: the clip
shrinks with equity, so the account cannot be exhausted by a losing run.

## The thin-margin finding (more important than drawdown)

Stress: haircut every winning trade to X% of harness value, losses full size
(a proxy for live decision/input divergence; per-fill slippage is already
known to be ~zero at these clips).

| stress | frac 1% final | max DD % |
|---|---|---|
| wins x1.00 | $50,777 | 7% |
| wins x0.82 | ~break-even | |
| wins x0.70 | $100 (bleed) | 88% |
| wins x0.50 | $99 (bleed) | 88% |

**Break-even is at 82% win realization.** If live captures less than ~82% of
the harness's winning-trade P&L, the edge nets negative at any sizing;
fractional sizing then turns ruin into a slow bleed rather than saving the
account. The margin of safety is thinner than the healthy-looking June total
suggests.

Implication for the July soak: process parity alone is not enough to re-arm.
The soak (and the first micro-live phase) must measure the **realization
ratio**: realized P&L per day / harness P&L for the same day, same markets.
Paper fills cannot measure this (no real fills), so the first 2 weeks of
micro-live at 1% clips (~$8) ARE the measurement, with a bounded worst case
(slow bleed at $8 clips loses tens of dollars per bad week, not hundreds).

## Policy (wired into PROD.md governance)

1. **Fractional sizing is mandatory**: PM_SHADOW_CLIP_FRAC=0.01,
   PM_SHADOW_CLIP_USD ceiling $10 at the current bankroll. Never flat clips
   above ~1.2% of bankroll.
2. **Ceiling raises only with bankroll** (clip = 1% x equity, ceiling = 1.2% x
   equity, recomputed weekly, manual until proven).
3. **Re-arm gates, in order**: (a) 7 PASS soak days (process), then (b) 14
   micro-live days measuring realization ratio >= 0.85 vs same-day harness
   replay before any size increase.
4. No P&L circuit breakers (research-rejected; amputates recoveries). The
   drawdown control IS the sizing.

## Caveats

The replay takes every harness trade with zero misses and compounds two months
uninterrupted; absolute finals are fantasy upper bounds. The decision-relevant
outputs are the ruin/no-ruin distinction, the DD percentages, and the
break-even realization ratio.
