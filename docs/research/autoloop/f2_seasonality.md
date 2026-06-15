# F2: Session seasonality gating for the BTC-5m fade

Verdict: **REJECT**

Question: does gating out structurally negative hour-of-day (UTC) or hour x day-of-week
sessions improve fee-net P&L and daily Sharpe out of window?

## Setup

- Data: existing trades files only, no new backtest runs.
  - W3 (tune): `data/runs/alpha/feemin/base.trades.jsonl`, `midtimeout.trades.jsonl` (2026-05-07 to 2026-05-18, 3,046 trades each)
  - W1 (OOS): `W1_base.trades.jsonl`, `W1_midtimeout.trades.jsonl` (2026-02-12 to 2026-03-31, 7,678 trades each)
  - W2 (OOS): `W2_base.trades.jsonl`, `W2_midtimeout.trades.jsonl` (2026-04-01 to 2026-04-30, 5,418 trades each)
- All trades are `window_secs == 300`; verified no trade timestamp on or after 2026-05-19.
- P&L is the fee-net `pnl` field. Daily Sharpe = mean(daily pnl) / std(daily pnl), per window, not annualized. Fully gated days count as zero-pnl days.
- Gate fit on W3 ONLY: exclude a cell if total pnl < 0 AND n >= 30 trades. Frozen gate then applied to W1 and W2.
- Script: `scripts/f2_seasonality.py`.

## Hour-only gate: empty

Every UTC hour is net positive in W3 for both variants (worst: 04h at +$0.59/trade for base,
06h at +$0.92/trade for midtimeout). The hour-only gate excludes zero hours, so it is a no-op
by construction. Nothing to carry out of window.

W3 hour-of-day (UTC), base variant, pnl per trade:

| hour | n | pnl/trade | hour | n | pnl/trade |
|---|---|---|---|---|---|
| 00 | 171 | +5.20 | 12 | 150 | +9.37 |
| 01 | 141 | +4.24 | 13 | 167 | +3.95 |
| 02 | 114 | +5.00 | 14 | 112 | +9.48 |
| 03 | 104 | +5.47 | 15 | 114 | +12.14 |
| 04 | 113 | +0.59 | 16 | 101 | +9.27 |
| 05 | 137 | +2.58 | 17 | 119 | +11.74 |
| 06 | 145 | +0.57 | 18 | 137 | +9.04 |
| 07 | 134 | +6.27 | 19 | 117 | +9.54 |
| 08 | 110 | +8.48 | 20 | 129 | +7.92 |
| 09 | 112 | +5.41 | 21 | 125 | +5.97 |
| 10 | 127 | +5.84 | 22 | 120 | +11.14 |
| 11 | 113 | +7.52 | 23 | 134 | +11.72 |

## Hour x day-of-week gate: fails out of window

W3 has only ~18 trades per (dow, hour) cell on average, so with the n >= 30 floor the fitted
gates are tiny and noise-driven.

Fitted gates (W3, frozen):

- base: exclude Sat 12h (n=33, -$25.8), Sat 13h (n=36, -$97.7), Sun 10h (n=31, -$47.4)
- midtimeout: exclude Fri 23h (-$16.8), Sat 12h (-$48.8), Sat 13h (-$55.5), Sun 00h (-$40.5), Sun 17h (-$3.1)

Results (NET in $, removed_pnl = pnl of trades the gate dropped; positive removed_pnl means the gate cost money):

base, hour x dow gate:

| window | n kept | n removed | ungated NET | gated NET | delta NET | removed pnl | Sharpe ungated | Sharpe gated |
|---|---|---|---|---|---|---|---|---|
| W3 (fit) | 2,946 | 100 | 20,973.7 | 21,144.7 | +171.0 | -171.0 | 1.905 | 1.938 |
| W1 (OOS) | 7,502 | 176 | 24,123.5 | 23,935.1 | -188.4 | +188.4 | 0.849 | 0.871 |
| W2 (OOS) | 5,309 | 109 | 29,775.1 | 29,498.0 | -277.1 | +277.1 | 1.149 | 1.155 |

Survival of W3 improvement: W1 -110%, W2 -162% (the gate inverts out of window).

midtimeout, hour x dow gate:

| window | n kept | n removed | ungated NET | gated NET | delta NET | removed pnl | Sharpe ungated | Sharpe gated |
|---|---|---|---|---|---|---|---|---|
| W3 (fit) | 2,883 | 163 | 19,835.5 | 20,000.0 | +164.6 | -164.6 | 2.078 | 2.104 |
| W1 (OOS) | 7,364 | 314 | 31,379.7 | 30,826.7 | -553.0 | +553.0 | 1.037 | 1.050 |
| W2 (OOS) | 5,223 | 195 | 27,999.7 | 27,380.1 | -619.6 | +619.6 | 1.140 | 1.159 |

Survival of W3 improvement: W1 -336%, W2 -377%.

In all four OOS cells the trades the gate removed were net POSITIVE, so the frozen gate
strictly loses NET out of window. Daily Sharpe ticks up marginally (0.01-0.02) because
removing any trades trims daily variance, but that is not worth giving up 0.8-2.2% of NET,
and the Sharpe gain is within noise.

Cell instability confirms overfit: negative cells (n >= 30) fitted independently per window
barely overlap. base W3 vs W1 overlap: 0 of 3 cells; W3 vs W2: 1 of 3 (Sat 13h). midtimeout
W3 vs W1: 2 of 5; W3 vs W2: 2 of 5. Even W1 vs W2 (both large samples) agree on only 4-5
cells out of 21-28. Individual negative cells are noise, not structure.

## Real but non-actionable seasonality: the weekend is weaker

Day-of-week expectancy IS stable across all three windows and both variants: Saturday is
the weakest day everywhere, Sunday second weakest, and weekday expectancy is 2-4x weekend
expectancy (base pnl/trade: Sat +3.37/+0.08/+1.54 and Sun +3.44/+2.25/+2.92 across
W3/W1/W2, vs Mon-Thu roughly +3.5 to +13.3). But weekend expectancy stays positive in every
window, so a hard gate on weekends would discard profitable flow; that is exactly why the
fitted gates fail. If anything this supports a future sizing tilt (downweight Sat/Sun stake)
rather than a binary gate; that is a separate study with its own OOS protocol, not adopted here.

## Verdict

REJECT. No hour-of-day gate exists (all hours positive in the tune window). The hour x
day-of-week gate's W3 improvement (+$171 / +$165) not only fails to survive in W1/W2 but
reverses sign in all four out-of-window cells (-$188 to -$620), with near-zero cell overlap
between windows. Session seasonality gating does not improve the BTC-5m fade; leave
`process_markets` seasonal tables unused for this strategy.
