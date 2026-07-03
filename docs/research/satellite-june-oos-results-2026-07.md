# Satellite candidates: June out-of-sample results

**Date:** 2026-07-03
**Method:** June 13-30 Telonex data (freshly ingested for ETH-5m and BTC-15m),
canonical accounting (fee-net, latency 250ms, $50 clips), frozen BTC-5m fade
config transplanted per book (correct per-asset perp symbol), each candidate
run ungated AND under the pre-registered stability gate. Runs in
data/runs/multibook_june/. June is burned for selection generally, but these
books were never selected on June, so this is clean OOS for them.

## Results

| candidate | config | n | NET | hit | verdict |
|---|---|---|---|---|---|
| **BTC-15m** | frozen | 1,059 | **+$4,586** | 60.4% | **VALIDATED-OOS (first satellite to pass)** |
| BTC-15m | gated | 888 | +$3,537 | 56.8% | gate costs ~23% recorded (expected; value is live-only) |
| ETH-5m | frozen | 1,931 | -$3,528 | 54.9% | **REJECTED** |
| ETH-5m | gated | 1,519 | -$4,461 | 49.8% | gate does NOT transfer to ETH; rejected harder |
| BTC-5m late-favourite lane (align>=0.70, last 120s, sigma>=4) | | 395 | -$612 | 76.5% | **REJECTED (third strike)** |

## BTC-15m detail (the winner)

12/15 trading days green, worst day only -$318, and day-correlation to the
BTC-5m fade of **r=+0.23**: it makes money on different days than the core
book, which is exactly the all-weather property the book strategy calls for
(memory: all-weather-book). June economics ~$300/day at $50 clips fee-net,
roughly a quarter of the 5m book's, on 6x fewer markets.

Path to deployment (after the core BTC-5m rollout completes): the 15m book is
subject to the same D2 physics, so it needs its own shadow stream + twin +
realization measurement before any live allocation. It shares the engine and
infra; the incremental work is a second shadow config and soak.

## ETH-5m: rejected with corroboration

Our frozen config loses on ETH June OOS, and the stability gate makes it
worse (the BTC-derived gate removes ETH's better trades; whatever edge ETH
has lives in the fast entries the gate excludes). External corroboration:
ce25, the most profitable known operator in these markets, LOSES on its own
ETH book (-$2.5k over May 1-Jun 12, from its 752k-fill archive) while making
+$45.2k on BTC. ETH-5m is dead for this strategy family; do not revisit
without a fundamentally different belief.

## Late-favourite lane: rejected, third strike

76.5% hit rate and still net negative: buying 0.70+ favourites earns pennies
per win and pays full price per loss, minus fees. This is the same knife-edge
that produced 3 straight losing live sessions in June and the PARTIAL-with-
concerns audit verdict. The lane is now REJECTED on OOS evidence; remove it
from the candidate queue.

## ce25 decomposition (context for multibook)

Per-asset net cash flow May 1-Jun 12 from its on-chain archive: BTC +$45.2k,
SOL +$6.8k, XRP +$0.5k, ETH -$2.5k; day-correlations to BTC of r=+0.12 to
+0.28. Its breadth is a variance tool more than a profit tool, and its profit
is BTC. Its trade profile (spend across all price bands, 47% in 0.55-0.85)
differs from our one-sided fade, so its SOL profit does not contradict our
SOL-fade rejection.

## Updated book plan

1. Core: BTC-5m fade through the rollout gates (unchanged).
2. First expansion: BTC-15m via its own shadow+twin soak (this doc's result).
3. Rejected permanently absent new theses: ETH-5m, late-favourite lane,
   everything in the satellite audit's REJECTED column.
4. SOL/XRP: untested for OUR strategy shape on June; low priority, ce25's
   profile suggests a different trade is needed there.
