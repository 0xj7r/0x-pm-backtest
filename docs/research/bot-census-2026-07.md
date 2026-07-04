# Bot census: who trades BTC-5m, what they run, what replicates

**Date:** 2026-07-04
**Method:** every trade in 72 consecutive BTC-5m markets (6h window) via the
data-api per-market trades feed (868 wallets, 1,931 taker prints), then
activity deep-dives on the distinctive wallets (3.5k-row API cap means
high-volume wallets show partial windows; net-flow reads are only trusted
where redeems fall inside the window). Census tooling:
wallet_census.py / wallet_deepdive.py (session scratch; rerunnable).

## Archetype map (observed live, July 4)

| archetype | example | evidence | verdict for us |
|---|---|---|---|
| Hold-to-redemption fade/mispricing at scale, multi-asset, late entries | ce25, "Bonereaper" whale | ce25 +$49.9k/6wk (archive); Bonereaper 3,380 buys 0 sells in hours | OUR family; already validated |
| End-game certainty sweep (buy >=0.97-0.99, final 30-60s, hold) | EVP-HalfKelly | +$4.7k over 1.8d incl. redeems; fee curve at p=0.99 is ~free | REPLAYED: naive version LOSES (below) |
| Two-way intra-window scalper | TZOdds | +$3.2k over 25 days, 1,774 buys / 1,549 sells, 55s median entry | credible small edge; needs exit reverse-engineering (parked) |
| Sub-15s sniper | Kim10 | -$1,047 over 53 days, 100% early entries | LOSES; independent validation of our stability gate |
| Cheap-tail buyer (median px 0.25) | F0x | -$8.6k/day | furnace; matches our F11 rejection |

## The sweeper replay (the census's main testable lead)

Harness, June 13-30 BTC-5m, canonical accounting, aligned-mode late entry,
hold to redemption:

| variant | n | hit | break-even hit | NET |
|---|---|---|---|---|
| mid >= 0.97, last 45s | 1,665 | 97.90% | ~98.0% | -$556 |
| mid >= 0.985, last 25s | 1,253 | 98.48% | ~98.6% | -$386 |

Fees are nearly zero there ($90 on 1,665 trades: the 0.07 p(1-p) curve
subsidizes the trade), but the book's final-minute certainty pricing is
EFFICIENT: posted 0.98 resolves ~98%. The operator's profit therefore comes
from selection beyond the posted price (own certainty model at t-15s) and
exit discipline on the rare flips (346 sells in the sample), not from a
harvestable posted-price bias. Naive replication: REJECTED. A model-assisted
variant (our belief as the certainty filter) is possible future work but
starts from a measured -0.1 to -0.5% margin to overcome per trade.

## Structural findings worth keeping

1. **The fee curve shapes the ecosystem**: max toll at p=0.5 (where we trade)
   and ~zero at the extremes. Any strategy near 0.5 must clear ~3.5% of
   notional per round trip; extreme-price strategies pay ~nothing and are
   correspondingly crowded/efficient.
2. **The fast race does not pay at observable scale**: the census's dedicated
   early sniper loses over 53 days; ce25 avoids <5s entirely; our own gate
   evidence says the same. Three independent sources now.
3. **Census tooling is reusable**: rerun the census periodically (cheap) to
   watch for new archetypes; the per-market trades feed is the discovery
   surface Telonex lacks (no wallet ids in its parquet).

## Follow-ups (parked, post-rollout)

- TZOdds-style scalper: reconstruct entries/exits against our book archive to
  identify its signal; only two-way archetype with a long positive record.
- Model-assisted sweeper: our belief at t-20s as certainty filter over the
  0.985 basket (needs to find >0.5% selection edge; plausible but unproven).
- Periodic census cron for competitive drift.
