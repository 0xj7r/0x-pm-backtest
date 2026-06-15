# f11: deep cheap-underdog tail (taker and maker) on BTC short-horizon

Verdict: NO-EDGE (premise fails before economics: the <=0.05 underdog book level does not exist near close in BTC-5m/15m W3).

## Hypothesis

Two profitable wallets (0x8d1d, 0x2855) make money in the cheap-underdog tail on
BTC short-horizon markets, entered late, as makers. The question was whether
buying a deep underdog (ask <= 0.05) in the final part of a BTC-5m/15m window,
held to redemption, is positive EV, first as a taker (our execution model), then
(per the mid-study pivot) as a one-sided resting maker bid.

## Data and method

- Tapes: merged two-sided `BookTick` cache in `data/cache/ticks/{date}`, W3
  2026-05-07..05-18 (NEVER touched 05-19..06-30). Files are
  `{asset_id}.{1s|2s}.btc` = `PTC2` magic + zstd(bincode `Vec<BookTick>`); `.2s.`
  carries the real Down ladder, `.1s.` is YES-only.
- Manifests: `data/manifests/canonical/btc-updown-{5m,15m}_up.jsonl` (asset_id =
  Up token, official `outcome` Up/Down).
- Window: `[close_ts - duration, close_ts]` from the manifest; per tick
  `frac = (ts - start)/duration`, with final-40/20/10% = frac >= 0.60/0.80/0.90.
- For each window I scanned both sides (YES ask = Up, NO ask = Down) for asks
  <= 0.05 in buckets 0.01-0.02 / 0.02-0.03 / 0.03-0.05, recording whether that
  side ultimately won, the fee-inclusive priced rate (ask + 0.07*p*(1-p)), a
  maker fill model (resting bid at L in {0.02,0.03,0.05} fills when the side ask
  crosses <= L), capacity from the ask ladder, and a stale-vs-true split (ask
  later climbs > 0.10 = revived).
- Tooling: `crates/pm-app/src/bin/f11_deep_tail.rs` (reuses the workspace
  `BookTick` so the bincode layout is exact). Cross-checked independently with a
  standalone Python tape decoder (byte layout verified: implied bytes == file
  bytes for a 159,816-tick tape).

## Result: the cheap tail is not in the book

Across all 4,608 W3 BTC-5m and BTC-15m markets (4,596 with a tape):

| metric (final 20% window) | value |
|---|---|
| markets with min yes_ask <= 0.05 | 0 |
| markets with min yes_ask <= 0.10 | 0 |
| markets with min yes_ask <= 0.20 | 0 |
| markets with min yes_ask <= 0.35 | 145 |
| global min yes_ask (all 4,596) | 0.26 |
| global min no_ask (1,177 with real NO) | 0.34 |

Per-horizon, independently re-derived in Python on full days:

- BTC-5m 2026-05-12 (288 markets): min yes_ask in [close-300, close] = 0.31;
  zero cheap tick-events; full ask-LADDER (all 5 levels) min price = 0.31, so it
  is not just top-of-book clamping.
- BTC-5m 2026-05-12 distribution: 0 markets <=0.20, 2 <=0.35, 125 <=0.50, 161 >0.50.
- BTC-15m 2026-05-12 and 05-15 (96 each): min yes_ask = 0.43 / 0.45; nothing
  below 0.35.

Consequence: every downstream table is empty by construction.

- Realized-vs-priced upset rate by bucket: no observations (n = 0 in every
  price x time-to-close cell). There is no realized rate to compare to the
  priced rate because no <=0.05 ask ever exists in-window.
- Taker P&L sim (final 20%, $7.50 clip, hold to redemption): 0 entries, $0 net.
- Maker fill model: 0 fills at every level (0.02/0.03/0.05) x window
  (40/20/10%) across 5,771 side-opportunities. Fill rate 0.000 everywhere,
  capacity $0, so adverse-selection on fills is undefined (no filled tickets).
- Stale-vs-true: 0 / 0.

## Why the premise fails

BTC-5m is effectively a coin-flip on a 5-minute BTC move, and BTC-15m on a
15-minute move. Even the trailing side stays meaningfully probable until the
final second because a few minutes of BTC vol can flip a small lead, so the book
correctly never prices either side below ~0.26 (5m) or ~0.43 (15m) at close. The
deep <=0.05 underdog tail that the whales harvest is simply not a resting book
level in this horizon/data: it would have to be a transient stale quote during a
Binance spike, a sub-tape-resolution flicker, or it lives on a different
asset/horizon entirely. As a resting maker bid at 0.02-0.05 you would never get
filled here, and as a taker there is nothing to lift.

This is a verified negative, checked three ways (workspace-typed Rust binary,
independent Python byte-decoder on best-ask, independent Python on the full
5-level ask ladder), so it is a property of the data, not a window-mapping or
bincode bug. Window alignment was confirmed: the last in-window ticks land
exactly at `close_ts` (frac = 1.000).

## Verdict

NO-EDGE for both expressions on BTC-5m and BTC-15m in W3.

- Not VIABLE-TAKER-SLEEVE and not VIABLE-MAKER-SLEEVE: there is no entry to
  define because the <=0.05 level never appears in the final 10-40% of the
  window.
- Not ADVERSELY-SELECTED in the usual sense (we cannot even get filled) and not
  CAPACITY-TOO-THIN (capacity is exactly zero, not merely thin).
- The whales' cheap-tail edge does not reproduce on BTC short-horizon resting
  book data. Before retrying, point the same scanner at (a) longer horizons
  (1h/4h, where one side can genuinely die early) and (b) the actual wallet
  fill prices to confirm whether their "cheap" fills are transient stale quotes
  rather than resting levels. Until then there is nothing to deploy.

## Reproduce

```
cargo run --release --bin f11_deep_tail
# diagnostic histogram of in-window min ask:
F11_DEBUG=1 cargo run --release --bin f11_deep_tail 2>&1 | grep '^DBG'
```
