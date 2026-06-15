# Whale study: 0x2855555a48ee7ec2e67272701651bfe77034ebe8 ("Dental-Latex")

Date: 2026-06-13. Window analyzed: 2026-03-28 14:20 UTC (first trade) to 2026-06-12 16:29 UTC.

## Headline

~$20k since March. Verified from cached /activity: realized cash-flow P&L +$17,680 plus $5,539 maker rebates = +$23,219 gross, on $5.48M of buy turnover and a peak open exposure of only ~$2,949. This is a high-turnover, razor-thin-margin pair-accumulator, not a directional trader.

## Data

- 809,110 activity rows cached at `data/external/whale_2855/activity.jsonl` (per-day raw also under `raw/`, 104 day files 2026-03-01 to 2026-06-12).
- Type/side breakdown: 794,349 TRADE/BUY ($5.48M), 124 TRADE/SELL ($3.9k), 14,564 REDEEM ($5.50M), 73 MAKER_REBATE ($5,539).
- Unlike wallet 8d1d, this account holds essentially everything to expiry and redeems (14,210 of 14,607 markets have a REDEEM, only 5 exit via SELL), so /activity REDEEM cash flows are nearly complete and cash-flow P&L is trustworthy here. The recorded REDEEM total ($5.50M) closely tracks buy turnover, consistent with a near-fully-paired book.
- Script: `scripts/whale_2855_analyze.py`.

## 1. Universe

Effectively a single market: btc-updown-5m is 99.0% of trade USD (777,780 fills, 13,887 markets). eth-updown-5m is the remaining 1.0% (16,693 fills, 718 markets). Zero non-crypto, zero longer horizons, zero alts beyond ETH. He trades the 5-minute Bitcoin up/down market and almost nothing else.

Monthly volume:

| month | trades | buy USD | active days | trades/day |
|---|---|---|---|---|
| 2026-03 (4d) | 53,370 | 580,306 | 4 | 13,342 |
| 2026-04 | 388,576 | 2,417,327 | 30 | 12,953 |
| 2026-05 | 243,102 | 1,466,330 | 30 | 8,103 |
| 2026-06 (12d) | 109,425 | 1,020,398 | 12 | 9,119 |

He trades flat around the clock: every UTC hour carries 3.6-4.8% of fills, no session tilt. ~480 markets is the entire daily btc-5m schedule (288 5m windows/day), so he participates in essentially every BTC 5-minute window, every day.

## 2. Execution style

He is a passive two-sided pair-builder. 97.2% of markets have buys on BOTH outcomes; 97.3% have a REDEEM (held to expiry); only 5 markets in the whole sample exited via SELL. The 124 SELL fills are rare profit-locks (median hold to sell 24s).

Maker/taker: at extreme prices the fee is below the detection band so most fills are fee-undetectable, but among detectable mid-price fills he is 70% maker, and he received $5,539 across 73 MAKER_REBATE events. He rests limit orders on both legs and gets filled; he is a quoter, not a crosser.

Entry is laddered and crammed into the final two minutes of the 5-minute window:

| window timing | fills | USD |
|---|---|---|
| 0-60s | 0.4% | 0.4% |
| 60-180s | 23.5% | 22.0% |
| 180-240s | 31.4% | 29.4% |
| 240-300s | 44.5% | 48.1% |

Median buy-fill offset is 231s into the 300s window; 76% of dollars land after the 180s mark. He builds each leg with a median of 22 buy fills per position (p90 59, max 168), in tiny clips (median BUY $1.2, median 8 shares). This is a market-maker accumulating depth as the window resolves, not a single-shot entry.

Price distribution confirms the pair structure. BUY fills cluster at the two extremes: 35% at 0.00-0.05 (the cheap leg) and 24.7% at 0.95-1.00 plus 18.9% at 0.85-0.95 (the expensive leg). USD-weighted, 66% of dollars buy the >=0.95 favourite and 28% the 0.85-0.95 leg, so capital is in the favourite while the cheap leg is bought in size of shares but small dollars. The 99c-sell/1c-buy recycle is absent (26 events in 14,607 markets).

## 3. Economics

The structure is: buy the favourite leg at a VWAP of ~0.951 and the underdog leg at ~0.035, so the combined cost per fully-paired share is a median of 0.988 (p10 0.979, p90 0.998). 92% of markets pair below 1.00 and 71% below 0.99. Every paired share is a guaranteed $1 at redemption, so a 0.988 pair cost locks ~1.2c of risk-free edge per paired share. That paired book is the engine: estimated locked (pair) P&L +$64,317.

But he does not pair perfectly. He carries a median 164 unpaired excess shares per market (dog/fav share ratio median 1.36, i.e. he overweights the cheap underdog leg). That excess is a directional longshot bet, and it loses: the excess leg wins only 7.9% of the time (1,119 of 14,171), for an estimated -$43,971. Net of the locked pair edge and the directional bleed, realized cash P&L is +$17,680, plus $5,539 rebates.

Outcome-class decomposition of both-sides markets (actual cash P&L):

| class | markets | share | total P&L | avg | median |
|---|---|---|---|---|---|
| favourite won | 13,109 | 92.4% | -47,998 | -3.7 | -1.5 |
| underdog won | 1,069 | 7.5% | +66,815 | +62.5 | +23.8 |
| unknown | 15 | 0.1% | -1,994 | -133.0 | -53.8 |

The shape is a positive-skew lottery: he loses a small amount on the 92% of markets where the favourite wins (the overweighted cheap leg expires worthless) and makes a large amount on the 7.5% where the underdog upsets. His whole P&L is the underdog tail paying off more than the pair-cost-plus-directional-bleed costs him. P&L per family per month (cash, excl rebates): btc-5m +$17,888 total (Mar +4,653, Apr +10,731, May +3,870, Jun -1,367); eth-5m -$207.

The edge is thinning. Median combined pair cost rose 0.9873 (Apr) -> 0.9884 (May) -> 0.9900 (Jun), and June is the first negative month (-$1,367, avg market P&L -0.3). As more makers crowd the cheap-leg quote, the locked spread compresses toward zero.

Fees and capital: an all-taker counterfactual would cost ~$36,862 in fees, which would erase the entire edge. He avoids that by being a maker and is paid $5,539 in rebates instead, so the maker rebate is not a rounding item: it is ~24% of net P&L and the difference between a thin profit and a loss. P&L per $ of buy turnover is 0.0032 (32bps), and per $ of peak capital ($2,949) is 5.99x. Daily: 49 of 76 days green, median +$102, best +$3,474, worst -$1,846.

## 4. Archetype

Pure pair-accumulator (calm-regime market-maker) with a deliberate underdog overweight. He quotes both legs of btc-updown-5m passively in the final two minutes, locks a ~1.2c risk-free pair spread, and overweights the cheap underdog leg to harvest the positive-skew upset tail. This is the catalog's pair accumulator, not tail convexity (his tail bet loses money on its own, -$44k) and not latency fade (he never flips). The economics are dominated by maker rebates and the locked spread; the directional overweight is a deliberately-sized lottery that the locked spread subsidises. It is structurally the calm-regime MM play our own notes flag as marginal, and the data agrees: 32bps per turnover dollar, edge compressing month over month, June already negative.

## 5. Transferability

Transferable to our $50-500 taker book:

| element | transferable? | note |
|---|---|---|
| locked pair spread (buy both legs <1.00) | No | Requires resting maker orders on both sides; as a taker the 0.07*p*(1-p) fee on each leg destroys the ~1.2c spread. This is a maker-only edge. |
| maker rebate harvesting | No | We are a taker book; rebates are ~24% of his net and not available to us. |
| underdog-overweight upset tail | Partially, high interest | The standalone directional leg (overweight the cheap underdog) carries the upside; isolated from the pair, it is a cheap-underdog longshot bet on btc-5m, which echoes 8d1d's cheap-tail edge. Worth testing as a directional sleeve. |
| late-window (final 2 min) entry | Yes | Concentrating entry in the last 40% of the 5m window is a clean, taker-survivable timing rule to test. |
| laddered many-clip accumulation | Yes | Same as 8d1d: many small fills beat one clip on vwap and impact. |
| hold-to-redemption | Already do | Matches hold@0.12. |

What this wallet does that hold@0.12 does NOT, worth testing: enter in the final two minutes of the window rather than on signal at arbitrary window-time. 2855 puts 76% of dollars in after the 180s mark of a 300s window, when the price has nearly converged and the residual uncertainty (and the underdog's mispricing) is sharpest. hold@0.12 has no explicit late-window timing gate. Testing a "enter only in the final 40% of the window" filter on the BTC short-horizon underdog leg is the most transferable idea here. The pair-spread and rebate machinery itself is maker-only and not portable to our taker book.
