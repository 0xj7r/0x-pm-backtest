# ToD drawdown decomposition + structural sigma-floor overlay (BTC-5m fade)

Verdict: **ADOPT a sigma_bar_bps >= 3.0 floor** (structural, not calendar). It is a near-free
drawdown trim: it removes the genuinely toxic micro-vol slice (overnight and daytime alike)
at zero or slightly positive NET, and the bad-hours P&L is mostly a low-sigma proxy that the
floor captures without any time-of-day rule. A larger floor (4-5 bps) cuts more drawdown but
gives up more NET than it saves, so it is rejected. Time-of-day has no residual negative
signal beyond vol in this config.

This is the legitimate complement to F2 (`f2_seasonality.md`), which REJECTED P&L-cell
calendar gating as overfit. Here the gate is a continuous physical quantity (the live vol/
activity proxy), fit as a single threshold, frozen out of window.

## Setup

- Read-only over existing trades; pm-app never run. Script: `scripts/tod_drawdown.py`.
- Config: feemin **base** (the passive-exit fade). W3 (tune) `base.trades.jsonl` (May 7-18,
  n=3,046), W1 (OOS) `W1_base.trades.jsonl` (Feb 12 - Mar 31, n=7,678), W2 (OOS)
  `W2_base.trades.jsonl` (April, n=5,418). All `window_secs == 300`. No trade in the sealed
  live window 2026-05-19..06-30 (asserted at load).
- `pnl` is fee-net. Daily Sharpe = mean(daily pnl)/std(daily pnl), not annualized.
  Max drawdown (MDD) = max peak-to-trough of the cumulative fee-net pnl curve, trades ordered
  by (date, hour), in dollars.
- Drawdown share per cell = that cell's losing-trade pnl mass as a fraction of total
  losing-trade pnl mass (drawdown is path-dependent and not additively decomposable; this is
  the well-defined "where do the losses live" proxy).
- testlook (`testlook0612/hold012_test.trades.jsonl`, sealed live window, hold012 config) is
  used for the DESCRIPTIVE ToD picture only, never for fitting.

## 1. Descriptive: where is P&L (and loss mass) by hour and day

### 1a. Hour-of-day (UTC), pooled W1+W2+W3

| hour | n | NET | pnl/trd | dd share | W1/W2/W3 pnl/trd | hour | n | NET | pnl/trd | dd share | W1/W2/W3 pnl/trd |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 00 | 841 | 2836 | 3.37 | 5.0% | 2.2/3.8/5.2 | 12 | 657 | 5090 | 7.75 | 3.8% | 7.2/7.4/9.4 |
| 01 | 717 | 1988 | 2.77 | 4.5% | 2.5/2.4/4.2 | 13 | 798 | 3027 | 3.79 | 5.1% | 3.0/4.8/4.0 |
| 02 | 664 | 1734 | 2.61 | 4.8% | 1.5/2.9/5.0 | 14 | 566 | 4739 | 8.37 | 3.4% | 7.0/10.1/9.5 |
| 03 | 651 | 2169 | 3.33 | 3.9% | 2.4/3.8/5.5 | 15 | 558 | 3949 | 7.08 | 3.6% | 6.4/4.9/12.1 |
| 04 | 735 | 1314 | 1.79 | 4.9% | 2.4/1.6/0.6 | 16 | 530 | 4851 | 9.15 | 3.0% | 6.7/12.7/9.3 |
| 05 | 764 | 2726 | 3.57 | 4.7% | 2.3/5.7/2.6 | 17 | 608 | 4846 | 7.97 | 2.7% | 5.1/10.1/11.7 |
| 06 | 772 | 287 | **0.37** | 5.4% | 0.4/0.2/0.6 | 18 | 588 | 3028 | 5.15 | 3.9% | 2.0/6.9/9.0 |
| 07 | 807 | 2255 | 2.79 | 4.9% | 1.8/2.6/6.3 | 19 | 618 | 3132 | 5.07 | 4.0% | 2.6/6.0/9.5 |
| 08 | 649 | 3278 | 5.05 | 4.0% | 3.2/6.2/8.5 | 20 | 599 | 3511 | 5.86 | 3.3% | 4.4/6.8/7.9 |
| 09 | 579 | 2249 | 3.88 | 4.0% | 2.5/5.0/5.4 | 21 | 691 | 2711 | 3.92 | 4.3% | 3.4/3.7/6.0 |
| 10 | 630 | 4266 | 6.77 | 3.7% | 2.9/12.9/5.8 | 22 | 778 | 3889 | 5.00 | 4.6% | 3.3/4.7/11.1 |
| 11 | 631 | 3174 | 5.03 | 3.9% | 2.4/7.7/7.5 | 23 | 711 | 3823 | 5.38 | 4.7% | 1.2/7.7/11.7 |

**Structurally-negative hours (negative in ALL of W1, W2, W3): NONE.** Every hour is net
positive in every window. The weakest hour everywhere is **UTC 06 (+0.37/trade pooled,
+0.4/+0.2/+0.6 per window)**, then 04 (+1.79). These are the overnight/pre-Europe dead hours:
weak-positive, not negative, in this base config. This matches F2's finding that no hour-only
gate exists, and refines it: the dead hours bleed margin via fees but do not lose outright.

### 1b. Overnight dead-hours block (UTC 02-09) vs rest, pooled

| block | n | NET | pnl/trd | fees | dd share | win% | mean sigma |
|---|---|---|---|---|---|---|---|
| overnight 02-09 | 5,621 | 16,013 | **2.85** | 17,085 | 36.7% | 55.2% | 7.98 |
| daytime | 10,521 | 58,860 | **5.59** | 32,534 | 63.3% | 59.2% | 9.72 |

Overnight expectancy is ~half of daytime (2.85 vs 5.59 per trade) and carries lower mean sigma
(7.98 vs 9.72 bps) and a lower win rate. Fees are a heavier drag overnight relative to gross.
This is the quantitative "dead-hours effect": thinner edge, vol-driven, not a sign flip.

### 1c. Day-of-week, pooled W1+W2+W3

| dow | n | NET | pnl/trd | dd share | W1/W2/W3 pnl/trd |
|---|---|---|---|---|---|
| Mon | 1,909 | 11,998 | 6.28 | 11.1% | 3.8/8.6/8.4 |
| Tue | 1,879 | 12,921 | 6.88 | 9.0% | 4.9/8.7/10.3 |
| Wed | 1,995 | 10,728 | 5.38 | 11.7% | 3.5/6.0/10.5 |
| Thu | 2,203 | 17,118 | 7.77 | 10.6% | 6.2/7.0/13.3 |
| Fri | 2,231 | 10,681 | 4.79 | 14.5% | 3.2/6.0/6.1 |
| Sat | 3,094 | 3,765 | **1.22** | **22.5%** | 0.1/1.5/3.4 |
| Sun | 2,831 | 7,662 | **2.71** | **20.6%** | 2.3/2.9/3.4 |

The weekend is the stable seasonal pattern (Sat weakest, Sun second, in all three windows) and
carries 43% of total loss mass on ~37% of trades. But weekend expectancy stays positive in
every window, so a hard weekend gate would discard profitable flow. Same conclusion as F2:
this argues for a sizing tilt, not a binary calendar gate. Not actioned here.

### 1d. testlook (sealed live window, hold012) hour-of-day, descriptive only

The sealed window shows real per-hour negatives (01h -0.86, 04h -1.05, 08h -1.33, 17h -2.36)
but they do NOT line up with the fit-window weak hours (e.g. 17h is among the strongest in
W1/W2/W3). The low-sigma hours (04-09, mean sigma 4.5-5.9) cluster near or below break-even
live, consistent with the dead-hours-are-low-vol story; the negative hours are noise on a
single window. This confirms calendar cells do not repeat (the F2 lesson) and that the live
signal is vol, not clock.

## 2. Structural overlay: sigma_bar_bps floor (fit W3, freeze W1/W2)

Gate: keep trades with `sigma_bar_bps >= floor`. The last column is the structural test asked
for: dollars of MDD removed per dollar of NET given up (want >> 1 to justify the trim).

**W3 (fit)** base NET=20,974, Sharpe=1.905, MDD=607:

| floor | n_kept | n_rm | rm_pnl | NET | dNET | Sharpe | MDD | dMDD | dd_saved/$NET_up |
|---|---|---|---|---|---|---|---|---|---|
| 0 | 3046 | 0 | 0 | 20974 | 0 | 1.905 | 607 | 0 | - |
| **3** | 2803 | 243 | -165 | 21138 | **+165** | 1.931 | 560 | -47 | NET *gained* |
| 4 | 2239 | 807 | +814 | 20160 | -814 | 1.834 | 249 | -358 | 0.44 |
| 5 | 1618 | 1428 | +5164 | 15810 | -5164 | 1.456 | 300 | -307 | 0.06 |

**W1 (frozen OOS)** base NET=24,124, Sharpe=0.849, MDD=2,232:

| floor | n_kept | n_rm | rm_pnl | NET | dNET | Sharpe | MDD | dMDD |
|---|---|---|---|---|---|---|---|---|
| **3** | 7645 | 33 | -129 | 24253 | **+129** | 0.859 | 2232 | 0 |
| 4 | 7488 | 190 | +163 | 23960 | -163 | 0.845 | 2232 | 0 |
| 5 | 7159 | 519 | +69 | 24054 | -69 | 0.851 | 2208 | -24 |

**W2 (frozen OOS)** base NET=29,775, Sharpe=1.149, MDD=1,528:

| floor | n_kept | n_rm | rm_pnl | NET | dNET | Sharpe | MDD | dMDD |
|---|---|---|---|---|---|---|---|---|
| **3** | 5164 | 254 | -64 | 29839 | **+64** | 1.154 | 1512 | -15 |
| 4 | 4575 | 843 | -830 | 30605 | +830 | 1.258 | 669 | -859 |
| 5 | 4069 | 1349 | -378 | 30153 | +378 | 1.259 | 548 | -980 |

Reading: at **floor 3 bps the NET delta is POSITIVE in all three windows** (+165/+129/+64),
because the removed slice is net-negative (rm_pnl -165/-129/-64). It removes 243/33/254 trades
(pooled ~530 of 16,142 = **3.3% of entries**) and trims W3 MDD by ~8% with zero MDD cost in W1.
This is a free trim: the gate pays for itself in NET while shaving the worst micro-vol trades.

At **floor 4-5 bps** the picture splits by window: W2 keeps improving (MDD 1,528 -> 669 at 4bps)
but W3 inverts (NET -814, dd_saved/$given_up = 0.44, i.e. you sacrifice $2.27 of NET for every
$1 of drawdown removed). Because the fit window (W3) says 4+ bps gives up more NET than it
saves, the frozen choice is **3 bps**, not 4-5. A higher floor would be fitting to W2's
favorable draw, exactly the trap F2 warns against.

## 3. Cross-check: is "bad hours" just low sigma?

Mean sigma tracks the hour-of-day P&L closely: the weak hours (04-09, pnl/trd 0.4-3.9) carry
mean sigma 7.4-8.0 and the highest share of sub-3-bps trades (06h: 5.8% under 3 bps), while the
strong afternoon hours (14-17, pnl/trd 7-9) carry mean sigma 10.9-14.5 with almost no sub-3-bps
trades. So time-of-day is largely a vol proxy: the floor captures the weak hours without any
clock rule.

But the loss is **not purely overnight** (pooled, by sigma slice):

| | overnight 02-09 | daytime |
|---|---|---|
| sigma < 3 bps | n=264, NET **-217**, -0.82/trd | n=266, NET **-141**, -0.53/trd |
| sigma >= 3 bps | n=5,357, NET +16,230, +3.03/trd | (profitable) |

The toxic slice (sigma < 3 bps, net-negative) exists in the daytime too (-$141 on 266 trades),
so a calendar overnight gate would both miss daytime micro-vol losers and discard profitable
overnight trades. The sigma floor targets the actual driver. At 4 bps the daytime sub-floor
slice turns slightly positive (+0.23/trd), which is why pushing the floor past 3 starts
discarding good flow: the genuinely-bad mass is concentrated below ~3 bps.

## Verdict

**ADOPT sigma_bar_bps >= 3.0 as a structural entry floor.** Out of window (W1, W2 frozen) it is
NET-positive (+$129, +$64), removes ~3.3% of entries (the net-negative micro-vol slice), and
trims drawdown at zero NET cost. It is a physical, single-threshold gate on the live vol proxy,
not a calendar table, so it does not inherit F2's overfit failure mode (the removed slice is
net-negative in all three windows, the opposite of F2's calendar cells which were net-positive
OOS). Do NOT raise the floor to 4-5 bps: the fit window says that gives up more NET than the
drawdown it buys (dd_saved/$NET_given_up = 0.44 at 4 bps in W3). **Time-of-day has no residual
negative signal beyond vol** here: the weak hours are the low-sigma hours, and the genuine
losses live below ~3 bps in both overnight and daytime, which the floor removes directly.
