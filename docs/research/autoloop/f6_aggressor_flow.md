# F6: Polymarket aggressor-flow fade (book moves without a spot move)

Date: 2026-06-12. Script: `scripts/f6_flow_fade.py`. Events: `docs/research/autoloop/f6_events.jsonl`.

## Verdict: NO-SIGNAL

Book moves that occur without a corresponding Binance spot move are NOT fadeable on BTC-5m updown books. They show zero net reversion (the gross edge at the fade side's own mid is -0.2c to +0.1c per share, statistically zero), so a taker fade loses almost exactly its costs: about -2.1c/share at 30s in W2 (t = -9.8, n = 3344), of which 1.52c is the 0.07*p*(1-p) fee and 0.59c is spread/impact. Negative at every horizon, in both windows, in both vol regimes, with win rates 0.40-0.45. No harness feature or live module is warranted.

## Question

Our validated fade monetizes spot moves against a stale book. F6 asks the inverse, targeting calm regimes: when taker flow moves the BOOK while spot is stationary, does the book revert (fadeable), versus book moves accompanied by spot moves (information, should not revert)?

## Data and method

- Markets: btc-updown-5m from `data/manifests/fullhist/btc5m-20260212-20260520-full-28205.jsonl`. W2 sample: 10 markets/day for 2026-04-01..04-30 (300). W3 contrast: 50 markets sampled from 2026-05-01..05-18 (slightly broader than the canonical 05-07..05-18; no date at or after 2026-05-19 was touched). 335 ran, 15 skipped (missing tape/strike).
- Book: `data/cache/ticks/<date>/<asset_id>.2s.btc` (PTC2 bincode+zstd of the harness BookTick series). Important format finding: despite the `.2s` suffix these are EVENT tapes at sub-second cadence (~100k ticks per 300s market), not 2s snapshots.
- Spot: Binance BTCUSDT agg_trades parquet from `data/cache/raw/binance/...` (the same source the harness SpotCache uses). The `transact_time_ms` column is actually microseconds.
- Event definition: resample the book to 2s snapshots; a burst is |yes-mid move| >= 3c within <= 2 snapshots (<= 4s). Spot move over the same interval classifies it: < 2bps = flow-driven, >= 4bps = information-driven, 2-4bps = ambiguous. 5s cooldown, events need 120s of market left, fade entry price clipped to [0.02, 0.98].
- Fade: buy the OTHER side at its post-burst ask (real NO ladder when present, else synthetic complement), fee 0.07*p*(1-p), mark to mid at +30/60/120s. Reversion fraction rev = (mid_post - mid_h) / dmid (1 = full reversion, negative = continuation).

A first pass at raw event-tape cadence was degenerate: with ticks milliseconds apart, spot can never move 4bps "within 2 ticks", so the info class was empty (0 of 1068 events). The 2s resample restores the intended semantics; that pass is what is reported.

## Results

4451 events total. W2 flow opportunities: 11.1 per market, roughly 3200/day at 288 markets/day (abundant, but worthless).

W2 (2026-04, n flow = 3344, amb = 520, info = 106), fade pnl per share at the taker ask, mark-to-mid:

| class | h | rev_frac mean/med | pnl mean | pnl med | win | t |
|---|---|---|---|---|---|---|
| flow | 30s | -0.05 / 0.00 | -2.14c | -2.10c | 0.42 | -9.8 |
| flow | 60s | -0.12 / -0.14 | -2.33c | -2.66c | 0.43 | -7.7 |
| flow | 120s | -0.04 / -0.33 | -2.03c | -3.99c | 0.45 | -4.7 |
| amb | 120s | -0.45 / -0.89 | -4.06c | -8.07c | 0.42 | -3.4 |
| info | 120s | +0.27 / +0.07 | +0.52c | -0.94c | 0.46 | +0.2 |

W3 (n flow = 425): strictly worse, flow fade -3.2c to -3.4c at every horizon (t -2.4 to -5.3), rev_frac -0.18 to -0.22 (continuation).

Vol split (W2 flow, trailing 600s spot sigma, median 0.39bps/s): low-vol -1.74c to -2.02c, high-vol -2.32c to -2.65c. The calm-regime version of the hypothesis is the less bad cell but still decisively negative.

Loss decomposition (W2 flow): a fee-free fade resting at the other side's own mid would earn rev * |dmid| = -0.03c (30s), -0.23c (60s), +0.08c (120s). The signal is zero gross; the taker loss is entirely fee (1.52c mean) plus spread/impact (0.59c mean). Maker execution cannot rescue a zero-gross signal.

## Classifier sanity and mechanism

The hypothesized asymmetry (flow reverts, info does not) is absent and if anything inverted: info-class moves show MORE mean reversion at 60-120s (+0.27 to +0.31) than flow-class moves (~-0.1). So the classifier separates inputs but the premise fails: a 3c book move with stationary spot is not retail noise, it is a repricing that sticks.

Mechanism check (60-market W2 subsample, n = 633 flow events): forward spot move over the next 60s in the burst direction is +0.23bps mean, 50% positive, i.e. flow bursts do not lead spot either. They are permanent book-level repricings: plausibly the book catching up to sub-2bps spot drift, perp-led fair-value updates (we already blend perp at 0.5 live), or quote-ladder adjustments near extremes, none of which mean-revert. Flow moves end beyond the BSM fair (strike proxy = spot at open, trailing 600s vol) 55% of the time, barely above coin-flip, so they are not even reliably "overshoots".

## Honest limits

- The 2s resample smears sub-second burst microstructure; a genuine lift-the-ask burst and a quiet two-sided requote between snapshots look identical. A trade-print tape (aggressor side and size) would classify properly; the book cache alone cannot.
- Marks are at the mid, no depth walking, no latency. These are OPTIMISTIC simplifications and the fade still loses, which strengthens the rejection.
- Strike proxy is Binance spot at window open (official strikes ~14bps lower); irrelevant for deltas and reversion, mildly noisy for the BSM-fair comparison.
- The 2-4bps ambiguous band was measured rather than dropped (it behaves like info: strong continuation).

## Disposition

Close F6. Do not build a tape-derived aggressor feature for the harness. If aggressor flow is revisited it needs actual trade prints with aggressor flags (Polymarket trades feed), not book snapshots, and the burden of proof is on overcoming a measured zero gross edge.
