# Data validation, chop-day robustness sweep, regime context

**Date:** 2026-07-02

## 1. Deep-history data validation

| dataset | coverage | verdict |
|---|---|---|
| Binance spot klines-1m | 2,738 days, 2019-01-01 to 2026-06-30 | CLEAN: zero calendar gaps; prices match known history at every sampled point; 21 days short of 1440 rows, all known 2019-2023 maintenance windows (worst 840 rows), none since Mar 2023 |
| Binance futures klines-1m | 2,373 days, 2020-01-01 to 2026-06-30 | CLEAN: zero gaps, every sampled day exactly 1440 rows; spot-futures basis sane |
| Deribit DVOL (BTC IV index) | 1,917 days, 2021-04-01 to 2026-06-30 | CLEAN: known events reproduce exactly (May-2021 crash 156 = series max, FTX 115, Aug-2024 yen-carry 63); 60s resolution from 2025-12-28, 1h before (API depth limit); resolution seam continuous |

## 2. Regime context: is Feb-Jun 2026 representative?

Two answers, both matter:

- **30-day implied vol (DVOL): 23rd percentile of 2021-2026.** The validation
  period is a low-trend regime by macro standards.
- **5-minute realized movement (the fade's actual input): historically
  NORMAL.** Median 5m |return| in the validation window is 7.7 bps vs 7.0-8.7
  in 2019/2020/2024/2025 (only 2021's 13.7 and 2022's 9.2 were higher). The
  sigma >= 3 entry gate passes on ~100% of days in every year since 2019.

Interpretation: low DVOL + normal micro-movement = chop without trends, which
is the fade's home regime. The untested scenario is a 2021-style macro-vol
explosion; sizing (1% fractional) is the protection there, not prediction.

## 3. Chop-day parameter fragility sweep (Jun 16-22, fee-net, latency 250ms)

Grid: edge {0.10, 0.12, 0.14} x lookback {1800, 3600} x sigma {0, 3} x
entry-delay {0, 15, 30}s over the five June chop trading days (Jun 20 =
Saturday, skipped by design). data/runs/june_chop_sweep/.

| config | thr 0.10 | thr 0.12 | thr 0.14 |
|---|---|---|---|
| look1800 delay0 sig3 | +8,278 | +7,951 | +7,938 |
| look3600 delay0 sig0 | +6,438 | +6,680 | +5,979 |
| **look3600 delay0 sig3 (frozen)** | +6,290 | **+6,488** | +5,917 |
| look3600 delay15 sig3 | +4,836 | +5,657 | +5,842 |
| look3600 delay30 sig3 | +4,525 | +5,731 | +5,702 |

**Verdict: plateau, not spike.** Every neighbor of the frozen config is
profitable on the worst week of the period; threshold moves change totals by
under 10%, the sigma floor is near-neutral, and lookback 1800 vs 3600 is the
familiar more-money-less-consistency trade. There is no parameter cliff, which
is the signature of a real in-sample edge rather than an overfit point.

Per-day at the frozen config: even the weakest chop day (Jun 17) is flat, not
catastrophic, at every delay (+252 / -24 / +3). Jun 18, the day live lost
money, is +2,764 on recorded feeds at delay 0 and still +2,351 at delay 15.

**Entry-delay cost (recorded world): ~13% on chop days** (6,488 -> 5,657 at
15s; per-trade $4.89 -> $4.52). Matches the May+June-wide ~11% estimate. The
delay buys nothing on recorded feeds; its entire value proposition is
eliminating the live-only first-seconds coin flips (realization analysis).
The July soak decides whether that trade is worth it.

## 4. Soundness verdict

On recorded feeds the strategy is sound by every test available: green across
TUNE/VERIFY/HOLDOUT and the June backfill, robust to parameter perturbation on
its worst days, operating in a historically normal micro-vol regime, with fees
and latency in the accounting. The open risk is unchanged and is not a
backtesting question: live input divergence in the first seconds of fast
windows (June realization 0.34). That is what the paper soak, the realization
reports, and the dwell telemetry are measuring now.
