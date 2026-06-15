# Wallet 0xce25e214d5cfe4f459cf67f08df581885aae7fdc ("Agile-Spacing")

Analysis date: 2026-06-12. Full activity pull May 1 to Jun 12 2026 from data-api.polymarket.com/activity, 752,864 rows cached to `data/external/whale_ce25/activity_raw.jsonl` (complete history, no sampling needed; pagination by timestamp cursor with offset drill-down; daily P&L curve in `pnl_daily.json`). Scripts: `scripts/fetch_whale_ce25.py`, `scripts/analyze_whale_ce25.py`, `scripts/ce25_buckets.py`, `scripts/ce25_offset_edge.py`.

## Headline: the $300k is pre-fee accounting, real net is ~$55k

The profile P&L curve ($298k cumulative) reconciles exactly with gross (pre-fee) trade prices. The activity feed embeds the taker fee in `usdcSize` (verified: usdcSize = price\*size + 0.07\*p\*(1-p)\*size to the cent on taker fills), while Polymarket's position basis and profile P&L exclude fees (verified per-market against closed-positions API: their avgPrice 0.3889 vs cash avgPrice 0.4022 on the same fills).

| Item | USD |
|---|---|
| Gross buy notional (pre-fee) | $9,769,095 |
| Cash spent (incl taker fees) | $10,013,967 |
| Taker fees paid | $244,871 |
| Redeem payouts | $10,063,876 |
| Maker rebates received | $5,046 |
| Gross P&L (matches profile ~$298k) | $294,781 |
| Net cash P&L | $54,955 |

Fees consumed 83% of gross edge. Still an extraordinary return on capital: peak external capital ever needed was $12,501 (2026-05-05), so net $55k in 6 weeks is roughly 4.4x on working capital, with 801x capital recycle (buy notional / peak capital).

## 1. Universe

8 series only, all crypto up/down, 5m and 15m horizons. No hourly, no 4h, nothing else. 32,639 markets traded over 41 active days (idle May 2-3 only).

| Series | Notional | Share | Fills |
|---|---|---|---|
| btc-updown-15m | $3,241,776 | 32.4% | 164,450 |
| btc-updown-5m | $3,014,571 | 30.1% | 165,951 |
| eth-updown-15m | $1,302,247 | 13.0% | 108,662 |
| eth-updown-5m | $1,177,044 | 11.8% | 111,548 |
| sol-updown-15m | $455,071 | 4.5% | 53,417 |
| sol-updown-5m | $365,371 | 3.6% | 49,920 |
| xrp-updown-15m | $257,819 | 2.6% | 34,307 |
| xrp-updown-5m | $200,067 | 2.0% | 32,929 |

Weekly volume (USD, buys incl fees):

| Week | btc-15m | btc-5m | eth-15m | eth-5m | sol+xrp | Total |
|---|---|---|---|---|---|---|
| W18 (May 1) | 64,474 | 42,986 | 24,044 | 16,964 | 25,386 | ~174k |
| W19 | 785,581 | 520,971 | 272,418 | 201,427 | 235,548 | ~2.02M |
| W20 | 696,131 | 427,119 | 273,207 | 183,821 | 212,845 | ~1.79M |
| W21 | 615,086 | 526,610 | 253,857 | 217,550 | 242,679 | ~1.86M |
| W22 | 381,859 | 469,377 | 152,295 | 146,875 | 165,197 | ~1.32M |
| W23 | 395,901 | 478,649 | 178,869 | 200,585 | 193,754 | ~1.45M |
| W24 | 302,744 | 548,859 | 147,558 | 209,821 | 202,917 | ~1.41M |

Evolution: ramp complete by week 2; volume peaked W19 ($2.0M) then settled ~$1.4M/wk. Clear migration from 15m toward 5m over time (BTC W19: 15m 60% of BTC volume; W24: 5m 64%). Asset mix stable: BTC ~62%, ETH ~25%, SOL ~8%, XRP ~5%.

## 2. Execution style

| Metric | Value |
|---|---|
| Sides | 721,184 BUYs, zero SELLs, 31,636 REDEEMs, 41 MAKER_REBATE credits, 3 MERGEs |
| Fills/day | mean 17,590, max 31,653 (about 12 fills/min sustained, 24/7) |
| Fill size $ | p10 0.95, p25 2.94, p50 7.73, p75 16.34, p90 31.64, p99 102.61, max 520 |
| Buy price | p10 0.10, p25 0.27, p50 0.48, p75 0.65, p90 0.78 |
| Fills per market | p50 18, p90 50, max 156 |
| Both-sides markets | 29,704 of 32,639 (91%) |
| Hold to expiry | 100% (no early exits, ever; one redeem per market) |
| Taker share | 571k fills (87% of notional) pay full 0.07\*p\*(1-p); 146k fills (10% of notional) fee-free (maker) |

Entry timing (seconds after window open): 5m markets p10 28s, p50 139s, p90 249s, p99 278s; 15m markets p50 458s, p99 866s. They trade continuously through the entire life of every window across all 8 series at once, stopping ~20-30s before resolution. ~10% of fills have 5-6 decimal average prices, i.e. taker sweeps through multiple book levels.

No 99c-sell-plus-1c recycling (no sells at all). No laddered exits. Pure accumulate-and-redeem. The "pair" is implicit: median combined avg cost of Up+Down in both-sided markets is 0.991 (p25 0.878, p75 1.090), with median share imbalance 20% between sides.

Not directional: the heavier side wins only 45.5% of the time (n=25,769 markets with >5% imbalance). All P&L comes from per-fill price edge, not from picking winners.

## 3. Economics

Weekly, gross vs net (returns on buy notional):

| Week | Gross P&L | Fees | Rebates | Net P&L | Gross ret | Net ret |
|---|---|---|---|---|---|---|
| W18 | 1,204 | 3,477 | 303 | -1,970 | 0.71% | -1.16% |
| W19 | 67,350 | 50,960 | 522 | 16,911 | 3.43% | 0.86% |
| W20 | 56,655 | 47,347 | 325 | 9,633 | 3.25% | 0.55% |
| W21 | 44,823 | 45,393 | 703 | 133 | 2.48% | 0.01% |
| W22 | 41,990 | 26,002 | 2,057 | 18,046 | 3.26% | 1.40% |
| W23 | 48,851 | 38,677 | 317 | 10,492 | 3.47% | 0.74% |
| W24 (partial) | 33,907 | 33,015 | 818 | 1,710 | 2.46% | 0.12% |

Gross edge is remarkably stable at 2.5-3.5% of notional every week. Net is fee-hostage: W21 was a wash, W22 best (higher maker share 27% plus lower fee mix). Net cash P&L by series: btc-5m +$34.2k, btc-15m +$11.0k, sol-15m +$5.1k, sol-5m +$1.8k, xrp-5m +$1.8k, eth-15m +$0.6k, eth-5m -$3.1k, xrp-15m -$1.4k (plus $5.0k rebates unallocated). BTC carries 82% of net P&L; ETH is roughly breakeven after fees.

Hit rate and edge by entry price bucket (31,104 redeem-resolved markets; hold-to-expiry P&L; net includes actual fees paid):

| Bucket | n | Hit% | Implied% | Gross P&L | Net P&L | Gross/$ | Net/$ |
|---|---|---|---|---|---|---|---|
| 0.0 (<0.05) | 26,534 | 2.7 | ~2 | 7,524 | 6,381 | +32.4% | +27.5% |
| 0.1 | 77,421 | 10.4 | 10 | 11,449 | 1,074 | +5.7% | +0.5% |
| 0.2 | 63,764 | 22.4 | 20 | 26,235 | 9,403 | +7.6% | +2.7% |
| 0.3 | 72,963 | 32.9 | 30 | 40,419 | 14,474 | +6.8% | +2.4% |
| 0.4 | 82,866 | 42.6 | 40 | 52,910 | 17,520 | +5.7% | +1.9% |
| 0.5 | 112,010 | 51.7 | 50 | 77,565 | 22,535 | +4.6% | +1.3% |
| 0.6 | 102,107 | 61.0 | 60 | 40,430 | -5,002 | +2.3% | -0.3% |
| 0.7 | 82,307 | 70.2 | 70 | 17,908 | -13,937 | +1.1% | -0.9% |
| 0.8 | 59,263 | 80.4 | 80 | 14,424 | -2,738 | +1.1% | -0.2% |
| 0.9 | 28,724 | 91.7 | 90 | 16,612 | 11,985 | +1.9% | +1.4% |
| 1.0 (>0.95) | 8,799 | 98.5 | ~98 | 4,035 | 3,501 | +1.0% | +0.8% |

Every bucket beats its implied probability gross (they systematically buy below fair), but the 0.07\*p\*(1-p) fee flips 0.6-0.8 negative. Net edge lives in three places: deep longshots under 0.05 (+27.5%/$, small capacity), the underdog-to-coinflip band 0.2-0.55 (+1.3 to 2.7%/$, where 60% of their stake sits), and the late favourite 0.88+ (+0.8 to 1.4%/$).

BTC by window quintile (net/$, 5m series): mid-band 0.10-0.55 is positive in every quintile, strongest in q1 (+6.6%) and q3 (+5.5%); the 0.55-0.88 band is negative everywhere except the open and the final quintile; favourite >=0.88 is positive in all quintiles (+0.8 to 2.5%) with size concentrated late. Maker fills net +0.56%/$ vs taker +0.66%/$, so the maker leg is not where the edge is; it just reduces fee drag.

Capital: cum cashflow never went below -$12.5k. Average instantaneous inventory is only ~$1-3k (current open value $1.6k). Daily buys ~$240k against ~$13k working capital is ~18x intraday recycle.

## 4. Archetype vs catalog

| Archetype | Fit |
|---|---|
| Latency fade | Strong. Both-sides taker buying of momentarily-cheap sides, 87% taker, multi-level sweeps, edge in every gross bucket |
| Pair accumulator | Strong (as execution shell). 91% both-sides, combined cost p50 0.991, hold to expiry, never sells, redeems $1 per pair |
| Late-favourite hammer | Minor sleeve. 0.9-1.0 buckets only ~13% of stake but cleanly net-positive |
| Momentum rider | No. Heavy side wins <50%; no early exits |
| Tail convexity | Present at the <0.05 bucket (+27.5%/$ net) but tiny stake ($23k of $9.8M) |
| Maker/quoting | No. Only 10% of notional fee-free; rebates $5k total |

Verdict: a high-velocity latency-fade engine wearing a pair-accumulator shell. It snipes whichever side lags fair value after spot moves (mostly the 0.2-0.55 band), ends up roughly paired, holds everything to expiry, and lets redemption settle the books. The "whale" framing is wrong twice: median clip is $7.73, working capital ~$13k, and the $300k headline is pre-fee (real net ~$55k). It is the same family as our latency-fade lane, run smaller, faster, both-sided, and with zero exit logic.

## 5. Transferability to our stack ($50-500 taker clips, 115ms loop)

What transfers:

1. The fee boundary is the whole game. Their data is a clean natural experiment at $10M notional: gross edge 2.5-3.5% exists nearly everywhere, but net edge survives only where per-fill mispricing exceeds 0.07\*p\*(1-p). Entry bands 0.2-0.55 and 0.88+ survive; 0.6-0.8 does not. Our late_favourite entering at 0.88+ is validated; any taker entry in 0.6-0.88 needs model edge > ~1.7c/share just to cover fees.
2. BTC concentration. 82% of their net P&L is BTC despite ETH getting 25% of flow. Matches our alpha-hunt-002 finding (BTC-only edge, ETH rejected).
3. Hold-to-expiry both-sides is a legitimate exit-free architecture: no exit fees, no exit latency, redemption is the exit. Worth testing against our passive-exit lane on the same entries.
4. 15m markets carry comparable edge to 5m (btc-15m q0 0.55-0.88 band: +3.9%/$ net at the open). We do not trade 15m today; it nearly doubles the opportunity set with the same loop.

What does not transfer: their P&L per fill is tiny ($7.73 median clip, ~$0.08 net per fill). At $50-500 clips the stale-quote depth they nibble may not be there; expect materially worse fill quality at our size. Their 12 fills/min across 8 series also implies order-rate limits we should verify.

Backtest hypothesis (precise, falsifiable): on BTC 5m and 15m updown, every book tick, compute model fair p from spot displacement since window open (existing br2 belief). Submit IOC taker buy of $50 when ask is in [0.10, 0.55] and (fair - ask) > 0.07\*ask\*(1-ask) + 0.01, both sides eligible independently, no exits, hold to redemption, fee 0.07\*p\*(1-p) per share. Expected from this wallet's realized table: gross ~4-6% of stake in q1/q3 of the window, net ~1.5-3% of stake, win rate ~2-4 points above ask-implied. Falsified if net per-fill edge < 0.5% of stake at $50 clips on May-June replay. Secondary leg: same loop, buy at >=0.88 in the final two quintiles (their fav sleeve: net +1.1 to 1.7%), which is a direct A/B against our live late_favourite.

Caveats: winner per market inferred from redeem payout matching one side's share count exactly (29,691 of 29,695 both-sided markets matched to the cent); 1,526 one-sided no-redeem markets ($13.4k cost) treated as total losses; 9 both-sided markets ($1.5k) still pending redemption at pull time; W24 net is deflated by unredeemed in-flight markets. Maker/taker classification inferred from per-fill fee residual.
