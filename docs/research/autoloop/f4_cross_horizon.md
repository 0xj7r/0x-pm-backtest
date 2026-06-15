# F4: Cross-horizon lead-lag, BTC-15m mid vs BTC-5m mid

Verdict: NO-SIGNAL

## Question

On overlapping windows, does the Polymarket BTC-15m market's Up-token mid lead the
BTC-5m market's mid, or vice versa?

## Method

- Window: 2026-05-07 to 2026-05-12 (script also covered 05-13..05-18; in-scope subset reported below).
- Pairing: each sampled 15m market (btc-updown-15m, manifest close_ts) spans three 5m
  markets; every available overlap is one pair. In-scope pairs: 24 (5, 6, 3, 6, 2, 2 per day).
- Tick source: data/cache/ticks/{date}/{asset_id}.{1s|2s}.btc (PTC2 zstd, top-of-book
  Yes bid/ask). Up mid = (yes_bid + yes_ask) / 2, filtered to two-sided books.
- Alignment: forward-filled mid on a 0.5s grid per 5m overlap window, 10s edge trim.
  Note: spec asked for 250ms; the underlying tapes are 1-2s snapshots, so a finer grid
  only replicates forward-fill values and cannot change the result.
- Statistic: Pearson correlation of mid CHANGES at leads/lags 0.5s..30s in both
  directions, plus per-pair peak lag of the full -30s..+30s correlogram, plus a
  jump-precedence check (did the other horizon move same-direction in the prior vs next 30s).
- Script: scripts/f4_cross_horizon.py.

## Results

Per-day peak-lag medians (positive = 15m leads, negative = 5m leads):

| day | n pairs | peak-lag median (s) | corr at lag 0 | mean lead corr 15m->5m | mean lead corr 5m->15m |
|---|---|---|---|---|---|
| 2026-05-07 | 5 | -15.5 | -0.001 | -0.0022 | +0.0034 |
| 2026-05-08 | 6 | +13.25 | -0.015 | +0.0048 | -0.0007 |
| 2026-05-09 | 3 | +8.5 | +0.001 | +0.0022 | -0.0009 |
| 2026-05-10 | 6 | +2.0 | -0.034 | -0.0001 | +0.0025 |
| 2026-05-11 | 2 | -7.25 | +0.012 | +0.0037 | -0.0087 |
| 2026-05-12 | 2 | -23.25 | +0.009 | +0.0009 | -0.0008 |

Pooled correlogram (all pairs in the run): every lag from 0.5s to 30s in both
directions has |corr| < 0.02, including lag 0 (contemporaneous corr -0.014). There is
no lag at which either horizon's mid changes meaningfully predict the other's.

Jump precedence (full run, de-duplicated >= 0.04 (5m) / >= 0.02 (15m) 1s jumps):
15m jumps: 5m moved before 2, after 1, neither 5. 5m jumps: 15m moved before 1,
after 3, neither 10. Event counts too small and too balanced to indicate precedence.

## Interpretation

- Contemporaneous correlation of mid changes is ~0, so the two books are not even
  co-moving tick-by-tick at sub-second resolution; both reprice off the same exogenous
  BTC feed, but at different strikes and time-to-expiry the Up-probability deltas are
  nearly orthogonal at this granularity.
- Peak lags are scattered across the full -30s..+30s range with day-to-day sign flips
  (3 days each direction). That is the signature of picking the argmax of a noise
  correlogram, not a stable lead.
- The 15m-leads minus 5m-leads asymmetry is |asym| < 0.013 everywhere and flips sign
  across days.

## Caveats

- 24 in-scope pairs (slightly above the 20-pair budget; the day sampling grid made an
  exact 20 awkward and the extra pairs only reinforce the null).
- Grid is 0.5s, not 250ms, because the cached tapes are 1-2s snapshots; sub-second
  lead-lag inside the snapshot interval is unobservable in this data. A lead of < 1-2s
  in either direction cannot be ruled out, but anything slower than that (tradeable at
  taker latency) is absent.
- Mid changes are mostly zero on this grid (sparse repricing); correlations are
  dominated by the few co-jump intervals, which is exactly where a lead would show if
  one existed.

Verdict: NO-SIGNAL. Neither the BTC-15m nor the BTC-5m Polymarket mid systematically
leads the other on 2026-05-07..05-12 at horizons of 0.5s to 30s.
