# Whale study: 0x8d1d5d1c6041b13fc708b5d9f668070e1724ed4a ("CERTova" / "Piercing-Darkness")

Date: 2026-06-12. Window analyzed: 2026-04-02 09:39 UTC (first ever trade) to 2026-06-12 16:04 UTC.

## Headline verification

| Claim | Verified value | Source |
|---|---|---|
| ~$100k P&L since April | +$103,854 all-time | lb-api.polymarket.com profit, window=all |
| ~$8k today | +$6,485 rolling 24h | lb-api profit, window=1d |
| 30-day P&L | +$20,675 | lb-api profit, window=30d |
| Current portfolio value | $4,435 | data-api /value |

The account is real and profitable, but note the shape: roughly $83k of the $104k all-time profit was made in April and early May. The last 30 days contributed only ~$20.7k of the total, and 7d is $17.7k of that, i.e. mid-May was flat-to-negative and the engine restarted in June.

## Data

- 284,384 TRADE rows pulled from data-api /activity (cursor-paginated, deduped), cached at `data/external/whale_8d1d/activity.jsonl`.
- 15,764 REDEEM rows + 2 REWARD rows, cached at `data/external/whale_8d1d/nontrade.jsonl`.
- Authoritative per-market winners for all 32,959 conditionIds via clob.polymarket.com/markets/{cid} `tokens[].winner`, cached at `data/external/whale_8d1d/winners.jsonl`.
- Scripts: `scripts/whale_8d1d_pull.py`, `_pull_nontrade.py`, `_winners.py`, `_analyze.py`, `_patterns.py`, `_exec2.py`, `_final.py`.

Two data quirks that matter and that our own tooling should remember:

1. `usdcSize` in /activity EMBEDS the taker fee (BUY: usdcSize = price*size + 0.07*p*(1-p)*size; SELL: proceeds net of fee). Deviation of usdcSize from price*size identifies maker vs taker per fill.
2. REDEEM rows have `outcomeIndex=999` (useless for winner inference) and are INCOMPLETE: recorded redeems total $3.23M while imputed winning-share payouts are materially higher. Cash-flow P&L from /activity alone shows -$157k for an account the leaderboard scores at +$104k. Auto-redemptions are only partially indexed. Never compute a wallet's P&L from /activity cash flows alone.

## 1. Universe

100% crypto up/down markets. Zero sports, zero politics, zero anything else. Four assets, three horizons.

Buy volume (USD, fee-inclusive) by series and month:

| series | 2026-04 | 2026-05 | 2026-06 (12d) | markets traded |
|---|---|---|---|---|
| btc-5m | 797,144 | 743,389 | 327,616 | 7,779 |
| btc-15m | 463,494 | 354,674 | 275,988 | 5,114 |
| btc-1h | 369,540 | 256,429 | 171,333 | 1,478 |
| eth-15m | 208,884 | 121,369 | 48,112 | 3,788 |
| eth-5m | 173,089 | 133,435 | 45,085 | 3,399 |
| eth-1h | 165,056 | 107,694 | 37,019 | 1,475 |
| xrp-5m | 121,871 | 46,060 | 0 | 2,066 |
| xrp-1h | 102,316 | 48,683 | 0 | 944 |
| xrp-15m | 106,175 | 28,138 | 0 | 2,319 |
| sol-5m | 100,820 | 28,719 | 0 | 1,576 |
| sol-1h | 85,827 | 40,131 | 0 | 956 |
| sol-15m | 66,470 | 27,679 | 0 | 2,065 |
| TOTAL | 2,760,685 | 1,936,400 | 905,152 | 32,959 |

Evolution April to June: he started spray-everything (4 assets x 3 horizons, ~4,900 fills/day), cut XRP and SOL entirely by June, halved fill count in May (3,713/day) and cut it to ~1,170/day in June while RAISING per-position size (median position cost $128 Apr, $39 May, $154 Jun; median position 400 shares Apr, 600 shares Jun). June is BTC-dominated (86% of volume) with ETH small. He concentrated into what worked.

He trades around the clock (24h activity, modest US-hours tilt), median ~483 distinct markets/day.

## 2. Execution style

He is a maker who is turning into a taker. Using the fee-embed test on `usdcSize` (BUY usdcSize = price*size + 0.07*p*(1-p)*size for a taker; a maker pays no fee so usdcSize = price*size), the maker share of fee-detectable fills collapses month over month:

| month | total USD | maker USD | taker USD | maker share of detectable |
|---|---|---|---|---|
| 2026-04 | 4,226,462 | 2,085,127 | 62,773 | 97% |
| 2026-05 | 2,491,807 | 664,465 | 680,471 | 49% |
| 2026-06 | 1,039,336 | 168,001 | 349,935 | 32% |

In April he was almost purely a passive quoter, resting limit orders and getting hit. By June a third of detectable flow is taker (crossing the spread). Across the whole sample, maker share is 67-83% in every mid-price bucket, so he quotes on both sides of the book; the move to taker is a regime/aggression shift, not a price-bucket artifact. (Fills at >=0.95 are mostly fee-undetectable because the fee falls under the rounding band, so the 0.9-1.0 row understates taker volume.)

Entry-price distribution is favourite-heavy and front-loaded in the window. BUY USD by price x timing (timing = fraction of the market's lifetime elapsed at fill):

| segment | USD | share |
|---|---|---|
| favourite (>=0.85), early (<70% of window) | 2,419,023 | 43.2% |
| other | 1,316,478 | 23.5% |
| mid (0.30-0.70), early | 952,194 | 17.0% |
| favourite, late (>=70%) | 608,974 | 10.9% |
| mid, late | 124,167 | 2.2% |
| tail (<=0.10), late | 113,934 | 2.0% |
| tail, early | 67,468 | 1.2% |

54% of buy dollars go into the favourite (>=0.85), most of it early. He is buying conviction, not late-window certainty: only 11% of buy dollars are late favourite. There is a real cheap-tail program too (see economics): he sprays many small <=0.10 tickets.

Laddered accumulation is the norm. Per position (a cid+outcome leg with >=10 shares), he averages a median of 4 buy fills, 60% of positions get 3+ fills, p90 is 15 fills and the max is 166. He builds positions incrementally rather than in one clip.

Position sizing grew as he concentrated. Median position cost was $128 in April, $39 in May, $154 in June; median shares per position 400 -> 400 -> 600. Fill count fell from ~4,900/day (April) to ~1,170/day (June) while per-position size rose: fewer, bigger, more concentrated bets.

Exit style is hold-to-redemption with a late-window profit-lock overlay. Of all shares bought (positions >=10sh), 79% are held to expiry and redeemed; only 21% are sold before expiry. 24,345 positions are fully held (>=95% of shares) versus 6,564 fully sold. When he does sell, 81% of sell proceeds land in the late (70-95% of window) segment and 57% of all sell volume is at price >=0.97, i.e. selling near-certain winners back at ~99c to free capital before settlement rather than waiting for the redeem. The 99c-sell / 1c-buy recycle (sell a winner at 0.99, rebuy the loser at 0.01) is essentially absent: only 29 positions show both legs. He locks wins; he does not run a recycling loop.

## 3. Economics

P&L computed by joining each cid+outcome position to authoritative winners (`winners.jsonl`, 32,902 of 32,959 markets resolved) and imputing the $1 payout on held winning shares, rather than trusting /activity REDEEM cash flows (which under-index auto-redeems: recorded REDEEM is $3.23M against $3.52M imputed). Imputed total P&L is +$78,821 against a leaderboard all-time of +$103,854; the gap is the 55 expired markets with held shares but unknown winner plus residual redeem under-indexing, so treat +$79k as a conservative floor.

P&L by series (imputed, USD):

| series | P&L |
|---|---|
| btc-5m | +35,815 |
| btc-1h | +25,592 |
| btc-15m | +20,311 |
| eth-5m | +6,514 |
| xrp-1h | +3,667 |
| sol-1h | +1,471 |
| eth-1h | +1,333 |
| sol-15m | +360 |
| xrp-15m | +323 |
| eth-15m | -1,860 |
| sol-5m | -3,290 |
| xrp-5m | -11,415 |

BTC is 82k of the 79k net (the alts roughly cancel; xrp-5m and sol-5m were the worst, which is why he cut them). By month: April +$70,250, May -$6,321, June (12d) +$14,892. 46 of 70 days green, best +$11,681 (Apr 13), worst -$11,481 (May 2). The account is front-loaded to April and restarted in June.

Hit rate by BUY-price bucket against true winners, with break-even (be% = entry price) and per-dollar edge:

| bucket | shares | USD | win% | be% | edge/$1 |
|---|---|---|---|---|---|
| 0.0-0.1 | 7,985,410 | 172,942 | 2.4 | 2.2 | +0.126 |
| 0.1-0.2 | 594,793 | 84,221 | 14.5 | 14.2 | +0.023 |
| 0.2-0.3 | 394,503 | 98,736 | 23.0 | 25.0 | -0.080 |
| 0.3-0.4 | 323,958 | 113,529 | 34.9 | 35.0 | -0.003 |
| 0.4-0.5 | 412,900 | 186,839 | 47.5 | 45.3 | +0.049 |
| 0.5-0.6 | 537,025 | 295,477 | 56.3 | 55.0 | +0.024 |
| 0.6-0.7 | 670,011 | 436,818 | 67.4 | 65.2 | +0.033 |
| 0.7-0.8 | 916,164 | 690,628 | 75.5 | 75.4 | +0.002 |
| 0.8-0.9 | 1,275,979 | 1,086,829 | 86.9 | 85.2 | +0.020 |
| 0.9-1.0 | 2,539,134 | 2,428,072 | 97.2 | 95.6 | +0.017 |

Two engines drive the edge. The cheap-tail program (<=0.10 buys) is the standout: at 0.01-0.05 he is reliably above break-even (e.g. px=0.04 wins 6.1% on a 4% breakeven), and the whole 0.0-0.1 bucket runs +0.126 edge/$1 on $173k staked. Cheap-tail P&L is +$22.7k held-to-expiry, concentrated in btc-5m (+$14.2k). The favourite program (>=0.85) is the bulk of capital and grinds a steady +0.017 to +0.020 edge/$1. The 0.2-0.4 zone is where he bleeds (-0.080 at 0.2-0.3), i.e. low-conviction mid-priced longshots are his weak spot. Held-to-expiry P&L attribution: fav>=0.85 +$56.8k, tail<=0.10 +$22.7k, mid 0.50-0.85 +$28.7k, 0.10-0.50 +$1.9k.

Fee load: he is mostly maker (no fee) early, so realized fees are far below the all-taker counterfactual; the late shift to taker is adding fee drag exactly as participation/edge thins. Capital: minimum equity hit -$7,059 (own cash required) on Apr 5; peak equity $79,157 on May 25; max drawdown $32,233. The drawdown is large relative to standing capital, consistent with the April front-load then May give-back.

## 4. Archetype

Primary archetype: tail-convexity + favourite-grinder hybrid, executed as a two-sided maker. He is closest to the catalog's tail convexity (cheap <=0.10 longshots with positive expected value, +0.126 edge/$1) stacked on a late-favourite/conviction-favourite grinder (>=0.85 buys, the capital bulk, +0.02 edge/$1). He is not a latency-fade scalper (he holds 79% to expiry, does not flip) and not a pure pair-accumulator (only 7.4% of markets get >=$5 buys on both sides). The defining behaviour is breadth plus passive quoting: ~480 distinct markets/day, median 4 laddered fills per leg, resting on both sides of the book, monetising the cheap tail and the rich favourite simultaneously. The June evolution (cut alts, concentrate BTC, raise size, turn taker) is a maker losing queue edge and compensating with aggression.

## 5. Transferability

Transferable to our $50-500 taker book:

| element | transferable? | note |
|---|---|---|
| cheap-tail <=0.10 program | Yes, high value | +0.126 edge/$1 on btc-5m is a real, taker-survivable edge; small clips fit our size; this is the single most copyable piece |
| favourite >=0.85 grind | Partially | +0.017-0.020 edge/$1 is thin and was earned mostly as a maker (no fee). As a taker the 0.07*p*(1-p) fee at p=0.9 is ~63bps, which eats most of a 2% edge. Needs the passive-exit / rest-at-mid treatment to survive. |
| two-sided quoting / maker entry | No (for now) | The April 97%-maker engine is the calm-regime MM play we already judged structurally marginal; our book is taker. |
| laddered accumulation (4 fills/leg) | Yes | Clip-laddering into a position reduces impact and improves vwap; cheap to test in our executor. |
| hold-to-redemption | Already do | Matches hold@0.12. |
| 99c profit-lock sell | Maybe | Selling near-certain winners at >=0.97 late in the window frees capital ~minutes before redeem; only worth it if capital-constrained, which at $50-500 we are not. |

What this wallet does that hold@0.12 does NOT, worth testing: an explicit cheap-tail (<=0.10) long leg. hold@0.12 buys the underdog around 0.12 and holds; 8d1d's edge is concentrated one bucket lower, at 0.01-0.05, where the per-dollar edge is 5x larger (+0.126 vs the +0.023 of the 0.1-0.2 bucket). Testing a deeper-tail entry (a small <=0.05 sleeve alongside the 0.12 leg) on btc-5m is the highest-value experiment this wallet suggests. Second, his laddered multi-fill accumulation versus our single-clip entry is a cheap execution tweak to test.
