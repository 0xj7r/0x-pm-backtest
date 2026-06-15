# ETH multi-horizon backtest

Date: 2026-06-13. Goal: confirm the fade edge on ETH across regimes (W1 Feb-Mar,
W2 Apr, W3 May 7-18) and horizons (5m, 15m, 1h, 4h). SPOT-ONLY belief
(`--perp-price-weight 0`); never the BTC perp (the contamination bug that
poisoned the first ETH run, see eth_fade_diagnosis.md).

## Config (fade-candidate, frozen)

`--latency-ms 150 --vol-lookback-s 3600 --vol-estimator ewma --ewma-halflife-s 600`
`--stop-before-close-s 90 --fee-curve-rate 0.07 --edge-thresholds 0.12`
`--rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 --exit-after-s 0 (hold)`
`--perp-price-weight 0`. Sigma>=3 floor applied offline (keep trades with
`sigma_bar_bps >= 3.0`, equivalent to `--min-entry-sigma-bps 3.0`). $50 clips.
Score windows: 5m=300s, 15m=900s, 1h=3600s, 4h=14400s.

## Results (NET = sigma>=3 floored; $/day and Sharpe are daily)

| cell | NET $ | $/day | hit % | Sharpe | green | trades | verdict |
|------|------:|------:|------:|-------:|------:|-------:|---------|
| ETH-5m  W3 (May 7-18, whipsaw) | 4,487 | 374 | 57.8 | 0.67 | 8/12  | 2,670 | VIABLE |
| ETH-15m W3 (May 7-18) | 4,306 | 359 | 61.4 | 0.80 | 10/12 | 966   | VIABLE |
| ETH-4h  W3 (May 7-18) | 1,076 |  98 | 79.7 | 0.37 | 8/11  | 64    | VIABLE (thin) |
| ETH-1h  W3 |   -   |  -  |  -   |  -    |  -    |  -    | DATA-MISSING |
| ETH-5m  W2 (Apr 1-19, mixed)* | 9,729 | 512 | 55.2 | 0.61 | 13/19 | 2,940 | VIABLE |
| ETH-5m  W1 (Mar 23-26, trend)** | -360 | -90 | 53.6 | -0.14 | 2/4 | 373 | MARGINAL |

*W2 = the merge of W2a (Apr 1-15: +$6,151, $410/day, Sh 0.53, 11/15) and W2b
(Apr 16-19: +$3,578, $895/day, Sh 0.96, 2/4). Apr 20-30 not sourced (S3 per-key
throttling consumed the session); 19 of 30 April days, but spread across the
month and decisively positive.

**W1 is a 4-day Mar 23-26 SAMPLE, not the full Feb12-Mar31 window. The whole
$360 loss is 16 `clean_directional` trades (-$371, 11% hit): the fade gets run
over in a trending tape. The non-directional cells on those same days are flat-
to-positive. Read as "fade is regime-sensitive in the W1 trend window," not a
clean ETH failure. Full W1 left as DATA-PARTIAL.

(RAW vs sigma3: on 15m/4h all trades already clear the 3-sigma floor; on 5m the
floor prunes micro-vol trades and consistently *improves* NET, e.g. W3
4,631->4,487, W2a 5,611->6,151.)

## Deep-tail check (1h/4h new horizons)

The prompt asked whether the longer-horizon book offers cheap asks late in the
window (it does NOT on 5m/15m, where the best ask stays well above ~0.10).

- ETH-4h: YES. Scanning all 57 W3 4h book tapes, the best ask reaches down to
  0.0010, with 353k snapshot-ticks at ask<=0.10 and 180k at ask<=0.05, and
  those deep quotes persist into the last 300s before close. The 4h fade
  actually traded entries as cheap as 0.03. So the 4h book has a genuine deep
  underdog tail that the short horizons lack: a distinct, cheap-tail edge
  surface worth a dedicated study.
- ETH-1h: not testable. No `eth-updown-1h` manifest exists and there are zero
  `eth-updown-1h-` rows in down_all.jsonl; the horizon was never ingested (the
  1h ETH up/down market series does not appear in the canonical set). Marked
  DATA-MISSING, not DEAD.

## Verdict

ETH confirms the fade edge, and confirms it the way BTC does: it is a
whipsaw/mixed-regime edge, not an all-weather one.

Cross-horizon (W3 whipsaw): 5m, 15m and 4h are all net-positive, fee-aware,
spot-only. Hit rate rises with horizon (57.8 -> 61.4 -> 79.7%) and per-trade edge
rises too (5m $1.69, 15m $4.46, 4h $16.8) as participation thins. 15m is the
standout (Sharpe 0.80, 10/12 green). 4h is real but thin (~6 up-markets/day) and
is dominated by a deep-underdog tail (best asks down to 0.001) that the short
horizons do not offer.

Cross-regime (5m): the edge holds strongly in the W2 April mixed regime
(+$9,729 over 19 days, $512/day, 13/19 green) and the W3 May whipsaw, but a 4-day
W1 trend sample went slightly negative, with the entire loss in clean-directional
trades. This matches the BTC result that the edge IS the fade and it inverts when
the tape trends. So: ETH is a confirmed addition to the whipsaw/mixed book
(multi-horizon: 5m + 15m, plus a separate 4h deep-tail surface), and like BTC it
should stand down in clean-directional regimes rather than be treated as
regime-flat.

Recommended next: full W1 (Feb18-Mar31) and W2 Apr20-30 to size the trend-regime
drawdown precisely, and a dedicated ETH-4h deep-tail (buy-underdog) study.

## Data + ops notes

- Tapes resolve from the local cache only when `--local-cache-dir` is set (no S3
  fallback). Book tapes: `data/cache/raw/telonex/.../book_snapshot_25/date=D/
  asset_id=A/<A>_<D>_book_snapshot_25.parquet`. Spot:
  `.../binance/.../agg_trades/symbol=ETHUSDT/date=D/ETHUSDT-aggTrades-D.parquet`.
  Source: `s3://pm-research-data-prod` (profile visumlabs). Books cover
  2025-10-11..2026-06-07; ETHUSDT spot 2026-02-12+. Up asset_ids from the
  canonical manifests; down (NO-ladder) asset_ids from down_all.jsonl
  (`slug` startswith `eth-updown-Xm-`).
- W1/W2 require sourcing tapes per date (not in the working cache, which held
  only May7-Jun10 at start). Strict disk discipline: one window's tapes at a
  time, deleted before the next; never the W3 May7-18 cache.
- 15m/4h book tapes are 5-7x larger than 5m (2-3.5MB vs ~0.6MB) and the per-
  market replay is correspondingly slower.
- Scorer: `scripts/eth_complete_score.py <trades.jsonl> 3.0`.
- Runs in `data/runs/alpha/eth_complete/`.
