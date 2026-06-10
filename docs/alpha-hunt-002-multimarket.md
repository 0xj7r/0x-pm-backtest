# Alpha Hunt 002: Risk Overlays, Multi-Market, and the June Holdout

**Date:** 2026-06-10 (overnight program)
**Follows:** docs/alpha-hunt-001-base.md (champion: exogenous BSM fade, vol=3600s, BTC-5m)
**Protocol:** every selection on a tune window only; one verification per decision; June 1-7 sealed until the single final frozen run.

## 1. Risk overlays (tuned May 7-18, 8 configs)

Grid: exit-at-book {hold, 30s, 60s, 120s} x calm-stand-down {on, off}, selection by daily Sharpe with an EV floor:

| config | total | daily mean | daily sd | Sharpe |
|---|---|---|---|---|
| **exit 30s, no skip** | $21,476 | $1,790 | $822 | **2.18** |
| exit 60s, no skip | $22,780 | $1,898 | $929 | 2.04 |
| exit 120s, no skip | $25,372 | $2,114 | $1,067 | 1.98 |
| hold, no skip | $22,337 | $1,861 | $1,607 | 1.16 |

Chosen: **exit 30 s after fill** (sell at the book, spread-crossed, depth-walked). The trade is convergence-capture, not lottery-holding: hit rate jumps to 64-66% and daily variance halves. Calm-stand-down was *rejected by the tune data* (calm trades were net positive over the full window; May 25 was an exception, and cutting the whole regime costs more Sharpe than it saves) — the regime story stays in reporting, not gating, for now.

**Verification (one run, May 19-28):** +$12,152, hit 59.3%, worst day **-$189** (hold-to-resolution worst day was -$612), 9/10 days green, Sharpe 1.42. The exit rule gives up ~14% of EV for a 3x smaller tail.

## 2. Multi-market (metadata manifests, inferred labels; tune May 21-24, frozen test May 25-28)

| family | tune (best thr) | frozen test | verdict |
|---|---|---|---|
| btc-updown-5m | +$6,494 (0.16) | **+$2,179**, 709 trades, 52.5% | edge confirmed on a second split |
| btc-updown-15m | +$1,499 (0.16) | +$229, 81 trades, 60.5% | positive, small sample — promising |
| eth-updown-5m | +$636 (0.16) | +$91, 220 trades | ~flat — no validated edge |
| eth-updown-15m | +$157 (0.08) | -$175, 157 trades | negative — rejected |

The user's instinct was right: the edge does **not** transfer uniformly. It is strongest where our spot feed and the market are deepest (BTC); ETH books appear better-calibrated relative to our ETH belief (ETH LL_book 0.467, the sharpest of all four cells). Per-market recalibration is mandatory and ETH needs its own signal work (or stays out).

### Label integrity

Outcomes for these runs are inferred from the final tape mid (ambiguous finals in [0.45, 0.55] skipped, ~1.3% of markets). Validated against true availability-API outcomes on May 21 (identical day, config, and markets): hit 72.6% inferred vs 73.1% API (~1 trade in 200 differs) and inferred P&L is *lower* (-$796) — inference is conservative, not flattering.

## 3. June 1-7 sealed holdout — the BTE leak window

One run, everything frozen beforehand (BTC-5m, thr 0.16 from the May 21-24 tune, exit 30s, 150 ms): 

**+$11,726 on 853 trades across 1,989 markets, hit 66.7%, all 7 days positive (worst +$575, best +$2,432), daily Sharpe 2.14.**

This is the same late-May/June regime in which the deployed BTE strategy was losing money. No June information was used in any selection step. Worst-case label-inference bound: the 27 skipped ambiguous markets could subtract at most ~$1.3k.

(Only BTC-5m exists in the local June cache; the other families' June runs were skipped for lack of data.)

## 4. What this does and does not establish

**Established:** a leakage-free exogenous belief plus a 30s convergence-capture exit has positive, latency-robust, out-of-sample EV on Polymarket BTC-5m across three disjoint windows (May 19-28, May 25-28 re-split, June 1-7), with single-digit-percent worst days at $50 clips.

**Not established:** performance outside volatile regimes (Feb-Apr S3 validation pending AWS re-auth — May/June structurally favor fading); live capture fraction (the sim wins races to stale quotes that real competitors contest — must be measured live at minimum size); ETH/15m edges (weak or absent as-is); capacity beyond ~$50-100 clips.

## 5. Deployment frame ($1K starting capital)

At $1K: $50 clips = 5% of equity per market — too big. $25 clips with a 3-concurrent-market cap (~7.5% peak exposure) halves the June expectation to roughly +$5-6k/month *if* live capture matches the sim — which it will not fully; the honest path is a $10-25-clip live pilot measuring fill ratio vs sim before believing any monthly number. Engine-side prerequisites before that pilot: portfolio-level daily loss cap, per-market exposure cap, loss-streak cooldown (all exist in br2's harness already), plus the agent consuming `pm-alpha` for its belief.

## 6. Run inventory

All artifacts: `data/runs/alpha/overnight/` (8 risk configs + verify + 4 family tunes + 4 family tests + June finale, each with JSON + trade dumps + logs); configs disclosed in each JSON. Manifests: `data/manifests/multimarket/`.

## Appendix: momentum family (Family A) — rejected at the tune gate

Momentum drift added to the belief (lookback {60, 300}s x weight {0.5, 1.0}, on the exit-30s champion, May 7-18): best variant +$1,356 vs the +$21,476 no-momentum baseline; the 60s variants were -$4.3k to -$5.9k. Rejected without spending test-window data. Interpretation: the validated edge fades book overreaction to spot moves; a drift term aligns the belief with the move and erases the disagreement signal. Momentum remains a candidate for a separate directional strategy in trending regimes (the Feb-Apr profile), not an overlay on this one.

## Appendix 2: real NO-token ladders (2026-06-10)

The Down-token books were already cached on disk; the loader now merges them so NO entries/fills/exits use the real ladder (synthetic `1 - yes` retired, fallback only). Frozen-config reruns:

| window | synthetic NO | real NO | coverage |
|---|---|---|---|
| May 25-28 test | +$2,179 | +$2,167 | 100% |
| June 1-7 holdout | +$11,726 | **+$12,039** (862 trades, 67.1%) | 99.8% |

The validated edge is unchanged-to-slightly-better under the corrected execution model — synthetic NO was not flattering it.

**Pair-cost discovery** (now observable): the minimum same-tick `yes_ask + no_ask` dips below $1.00 in 98% of May test markets (mean min 0.9657) and 99.4% of June markets (mean min 0.9601). A sub-parity two-leg buy redeems at $1 regardless of outcome. Tradeability depends on window duration, touch size, and two-leg fill latency — the next measurement module. This is the quantitative basis for the directional + pair-cost (bonereaper-style) strategy family.
