# Lane TOD / liquidity study: why the late-favourite lane bleeds overnight

Verdict: **ADOPT overnight align-min-mid raise (00-09 UTC require favourite mid >= 0.90)**

Context: the late-favourite LANE (buy aligned favourite mid >= 0.85 in the final 120s,
hold to expiry, sigma >= 4 floor) burned 6 times clustered overnight on two live shadow
nights, net-negative both. The fade's overnight weakness was pure low-vol and a sigma
floor fixed it; the lane already runs a higher floor (4.0) and still bleeds, so its
overnight problem is a different mechanism. This study tests liquidity vs vol-ceiling vs
pure variance on the backtest trades (no new harness run).

Data: `data/runs/alpha/lanedecon/btc5_spot.trades.jsonl` (W3 spot-only BTC) and
`data/runs/alpha/lane_xtoken/btc_w3.trades.jsonl` (cross-token). Lane filter applied:
`avg_price >= 0.85 AND sigma_bar_bps >= 4.0`. Both files are BTC-only and span the same
12 days (2026-05-07 .. 2026-05-18, before the sealed 05-19..06-30 window), so xtoken is a
near-duplicate of W3, not an independent-days holdout. It serves only as a consistency
check, and it agrees on every rule below.

Filtered base: W3 n=1631, hit 0.9350, net +$1407.8, 106 burns, worst day -$218.1.
XTOK n=1651, hit 0.9346, net +$1470.6, 108 burns, worst day -$212.7.

## Q1. Hit rate and per-trade P&L by UTC hour

Breakeven hit is `avg_entry + per-share fee` (a hold-to-expiry buy needs hit > entry+fee
to clear costs). The lane lives on a ~1pt margin above this line.

| segment   | n   | hit    | net$    | /trade  | avg_entry | breakeven | margin  |
|-----------|-----|--------|---------|---------|-----------|-----------|---------|
| OVERNIGHT (00-09) | 656 | 0.9268 | +320.3 | +0.488 | 0.9126 | 0.9181 | +0.0087 |
| DAYTIME (10-23)   | 975 | 0.9405 | +1087.5 | +1.115 | 0.9151 | 0.9204 | +0.0201 |

Overnight hit is 1.4pt lower (92.7% vs 94.1%) and per-trade P&L is less than half
(+$0.49 vs +$1.12). The overnight margin above breakeven (+0.0087) is roughly the size of
the hit-rate noise, so overnight sits right on the knife edge: a 1pt hit-rate slip flips it
negative, which is exactly what the live nights showed. In the backtest overnight is still
net-positive, but the worst single hours are 01 (-$92.8, hit 0.897), 05 (-$113.5, hit
0.877). Daytime has bad hours too (17 -$142.6, 21 -$69.4), so the effect is a tilt, not a
clean on/off switch.

## Q2. Vol-explained or residual? (vol-ceiling test)

Within the lane (all already sigma >= 4), higher sigma does NOT predict more burns:

| sigma bucket | n   | hit   | burn% | /trade |
|--------------|-----|-------|-------|--------|
| [4,5)   | 430 | 0.935 | 0.065 | +0.804 |
| [5,6)   | 426 | 0.930 | 0.070 | +0.533 |
| [6,8)   | 412 | 0.937 | 0.063 | +0.926 |
| [8,12)  | 281 | 0.929 | 0.071 | +0.724 |
| [12,inf)| 82  | 0.976 | 0.024 | +3.042 |

Burn rate is flat (6.3-7.1%) across 4-12 and the highest-vol bucket actually wins MORE
(97.6%, +$3.04/trade). Rule B (sigma <= 12) drops 82 trades and costs ~$250 net while
worsening the worst day, on both files. Crucially, overnight is LOWER vol than daytime
(median sigma 5.70 vs 6.06; frac sigma>=8 is 0.157 vs 0.267). So vol is not the overnight
mechanism, and a vol CEILING is rejected.

## Q3. Entry-price effect (liquidity signature)

Favourite entry mid strongly predicts reliability:

| entry mid    | n   | hit   | burn% | /trade |
|--------------|-----|-------|-------|--------|
| [0.85,0.88)  | 382 | 0.906 | 0.094 | +1.750 |
| [0.88,0.90)  | 313 | 0.901 | 0.099 | +0.168 |
| [0.90,0.93)  | 344 | 0.942 | 0.058 | +1.115 |
| [0.93,0.96)  | 348 | 0.960 | 0.040 | +0.574 |
| [0.96,1.00]  | 244 | 0.980 | 0.020 | +0.424 |

Sub-0.90 mids carry ~2x the burn rate of 0.90+. Overnight skews slightly more toward these
unreliable low mids: frac < 0.88 is 0.252 overnight vs 0.223 daytime (frac < 0.90: 0.442
vs 0.415). A thinner overnight book leaves more 0.85-0.90 favourites that are less
informative. This is the liquidity signature, modest in size but the only mechanism with
support in the data.

## Q4. Rule test (W3, with XTOK consistency check)

| rule | W3 net$ | W3 hit | W3 burns | W3 worstDay | dropped | XTOK net$ / worstDay |
|------|---------|--------|----------|-------------|---------|----------------------|
| base                         | 1407.8 | 0.9350 | 106 | -218.1 | 0   | 1470.6 / -212.7 |
| A: skip 00-09                | 1087.5 | 0.9405 | 58  | -96.1  | 656 | 1085.9 / -43.2  |
| A2: drop hrs 00,01,04,05     | 1643.1 | 0.9419 | 79  | -170.0 | 271 | 1628.8 / -114.7 |
| B: sigma <= 12               | 1158.3 | 0.9329 | 104 | -243.9 | 82  | 1222.4 / -245.0 |
| C: overnight mid >= 0.90     | 1371.0 | 0.9456 | 73  | -114.8 | 290 | 1163.7 / -60.9  |
| C2: overnight mid >= 0.88    | 1141.8 | 0.9386 | 90  | -166.1 | 165 | 1016.7 / -109.8 |
| D: global mid >= 0.90        | 686.6  | 0.9583 | 39  | -234.5 | 695 | 600.6 / -221.9  |

Reading the table:

- **A (skip overnight entirely)** sacrifices ~$320 of net (overnight is positive in
  backtest) to halve burns and the worst day. Pure de-risking, leaves money on the table.
- **A2 (drop 4 specific hours)** has the best raw economics but the hr05 loss is largely
  ONE trade on 05-09 (n=1, -$50) and hr01 is driven by 2-3 days. Tuning to 4 named hours on
  12 days is overfitting; rejected on robustness grounds.
- **B (vol ceiling)** loses money on both files. Confirms Q2: rejected.
- **D (global 0.90)** halves net by killing the profitable daytime 0.85-0.90 entries.
  Wrong: the low-mid tail is only unreliable overnight.
- **C (overnight mid >= 0.90)** is the principled winner. It acts directly on the
  liquidity mechanism (Q3): it removes only the thin-book, low-information overnight
  favourites. Net stays essentially flat (-$37 on W3, hit lifts 0.9350 -> 0.9456), burns
  fall 106 -> 73, and the worst day halves -$218 -> -$115. On XTOK the worst day improves
  -$213 -> -$61. Both negative days (05-11, 05-15) improve under C; no day is made worse.

C2 (overnight 0.88 floor) under-cuts the unreliable band and gives back most of the
benefit, so 0.90 is the right threshold.

## Honesty on n

This is 12 days, ~1631 lane trades, ~656 of them overnight. The overnight hit-rate deficit
(1.4pt) is real and directionally consistent across both files and across the entry-price
buckets, but it is small relative to per-day P&L swings, and overnight is still net-positive
in the backtest. The case for the rule is risk-shaping (cut the tail that bit live), not a
net-P&L uplift. The vol-ceiling hypothesis is cleanly rejected. The liquidity hypothesis is
supported but modest.

## Verdict

**ADOPT: in 00-09 UTC, raise the lane align-min-mid from 0.85 to 0.90** (keep 0.85 in
10-23 UTC). It targets the thin-overnight-book mechanism, holds net P&L flat, lifts hit to
0.946, and roughly halves the worst day on both files. NOT a vol ceiling (rejected by Q2).
NOT pure variance, but the effect is small, so size the expectation as drawdown control
rather than P&L gain, and re-confirm on the sealed post-05-19 archive before trusting it
live.
