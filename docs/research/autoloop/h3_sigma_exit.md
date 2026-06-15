# H3: Sigma-conditioned exit style (taker-cross vs passive rest-at-mid)

Verdict: **REJECT**

Question: should the exit be switched between taker-cross (base) and passive rest-at-mid
with 60s timeout (midtimeout) based on volatility regime (regime cell or sigma_bar_bps)?

## Setup

- Data: existing paired trades files only, no new backtest runs.
  - W3 (tune): `data/runs/alpha/feemin/base.trades.jsonl` vs `midtimeout.trades.jsonl` (2026-05-07 to 2026-05-18, 3,046 trades each)
  - W1 (OOS): `W1_base` vs `W1_midtimeout` (2026-02-12 to 2026-03-31, 7,678 trades each)
  - W2 (OOS): `W2_base` vs `W2_midtimeout` (2026-04-01 to 2026-04-30, 5,418 trades each)
- All trades `window_secs == 300`; latest decision timestamp 2026-05-18, nothing on or after 2026-05-19.
- Entries verified identical: 1:1 join on `(decision_ts_ns, side, token)` with zero unmatched trades, zero `avg_price`/`shares`/`sigma_bar_bps` mismatches across each pair.
- Per-trade delta = passive pnl minus taker pnl (fee-net `pnl` field).
- Threshold fit on W3 ONLY (grid over observed sigma values, both rule directions), then frozen and applied to W1/W2.

## Per-trade delta by regime cell (passive minus taker, $)

| regime cell | W3 n | W3 mean | W1 n | W1 mean | W2 n | W2 mean |
|---|---|---|---|---|---|---|
| calm_low_vol | 1938 | -0.30 | 1003 | +0.75 | 2006 | +0.17 |
| clean_directional | 16 | -0.67 | 61 | +1.03 | 66 | -3.38 |
| expanded_high_flip | 19 | -1.01 | 225 | +0.79 | 133 | +0.13 |
| expanded_mixed | 1073 | -0.49 | 6389 | +0.98 | 3213 | -0.59 |

The premise (passive wins calm, loses whipsaw) does not hold. In W3 passive loses in
every regime cell including calm_low_vol. In W1 it wins in every cell. In W2 the signs
are mixed and do not match either W3 or W1. The regime label carries no stable
exit-style signal.

## Per-trade delta by sigma_bar_bps bucket (W3 quintile cuts 3.59 / 4.58 / 5.66 / 7.55)

| sigma bucket | W3 mean d | W3 midfill | W1 mean d | W1 midfill | W2 mean d | W2 midfill |
|---|---|---|---|---|---|---|
| Q1 (< 3.59) | -0.36 | 0.847 | +0.07 | 0.879 | +0.39 | 0.897 |
| Q2 (3.59-4.58) | -0.14 | 0.878 | +0.89 | 0.931 | +0.76 | 0.912 |
| Q3 (4.58-5.66) | -0.53 | 0.870 | +1.05 | 0.914 | -0.26 | 0.887 |
| Q4 (5.66-7.55) | -0.72 | 0.875 | +0.60 | 0.914 | -0.79 | 0.880 |
| Q5 (> 7.55) | -0.11 | 0.892 | +1.03 | 0.917 | -0.53 | 0.872 |

No monotone sigma relationship survives across windows: W3 is negative everywhere,
W1 positive everywhere, W2 positive at low sigma and negative at high sigma.

## Mid-fill rate by sigma: not the failure mode

Fill rate is flat in sigma (0.85 to 0.93 in every bucket of every window; top-decile
sigma fills 0.83 to 0.92). Passive does NOT systematically fail to fill in high vol.
The economics are a fill-vs-timeout asymmetry that sigma does not predict:

| window | filled at mid: n, mean delta | timed out: n, mean delta |
|---|---|---|
| W3 | 2657, +3.02 | 388, -23.63 |
| W1 | 7032, +2.95 | 642, -20.97 |
| W2 | 4776, +2.43 | 635, -21.04 |

When the resting order fills, passive gains about +$3/trade over taker; when it times
out it loses about -$21 to -$24/trade. At an 87-92% fill rate this nets roughly to
zero, and the sign of the net flips window to window with adverse-selection conditions,
not with sigma.

## Switching rule: fit on W3, frozen, applied OOS

Best W3 rule: passive when sigma_bar_bps > 11.14, taker otherwise (routes 6.5% of W3
trades passive; W3 total +21,185 vs taker +20,974, a +1% in-sample lift).

| window | switch (frozen) | taker everywhere | passive everywhere | switch beats both? |
|---|---|---|---|---|
| W3 (fit) | +21,185 | +20,974 | +19,836 | yes (in-sample) |
| W1 (OOS) | +27,563 | +24,124 | +31,380 | no (passive wins by +3,817) |
| W2 (OOS) | +29,259 | +29,775 | +28,000 | no (taker wins by +516) |

The frozen rule beats both baselines in neither out-of-window period. Per-window oracle
thresholds confirm there is nothing to recover: W1's oracle wants passive when
sigma > 3.45 (passive almost everywhere) while W2's oracle wants passive when
sigma <= 4.45 (the opposite direction). A direction flip between adjacent windows means
the sigma threshold is fitting noise.

## Verdict

REJECT. Exit style should not be conditioned on sigma_bar_bps or on the regime cell:
the passive-vs-taker delta is unstable in sign across W1/W2/W3 within every regime cell
and every sigma bucket, and the W3-fit switching rule fails both OOS comparisons.

Adjacent finding worth a future H-item: the entire passive-exit question reduces to
predicting the 8-13% of exits that time out (mean cost about -$21/trade vs the +$3/trade
gain when filled). Sigma does not predict timeouts (fill rate is flat in sigma); book
depth or quote stability at exit time might. If a timeout predictor with even modest
skill exists, passive-when-fillable dominates; without one, the choice between
taker-everywhere and passive-everywhere is regime-of-month luck.
