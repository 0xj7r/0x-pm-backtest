# Markout attribution of the live capture gap

Question (polymarket-mechanics-opportunities.md, TEST CHEAP item 4): attribute the live capture deflator (laddered fills realize ~0.45 of at-touch backtest P&L) using short-horizon markouts from the shadow telemetry.

Data: Dublin shadow stream, `~/data/pm-alpha/shadow/shadow-2026*.jsonl`, window 2026-06-10 12:19 UTC to 2026-06-12 11:10 UTC. 124 would_enter events, each with a quote_probe at decision +150ms (median 159ms, p10 150ms, p90 160ms) and a would_exit at decision +30s; 112 have resolutions. 102 trades carry full ladder telemetry (the first 22 predate the ladder fields), 4 exits had no quote and were dropped. All trades are BTC 5m up/down, median entry touch 0.53, median model edge 0.185.

## Capture in this window

| basis | at-touch P&L (same shares) | live ladder P&L | capture |
|---|---|---|---|
| 30s mark (the live exit policy) | $877.77 | $581.62 | 0.663 |
| settlement (hold to resolution) | $360 | $78 | 0.216 |

The cited 0.45 sits between the two bases; this window's mark-basis capture (0.663) is better than the historical live figure. One accounting caveat cuts the other way: the at-touch counterfactual above fills the full ladder size (mean 95 shares) at the decision touch. Capping the at-touch fill at the size actually quoted at the touch gives only $509.26, which the live ladder beats. Part of the historical "0.45 gap" is therefore backtest size fiction (the backtest fills a clip the touch never displayed), not execution loss.

## Component decomposition of the mark-basis gap ($296.15, n=102)

Per trade the buy is decomposed as touch (decision) to current touch (+150ms) to ladder average (depth walked beyond the new touch), and symmetrically on exit. The three components sum exactly to the gap.

| component | $ | share of gap | per-share mean | notes |
|---|---|---|---|---|
| entry quote decay / race loss (decision touch to +150ms touch) | $204.42 | 69.0% | +0.027 | zero when still_quoted; +0.059 mean when raced, p95 +0.13, p99 +0.21 |
| walking the book beyond the new touch (size vs depth) | $78.11 | 26.4% | +0.009 | the cost of taking 95 shares where the touch shows ~12 |
| exit-side ladder slippage (exit touch to ladder exit avg) | $13.62 | 4.6% | +0.001 | exits land into deep books near expiry; negligible |

still_quoted rate: 66/120 = 0.550, so the race is lost on 45% of signals. Capture splits cleanly on it: still-quoted trades capture 0.886, raced trades capture 0.472.

## Win/loss asymmetry (adverse selection on the race)

- By settlement outcome: winners were still quoted only 43% of the time; losers 75%. We keep the full at-touch price mostly when the signal is wrong.
- Conditional on being raced, eventual losers decayed further (+0.088/share) than eventual winners (+0.058/share): chasing into whipsaws buys the top.
- Despite this, raced trades are still positive EV after the chase ($223 ladder P&L on $474 at-touch).

## Markout horizons: what is and is not measurable

The event cadence gives exactly three priced marks per trade: +150ms (quote_probe), +30s (would_exit), and settlement. There are no priced events at +1s or +5s; no market re-enters within the window (0 of 124), and summary/venue_lead_lag events carry no per-market prices. So the +1s/+5s markouts requested in the spec are not measurable from this stream. What is measurable:

- Edge surviving entry at +150ms: per-share 30s-mark edge falls from +0.0895 (at decision touch) to +0.0532 after the ladder entry, so 59% of the mark edge survives the entry.
- On a settle basis, +0.0659 falls to +0.0308, so 47% survives. The entry slip is a fixed ~3.5c/share toll, which is why settle-basis capture (0.216) looks so much worse than mark-basis: the toll is the same but the settle edge per share is thinner.

## Counterfactual execution policies (mark basis, replayed on the 102 trades)

| policy | P&L | vs baseline |
|---|---|---|
| baseline ladder (chase the book) | $581.62 | 1.00 |
| skip trade if entry slip > 2c | $373.84 | 0.64 |
| skip trade if entry slip > 5c | $544.99 | 0.94 |
| IOC at decision touch only (no chase) | $255.31 | 0.44 |
| hybrid: IOC when quoted, chase if slip <= 3c | $355.38 | 0.61 |

Every slip cap and IOC variant loses money. Chased fills, even at +6c average decay, retain positive expectancy because the median signal edge (0.185) is much larger than the slip. Do not add slip caps or abort-on-race logic.

## The single biggest leak, and the fix

The leak is losing the 150ms race on 45% of signals (69% of the gap, and the depth-walk component is partly downstream of it since the raced touch is thinner). The race window is almost entirely our own signal feed latency: median Binance receipt lag is 101ms (p90 105ms, n=2,815 summaries) while the Polymarket book feed lags only 9ms. The counterparties who beat us to the touch are reading the same Binance move with a faster pipe.

The recoverable number: converting raced trades to still-quoted quality (capture 0.472 to 0.886) is worth about $196 over this 2-day window, lifting mark capture from 0.663 toward ~0.89. The execution change is to cut Binance-to-decision latency (lower-latency Binance feed or a relay near the matching path), not to change order placement. The remaining 26% (walking the book for size beyond displayed depth) is the irreducible price of being a taker at our clip size; the only structural answer to that piece is the maker direction already on the roadmap.

Caveats: 2 days, 102 trades, one regime (June 10-12), shadow ladder fills are simulated against the observed book and assume no market impact; the 250ms itode hold is not modeled in the probe (the 150ms probe understates the true uncancelable exposure window, so live race loss is likely somewhat worse than measured here).
