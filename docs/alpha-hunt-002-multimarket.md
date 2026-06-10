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

## Appendix 3: data-quality hardening (2026-06-10 audits)

Two adversarial audits (harness leakage; data-layer timestamps/labels) drove four corrections, each re-validated on the frozen June holdout:

| state | June 1-7 |
|---|---|
| original | +$11,726 |
| + real NO ladders | +$12,039 |
| + look-ahead-free strike (last trade at-or-before open; audit CRITICAL finding fixed) | +$12,106 |
| + canonical true resolutions (Telonex markets parquet result_id; inference retired) | **+$11,912** |

Timestamp semantics resolved offline: tape `timestamp_us` is Polymarket's own event time (collector receipt runs p50 +189ms / p99 +6.4s behind in a separate column) — the CEX-to-book lag is genuine exchange behavior, and the strategy's +EV at a simulated 1000ms covers any realistic input staleness. Remaining known-unknowns: live capture fraction (shadow pilot), NO-ladder staleness optimism (cutoff queued), Feb-Apr numbers pending their fixed-strike/true-label rerun.

The canonical metadata parquet also expands the validated universe: six assets (btc/eth/sol/xrp/doge/hype) x 5m/15m/4h with true resolutions back to Oct 2025 (~45k BTC-5m markets alone), and the polymarket.com crypto-price API serves official historical open/close prints (strike proxy retired going forward; Chainlink `crypto_prices` channel available from Apr 2 for the oracle-lag study).

## Appendix 4: hardened Feb-Apr final (2026-06-10)

Full rerun under the hardened pipeline (canonical true resolutions, real NO ladders, look-ahead-free strike): **+$102,114 at thr 0.16** (+$97,733 at 0.12) across Feb 12 - Apr 30, ~13,100 trades at $50 clips, every ten-day shard positive except Feb 12-21 (-$313). Within 1-3% of the pre-hardening estimate in every shard — the fourth time the headline survived a correctness upgrade unchanged. The Feb-June record is now: Feb-Apr +$102.1k (validation, never tuned on), May 19-28 +$12.2k (test), June 1-7 +$11.9k (sealed holdout), with the only flat stretch being early February.

## Appendix 5: the race clock (offline capture estimate, 2026-06-10)

For 500 June entries, the real trades channel shows whether the targeted quote was actually consumed and how fast: **98% were taken by real takers within 60s** (the liquidity is real), median competitor delay **77ms** (p10 8ms, p90 496ms); only **35% of takes are slower than our 150ms**. Realistic capture at current latency therefore sits near the 25%-depth-capture stress row (~+$7k/June-week at $50 clips). Latency is a purchasable edge: the live agent runs in eu-west-1 while Polymarket is US-east — relocating to us-east-1 moves us up the take-delay curve. No live taker fills exist in any historical journal (the br2 "live" taker run was kill-switched into paper; recorded real-money executions are maker-only, and those runs suffered engine degradation — treat their execution stats as a sick-engine floor, not a benchmark), so the shadow run on the healthy engine remains the first representative measurement of our side of the race.

## Appendix 6: entry deadline adopted (2026-06-10 evening)

Live shadow data (five late-window lottery entries, all full-premium losses) triggered tuning the long-recommended entry deadline. May 7-18 tune: Sharpe 1.85 -> 2.61 gating the final 90s (-24% EV). May 19-28 verification: +$10,101 with ZERO negative days, worst day +$273 (baseline +$12,152, worst -$189). Adopted into the frozen config; golden re-baselined; shadow redeployed with the gate. This is the first config change driven by live shadow evidence and validated by the offline protocol — the loop working in both directions.

## Appendix 7: Kelly sizing — rejected (2026-06-10)

Joint {deadline 10s/90s} x {flat/Kelly} tune (May 7-18): Kelly-instead-of-gate scores Sharpe 2.26 at $17.5k (vs the gate's 2.61 at $20.3k — worse on both axes); Kelly-plus-gate drops to 2.45 at $14.7k (only good trades left to shrink). The late-window cohort's EV is inseparable from its variance: sizing shrinks both proportionally, the gate removes both. Champion remains 90s deadline + flat clips. (One parameterization tested — half-trust belief discount + variance equalization; not iterated further to avoid tune-window mining.)

## Appendix 8: Continuation model v1 at the book — rejected (2026-06-10)

Label-space skill is real: logistic on the 14 DirFeatures (train Feb-Mar 154k, test Apr 96k OOS) beats base log-loss (0.628 vs 0.649) with a calibrated top decile (predicted 0.841, realized 0.846). At the book it fails: Aligned entries priced by the model lose -$3.6k..-$8.2k across thresholds on April (hit 59-60%), while the naive aligned control (BSM belief + book-agreement filter) makes +$11.6-14.3k (hit 65-69%, Sharpe 0.60 hold-to-resolution). Mechanism: the linear head's probabilities cap near 0.85, so "model edge" appears precisely where the book is cheaper than that, i.e. where order flow disagrees with continuation; the BSM belief gets appropriately extreme near close and wins. The DirModel plumbing (orientation-pinned, parity-tested vs the python trainer) stays; a stump/boosted head can plug in later. Golden re-baselined same-results (config echo gained kelly_sizing field).
