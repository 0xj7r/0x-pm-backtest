# btclong_xrp: BTC longer horizons + XRP full (W3)

Window W3 = 2026-05-07..05-18 (whipsaw regime). Holdout 2026-05-19..06-30 never
touched. Fade config: edge 0.12, 2 clips, rearm 0.08, min-marginal 0.04, ewma600
vol, latency 150ms, stop-before-close 90s, hold-to-redemption (exit-after 0), fee
curve 0.07. BTC uses BTC perp (weight 0.75); XRP is SPOT-ONLY (perp-weight 0).
Lane config: aligned, align-min-mid 0.85, edge 0.02, enter-within-close 120s.
Offline sigma floor (sigma_bar_bps): fade >= 3.0, lane >= 4.0.

## Headline: there is no 1h market

Settled against the master markets parquet (1,412,771 rows): Polymarket up/down
markets exist ONLY at 5m / 15m / 4h for every asset (btc, eth, xrp, sol, bnb,
doge, hype). There are zero `updown-1h-` slugs. So **BTC-1h and XRP-1h are
DATA-MISSING (the market does not exist)**. The longest available horizon is 4h,
which is what GOAL A's longer-horizon and deep-tail questions are answered on.

## GOAL A: BTC longer horizons

| cell | markets | trades | total $ | per-trade $ | hit% | t-stat | verdict |
|---|---|---|---|---|---|---|---|
| BTC-15m fade | 1151 | 1361 | +8437 | +6.20 | 61.2 | 4.10 | STRONG |
| BTC-4h fade | 49 | 58 | -649 | -11.19 | 56.9 | -2.38 | DEAD |

The sigma>=3 floor is a no-op in W3 (whipsaw vol keeps almost every entry above
3 bps): BTC-15m 1359 trades +8428, BTC-4h unchanged.

- **BTC-15m is the real longer-horizon edge.** Full coverage (1151/1152, 100%
  real-NO), t=4.10, +$6.20/trade. The edge concentrates in the `expanded_mixed`
  regime (+$16.57/trade, 455 trades) with `calm_low_vol` near flat (+$0.82). This
  is the same exogenous-fade mechanism as BTC-5m, still alive at 15m with the BTC
  perp in the belief.
- **BTC-4h fade is significantly negative** (t=-2.38). As hypothesised, the
  latency edge is gone on a 4h window (the book has hours to incorporate the perp
  move), and nothing replaces it: the fade just pays the spread on coin-flip
  outcomes. 49/72 markets had tapes (23 opened pre-W3, no W3 book).

### Deep cheap tail on 4h: DOES NOT EXIST

F11 proved BTC-5m/15m never show an ask below ~0.26 near close. The 4h hypothesis
was that a losing side has hours to decay to <=0.05. Scanned with the real
two-sided book (`deep_tail_long`, 35/34 markets carrying a real NO ladder):

| metric (final 20% of window) | BTC-4h | XRP-4h |
|---|---|---|
| markets with min ask (either side) <= 0.05 | 0 | 0 |
| markets with min ask <= 0.10 | 0 | 0 |
| markets with min ask <= 0.20 | 0 | 0 |
| global min yes_ask | 0.46 | 0.49 |
| global min no_ask (real Down) | 0.46 | 0.45 |
| losing-side min buy, final 40 / 20 / 10% | 0.42 / 0.42 / 0.44 | 0.49 / 0.49 / 0.49 |
| winner-side max yes_bid (sanity) | 0.58 | 0.55 |

**No <=0.10 or <=0.05 deep-tail level exists on either 4h book**, even in the
final 10% of the window. The reason is structural, not a data gap: the winning
side's bid only climbs to ~0.55-0.58 near close, i.e. 4h BTC/XRP up/down outcomes
stay genuinely uncertain to the wire (the strike is the price 4h earlier and the
asset can cross it until the last minutes). The cheap-tail-maker question is
therefore closed across every Polymarket horizon: 5m, 15m, and 4h all lack the
sub-0.10 underdog ask the strategy needs.

## GOAL B: XRP full (spot-only)

| cell | markets | trades | total $ | per-trade $ | hit% | t-stat | verdict |
|---|---|---|---|---|---|---|---|
| XRP-5m fade | 2204 | 822 | +1684 | +2.05 | 47.2 | 1.89 | MARGINAL-POSITIVE |
| XRP-15m fade | 766* | 321 | -89 | -0.28 | 33.6 | -0.13 | DEAD |
| XRP-4h fade | 51 | 28 | -110 | -3.93 | 42.9 | -0.98 | DEAD |
| XRP-5m lane | 1111 | 353 | -204 | -0.58 | 93.5 | -0.82 | DEAD |
| XRP-15m lane | 0 | 0 | - | - | - | - | NO-FILL |
| XRP-4h lane | 51 | 1 | +5 | - | - | - | NO-FILL |

\* XRP-15m ran on a partial subset (766/1152, book coverage gap); it is negative
even on that subset so full ingest was not justified under disk discipline.

- **XRP-5m fade is the only XRP edge, and it is marginal.** On the full 12-day W3
  (2204 markets, vs the prior 6-day-core +$434 at ~68% coverage) it is +$1,684,
  +$2.05/trade, t=1.89. The edge survives a larger sample but is weak and not
  cleanly significant. It lives in `expanded_high_flip` (+$5.26/trade, 223
  trades) with `expanded_mixed` thinly positive (+$0.95) and `calm_low_vol`
  negative (-$4.25). It is a whipsaw-regime fade, not an all-weather one.
- **XRP-15m and XRP-4h fade are both negative**: the exogenous edge does not
  survive past 5m on XRP. XRP is a thinner, slower follower asset; by 15m the
  book has already priced the spot move.
- **The lane is dead on XRP.** XRP-5m lane is -$204 at 93.5% hit (it wins small
  and loses big, classic negative-skew with no edge). At 15m/4h the
  align-min-mid 0.85 entry within 120s of close almost never triggers (0 and 1
  fills), so the lane has no expression on slower XRP horizons.

## Verdict

XRP is **structurally marginal, not dead** (unlike SOL where the book
out-forecasts the model): the XRP-5m fade is a real but weak whipsaw-regime edge
(+$2.05/trade, t=1.89), and everything slower (15m, 4h) or via the lane is
negative. It does not earn a standalone allocation; at most it is a small
diversifier inside the all-weather book during high-flip tape. The genuinely
useful longer-horizon find is **BTC-15m fade** (+$6.20/trade, t=4.10, full
coverage), which extends the BTC exogenous-fade edge one horizon up. BTC-4h is
dead and the deep cheap tail does not exist at any horizon.

## Method notes

- 4h books were never in the local cache; ingested per-asset (YES + real Down
  via down_all) for W3 only from the project S3 (visumlabs profile,
  `scripts/ingest_4h_w3.sh`), tapes rebuilt with the merged NO ladder, then the
  ingested raw was the only added footprint (~1 GB, deleted after scoring).
- Deep-tail scan: `crates/pm-app/src/bin/deep_tail_long.rs` reuses the workspace
  `BookTick` (exact bincode layout) over the merged tape cache; scans final
  40/20/10% per market on both the YES ask and the real Down ask, prints the
  min-ask histogram plus a winner-side max-bid sanity line.
- sigma scoring: `scripts/score_sigma_floor.py` over each cell's trades.jsonl.
