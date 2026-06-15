# F3: Perp basis momentum for the BTC-5m fade

Verdict: **SIGNAL** (as a sizing tilt, not a hard gate)

Question: is the widening/narrowing of the Binance perp minus spot basis informative for our
BTC-5m fade trades beyond the static 50/50 perp price blend?

## Setup

- Trades: `data/runs/alpha/feemin/base.trades.jsonl` (W3 fit, 2026-05-07 to 05-18, 3,046 trades),
  `W1_base.trades.jsonl` (2026-02-12 to 03-31, 7,678), `W2_base.trades.jsonl` (2026-04-01 to 04-30, 5,418).
  All `window_secs == 300`; fee-net `pnl`; nothing on or after 2026-05-19 touched.
- Basis series: per-day 1s last-trade price from
  `data/cache/raw/binance/.../channel=agg_trades` (spot) and `channel=futures_agg_trades` (perp),
  forward-filled, basis_bps = (perp/spot - 1) * 1e4. Note the parquet `transact_time_ms` column
  is actually microseconds.
- Per trade at decision time t: d60 = basis(t) - basis(t-60s), d300 = basis(t) - basis(t-300s).
  Signed momentum s = side_dir * d (side_dir = +1 Yes, -1 No). s > 0 means the perp is leading
  in the direction of our entry ("agree"); s < 0 means it leads against us ("disagree").
- Coverage: W3 3046/3046 matched, W1 7678/7678, W2 5417/5418 (1 skipped, missing basis lookback).
- Scripts: `scripts/f3_basis_momentum.py` (extraction), `scripts/f3_basis_analysis.py` (splits,
  W3 threshold sweep), `scripts/f3_tilt_eval.py` (frozen rule validation).

## Raw basis direction alone is uninformative

Widening vs narrowing without conditioning on side is flat everywhere (W3: +6.34 vs +7.39 per
trade; W1: +3.27 vs +3.01; W2: +5.39 vs +5.66). The signal is entirely in the interaction with
our entry side.

## Side agreement with d60 momentum: large, replicates in both validation windows

| window | bucket | n | pnl/trade | total | hit | t (agree-disagree) |
|---|---|---|---|---|---|---|
| W3 (fit) | agree | 1,849 | +9.02 | +16,681 | 0.660 | |
| W3 (fit) | disagree | 1,190 | +3.57 | +4,246 | 0.547 | +5.89 |
| W1 (OOS) | agree | 4,400 | +4.02 | +17,669 | 0.580 | |
| W1 (OOS) | disagree | 3,273 | +1.96 | +6,416 | 0.533 | +4.67 |
| W2 (OOS) | agree | 3,061 | +6.65 | +20,357 | 0.604 | |
| W2 (OOS) | disagree | 2,337 | +4.05 | +9,469 | 0.559 | +3.73 |

Agree trades earn 1.6x to 2.5x the per-trade pnl of disagree trades with a 4-11pt hit-rate gap,
in all three windows. Medians confirm it is not outlier-driven (W3 +6.17 vs +1.50; W1 +2.36 vs
+0.95; W2 +4.00 vs +2.22). Magnitude is monotonic within the agree bucket in every window
(|s| terciles, e.g. W1 avg +2.78 / +3.83 / +5.44), and roughly monotonic-down within disagree.

The 300s horizon shows the same sign but is weaker (t = +2.98 / +2.09 / +2.56); d60 dominates,
so the feature spec uses the 60s lookback only.

Because d60 is the perp return minus the spot return over the same 60s, common momentum cancels;
this is cross-venue lead information, not a re-measurement of spot momentum, and the static 50/50
level blend only captures the current spread level, not its rate of change.

## Hard gate fails; sizing tilt works

Disagree flow remains net positive in every window (+3.57 / +1.96 / +4.05 per trade), so skipping
it discards profitable trades. The W3 threshold sweep confirms no skip threshold on s60 (or s300)
improves total NET in the fit window; the best skip variants only approach break-even on the
dropped flow. Same lesson as F2: binary gates on positive-expectancy flow lose NET.

Frozen rule (chosen on W3, sign-only, no fitted threshold): stake x1.25 when s60 > 0, x0.75 when
s60 < 0, x1.0 when s60 = 0, linear pnl scaling.

| window | base NET | tilt NET | delta | avg stake mult | uniform-scaling equivalent |
|---|---|---|---|---|---|
| W3 (fit) | +20,974 | +24,083 | +3,109 (+14.8%) | 1.054 | +5.4% |
| W1 (OOS) | +24,124 | +26,937 | +2,813 (+11.7%) | 1.037 | +3.7% |
| W2 (OOS) | +29,763 | +32,486 | +2,722 (+9.1%) | 1.033 | +3.3% |

The tilt deploys 3-5% more average stake but earns 9-15% more NET, roughly 3x what uniformly
scaling all stakes by the same average multiplier would earn. The improvement survives in both
out-of-window periods at better than 2x the exposure-matched baseline.

## Harness feature spec

- New belief/sizing input in pm-alpha: `basis_mom_60s_bps` = basis_bps(t) - basis_bps(t-60s),
  where basis_bps = (perp_last / spot_last - 1) * 1e4 from the existing PerpState futures trades
  (already loaded via `--perp-symbol BTCUSDT`) and the spot series; 1s last-price, forward-fill,
  60s lookback warmup required before trading.
- Signed per-decision feature: s60 = basis_mom_60s_bps for Yes entries, negated for No entries.
- Sizing hook (default off): `--basis-mom-tilt <agree_mult> <disagree_mult>`, validated here at
  1.25 / 0.75 applied to the per-trade stake before existing caps; do NOT implement as an entry
  gate. Respect existing per-trade and bankroll caps after multiplication.
- Candidate refinement (not validated here, future work): scale the multiplier with |s60| given
  the monotone tercile pattern, and revalidate at $1k/$2.8k live sizing with cap interaction.

## Caveats

- Linear pnl scaling assumes stake multipliers do not move fills; at live $2,800 bankroll and
  current clip sizes this is plausible but should be confirmed in a harness run with real cap
  interaction before deployment.
- Tilt asymmetry (avg stake mult > 1) means slightly more capital at risk; the uniform-scaling
  column shows the edge is not explained by that extra exposure.
- d300 not used; adding it gave no incremental case worth a second parameter.
