# Position management: comprehensively falsified (2026-07-08)

Triggered by the bonrepear wallet study (0xeeb...a30: two-sided taker
accumulation, +0.76% of ~$635k/day churn, pair-hedged). Question: can the
fade adopt its pair-hedging to survive whipsaws? Answer: NO, in every form
tested. Each clip of the fade is an independent positive-EV bet; managing
"positions" degrades it.

All runs at truthful latency (1250ms), June 13-30 (whipsaw) + May 7-28
(control). Baselines: June +$2,506, May +$17,342. Knobs merged default-off
in commit ced568e1 (kept for reproducibility).

## E2 pair-lock hedge (buy opposite side when combined cost locks a margin)

| margin | June vs base | May vs base |
|---|---|---|
| 0.02 | -$4,415 | -$10,372 |
| 0.05 | -$3,159 | -$10,141 |
| 0.10 | -$3,240 | -$9,846 |

Mechanism: the trigger (cost_A + ask_B <= 1 - margin) can only fire when
side A is WINNING (opposite ask falls). It locks micro-margins on
62%-probability winners while losers still ride to $0. Anti-selection by
construction. The mirror (hedge while losing) means buying pairs >$1,
which E1 showed lose as markets. Hedging is dead in both directions.

## E3 cut-loser (sell held leg at bid when worth < p of cost)

| cut at | June vs base | May vs base |
|---|---|---|
| 0.2 | -$2,318 | +$884 |
| 0.3 | -$2,671 | +$187 |
| 0.5 | -$3,424 | +$481 |

Selling a 5m binary at a 0.1-0.2 bid sells the re-reversal option right
before it pays. Fails June decisively; regime-fragile; dead.

## Max-pair-cost gate (block the 2nd fade when combined cost > $1)

E1 showed both-sides markets with combined cost > $1 lost -$3,294 (June) /
-$4,677 (May). But the leg-2-only counterfactual (what blocking actually
removes) is PROFITABLE: +$651 June (98/133 green), +$1,575 May. The whole-
market loss is leg 1's sunk cost; leg 2 (the post-flip fade) is right 74%
of the time and recovers. Blocking it costs money in both months. Dead.

## E1 facts worth keeping

- Both-sides (whipsaw) markets: 7.1% of June markets, 11.3% of May.
- Hold-to-redemption already IS pair-locking: as-traded == perfect-pair
  accounting exactly ($0.00 diff across 609 markets). Execution cannot
  improve a pair already held to settlement.
- The single-side losing pool (-$60k June / -$114k May at $50 clips) is the
  cost of doing business for a 58-62%-hit hold-to-redemption strategy; the
  battery shows it cannot be managed away post-entry at our timescale.

## Why bonrepear works and we cannot copy it

Volume business: ~20k trades/day, $4 median clip, +0.76% of churn, needs
~$635k/day gross and fee terms that make thin margins survivable. Their
edge is infrastructure + fee economics, not signal. Our edge is signal
(fat mispricings, 12c threshold); the two do not compose at our scale.

## Standing conclusion

Entry-side selection (which trades to take: v1 gate pending) and execution
speed (fast engine) remain the only levers that survive cross-regime
falsification. Post-entry position management joins regime gates and the
dwell gate in the falsified pile. The scoreboard: 5 ideas tested against
May+June at truthful latency, 4 dead, 1 pending (v1, judged ~Jul 11-12).
