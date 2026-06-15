# F5: Binance futures liquidation cascades vs 5m fade

**Verdict: NO-SIGNAL.** True liquidation cascades show no short-horizon spot continuation, and fade losses do not concentrate after cascades. Used as a filter it would have removed +$6.5k of profit; post-cascade is actually the fade's best regime, not its worst.

Date: 2026-06-12. Window studied: 2026-02-12..2026-05-18. Nothing dated 2026-05-19 or later was touched.

## Data acquisition

- **data.binance.vision has no liquidation data for this period.** The S3 listing under `data/futures/um/daily/` contains only aggTrades, bookDepth, bookTicker, klines, indexPriceKlines, markPriceKlines, premiumIndexKlines, metrics, trades. The `liquidationSnapshot/` prefix exists but is empty (no symbol directories, no keys); same for monthly. Verified directly against the S3 listing endpoint on 2026-06-12.
- **Ground truth: Tardis.dev free first-of-month tapes**, `data/external/liquidations/liq_{2026-03-01,04-01,05-01}.csv.gz` (binance-futures BTCUSDT forceOrder stream; 1,483 / 1,090 / 887 events, full 24h coverage each). Caveat: since 2021 Binance throttles the forceOrder stream to at most one event per second per symbol, so notional is a floor, but cascade *timing* is preserved.
- **Proxy coverage (25 days, all zips integrity-checked):** futures aggTrades and spot 1s klines in `data/external/liquidations/{aggtrades,spot1s}/` for Feb 15, 16, 23, 25, 26; Mar 01, 02, 13, 16, 20; Apr 01, 09; May 01, 07..18. Total on disk ~560MB, well under the 3GB cap. Derived 10s taker-flow files in `derived/agg10s_*.csv`.
- Trades: `data/runs/alpha/feemin/{base,W1_base,W2_base}.trades.jsonl`, `window_secs=300` only; 4,933 trades land on the 24 covered days that have trades (May 01 has no trades).

Script: `scripts/f5_liquidations.py`.

## Phase 1: do true cascades predict continuation?

10s buckets of liquidation notional over the 3 ground-truth days (25,920 buckets, 1,984 nonzero). Cascade = bucket total > 99th percentile of all buckets ($31.6k; nonzero-only p99 is $486k; max $11.9M). 260 cascade buckets, 196 episodes after collapsing runs within 30s. Signed forward spot return (positive = continuation in the liquidation direction: buy-side liq = shorts blown out = up-pressure):

| horizon | bucket-level mean (bps) | t | episode-level mean (bps) | t |
|---|---|---|---|---|
| 60s | +0.13 | +0.19 | +0.03 | +0.04 |
| 180s | +1.32 | +0.93 | +0.93 | +0.59 |
| 300s | -0.47 | -0.33 | -0.54 | -0.34 |

Split by direction (episodes): buy-side (n=114) and sell-side (n=82) are both indistinguishable from zero at every horizon (|t| <= 0.5). Baseline random non-cascade buckets: -0.1 to -0.4 bps, also null. **No continuation signal at 60/180/300s.**

## Phase 2: aggTrades proxy (to cover all 25 days)

Proxy cascade = 10s taker notional > 99th percentile across all 25 days ($14.3M/10s), direction = dominant taker side. Validation on the 3 ground-truth days: flags 97/260 (37%) of true cascade buckets within one bucket, with 90% direction agreement (87/97). Recall is limited by the throttled ground-truth stream and by liquidations being a subset of taker flow; precision-style sanity holds (proxy episodes on true days show +3 bps at 180/300s, t=1.7, not significant). 2,160 proxy buckets across 25 days (~86/day).

## Phase 3: do fade losses cluster within 5 min after cascades?

Trade flagged if any proxy cascade bucket ended within 300s before `decision_ts`. "vs-cascade" = trade side opposes cascade direction (No after buy-cascade, Yes after sell-cascade).

| run | no-cascade n / win% / avg pnl | post-cascade n / win% / avg pnl |
|---|---|---|
| base (May 07-18) | 2,666 / 60.1% / +6.35 | 380 / 71.8% / +10.64 |
| W1_base (Feb-Mar) | 1,187 / 57.1% / +3.99 | 410 / 62.7% / +5.46 |
| W2_base (Apr) | 239 / 60.7% / +3.09 | 51 / 64.7% / +4.53 |
| ALL | 4,092 / 59.2% / +5.48 | 841 / 66.9% / +7.74 |

- Flagged trades are 17.0% of trades but only **13.4% of total losses**. Losses are *under*-represented after cascades.
- Post-cascade trades outperform: win rate 66.9% vs 59.2% (z = +4.3), avg pnl +7.74 vs +5.48 (t = +2.35). Consistent across all three regime windows.
- Even vs-cascade trades (fading directly into the liquidation direction, n=257) are the single best subset: +11.08/trade, 65% win.
- Filter economics: dropping all post-cascade trades removes +$4,043 (base), +$2,238 (W1), +$231 (W2) of profit. Dropping only vs-cascade trades removes +$1,900 / +$787 / +$160. Every variant is value-destroying.

## Interpretation and caveats

The 5m fade's losses come from sustained crossed-mid trends (per the convexity work), not from liquidation bursts. Cascades on this tape are over within seconds and mean-revert by the 5m mark, which is exactly the environment the fade is built for; that is why post-cascade trades outperform. The post-cascade outperformance is conditioned on trades the engine already chose to take, so it is evidence the existing entry logic already harvests these moments, not evidence of incremental alpha worth a new gate. Caveats: only 3 days of true liquidation ground truth (first-of-month days, possibly calmer than average), throttled forceOrder stream understates cascade magnitude, proxy recall 37%. None of these change the sign of the trade-join result, which uses 24 days and 4,933 trades.

**Do not add a liquidation-cascade filter. Do not prioritise a cascade entry signal; if revisited, frame it as a sizing-up candidate, not a veto.**
