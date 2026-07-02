# Strike-basis experiment: official strikes MUST NOT feed a Binance-basis belief

**Date:** 2026-07-02
**Question:** would using the official Polymarket open price (the resolution
strike) instead of the Binance-open proxy improve the fade, especially in the
near-strike zone where live coin-flips concentrate?

**Answer: NO. It inverts the edge, and the mechanism is basis mixing, not
model quality.**

## The experiment

Chop week (Jun 16-22), frozen config, canonical accounting, identical in every
way except `--strikes data/external/official_strikes_jun16_22.jsonl` (851
markets fetched from polymarket.com/api/crypto/crypto-price).

| day | proxy strike $ | official strike $ |
|---|---|---|
| Jun 16 | +1,224 | -613 |
| Jun 17 | -16 | -423 |
| Jun 18 | +2,847 | -1,926 |
| Jun 21 | +635 | -583 |
| Jun 22 | +1,599 | -2,553 |
| TOTAL | **+6,290** | **-6,099** |

54.5% of overlapping markets flip sides; the flipped set is -$6,180 under
official strikes vs +$1,588 under the proxy.

## Why: the basis is bigger than the signal

Official opens sit **-10.4 bps median** below the Binance proxy (p10-p90:
-12.4 to -7.6; matches the known ~14bps USD-index-vs-USDT gap). The belief's
effective spot is Binance-basis. Feeding it an official strike shifts every
moneyness calculation by ~10bps, i.e. **1.3x the median full-window move**
(7.7bps). The model then systematically believes spot opened above strike and
leans UP, wrong-siding half the book. The distortion is worst exactly
near-strike, where the coin-flip problem lives.

## What this settles

1. **The live engine's `strike_source: binance_proxy` is CORRECT.** Same-basis
   strike + same-basis spot is the only coherent construction. Do not "fix"
   the shadow to Gamma/official strikes while the spot feed is Binance.
2. **Resolution-basis risk is already priced into the observed hit rate**:
   the proxy-strike model is scored against OFFICIAL resolution outcomes in
   every backtest (June: +$17.7k, 62% hit), and historical at-the-money
   label disagreement was 0/16. There is no hidden settlement tax to fix.
3. **The near-strike coin flips are NOT a strike-basis problem.** The
   remaining candidate causes stay as identified: first-seconds input
   staleness (entry-delay gate + dwell telemetry, under soak validation).
4. If official strikes are ever wanted in the belief, the whole price path
   must switch basis together (official strike + Pyth/official spot stream),
   as the strike-basis memory already prescribed. That is a separate,
   deliberate project, not a config flip.

Data: data/external/official_strikes_jun16_22.jsonl,
runs in data/runs/june_strike_compare/.
