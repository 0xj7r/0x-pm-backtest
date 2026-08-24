# Data and algorithm scope for the TWAP-era research cycle

Date: 2026-08-24. Companion to net-new-candidates-2026-08.md; turns each
candidate into concrete feeds, algorithms, and infra. Status codes:
HAVE (on disk or in S3), GAP (must acquire), BUILD (must implement).

## 1. Data

### 1.1 Feeds inventory

| Feed | Source | Granularity | History | Status | Needed by |
|---|---|---|---|---|---|
| Polymarket book_snapshot_25 | Telonex | ~1s L2 | local Feb-Jul 10; S3 mirror to Jun 30 | HAVE to Jul 10; GAP Jul 11-now (Telonex Pro quota) | everything |
| Polymarket trades (aggressor) | Telonex | tick | same window | HAVE / same GAP | flow tape, maker program |
| Binance spot agg_trades BTC/ETH | data.binance.vision | tick | Feb 12-Jul 10 local | HAVE; GAP to now, FREE | beliefs, everything |
| Binance perp agg_trades + metrics | data.binance.vision | tick / periodic | Feb 12-Jul 10 | HAVE; GAP free | perp-weighted belief |
| Binance 1H klines | data.binance.vision | 1h | 2019-Jun 30 | HAVE; GAP free | hourly (Binance-true) candidate |
| Chainlink stream prints (BTC/ETH) | Polymarket RTDS ws (crypto_prices, twap topics) or Chainlink Data Streams direct | sub-second | none before ~Aug | GAP: no history vendor known; must self-record from NOW | residual strike, TWAP fade, replica basis |
| Official partial/settlement TWAP prints | RTDS twap_sixty topic + market resolution records | per print | reconstructable per market from Gamma/data-api | GAP: capture + backfill resolved values | settlement verification, era model |
| Liquidity-reward score inputs | our own book capture (depth at 1.5c band per minute) | 1min samples | derivable from book_snapshot_25 | BUILD (derivation) | maker program |
| Taker-rebate cashflows | Polymarket data-api (activity) + docs formula | per fill | formula public | BUILD (accounting) | fade + taker accounting |
| Liquidation feed | CoinGlass free tier or Binance forceOrder ws | event | self-record | GAP (small) | quote masks |
| Macro calendar (CPI/FOMC/NFP) | static calendar file | event | trivial | BUILD (tiny) | quote masks |
| Kalshi 15m books (optional) | Kalshi API | tick | not started | DEFER | cross-venue, later |
| Deribit DVOL | Deribit public | 1min | 2021-Jun 30 | HAVE; GAP free | context only |

Priority actions, in order:
1. Start the Chainlink/RTDS recorder NOW (no vendor sells this history; every
   day not recorded is gone). Tiny always-on process writing RTDS topics
   crypto_prices + twap_sixty + book tops to S3; ~pennies/day. This slightly
   amends the "Telonex Pro only" data decision: Telonex does not carry the
   Chainlink stream, so this one recorder is unavoidable for candidates 1-3.
2. Free backfills (binance.vision spot/perp/klines, Deribit) Jul 11-now.
3. Telonex Pro backfill Jul 11-now for books/trades when the quota is enabled;
   Aug 14+ first (live regime), then Jul 11-Aug 13.
4. VERIFY step before relying on it: one week of recorded RTDS prints compared
   against official settlement values = the replica-basis haircut measurement
   (net-new candidate 10).

### 1.2 Storage and access

Everything lands in the existing hive layout on s3://pm-research-data-prod
(raw/chainlink/... new prefix mirroring raw/binance). Backtests read via
pm-telonex-loader S3 mode or ephemeral-instance local sync (Plan 3 runner).
No new database; parquet partitions by date/asset as today.

## 2. Algorithms per candidate

### C1 Residual-strike engine
- State: locked partial average Abar(t) over [T-60, t] from recorded stream;
  residual strike K* = (60K - e*Abar)/r; remaining-average law
  Var = sigma^2 r^3 / (3*60^2) (already in pm_alpha::twap_digital).
- Decision: taker entry when |twap_digital - book_ask| > fee(p) + spread +
  replica_haircut; exits by redemption (sub-minute holds).
- Estimation needs: 1s realized vol (existing VolEstimator), jump flag
  (threshold on 1s return vs rolling MAD), replica haircut (from data step 4).
- Backtest form: event replay at 750ms with era model ON; entries confined to
  final 90s; scorecard by seconds-to-close bucket.

### C2 TWAP fade
- Same belief stack as C1 plus a persistence classifier for wicks: features =
  1s return, 5s reversal fraction, perp-spot basis change, trade-flow imbalance
  (all computable from HAVE feeds). Start as two thresholds (jump size,
  reversal fraction), not ML; the falsified-models history says features
  before model class.
- Reuses fade plumbing: threshold entry, rearm, fractional sizing; belief is
  twap_digital instead of the snapshot BSM.

### C3 Pre-window 1/3 rule (maker leg)
- No new math (twap_digital case A); needs the maker fill model: queue position
  from our 19,435-record calibration, plus the reward-score simulator (below).

### C4 Nested term structure
- Algorithm: per timestamp, remaining-window CDF for every open window on the
  asset (5m/15m/1h/4h) from one shared vol estimate; z-score each book's mid vs
  model; trade richest-vs-model subject to fees; the pair variant (coincident
  5m/15m closes) trades the digital spread on shared A_T.
- Needs: multi-window manifest join (BUILD: extend discovery to emit
  overlapping-window sets); Feb-Jul testable immediately at snapshot-era rules.

### C5 Binance-true hourly
- Diagnostic first (measurement, not strategy): regression of 1h CLOB mid on
  (Binance remaining-digital, Chainlink remaining-digital) in the last 10 min.
- If slow-book confirmed: classic remaining-digital taker at 750ms with UMA
  capital-lock modeled (2h), Binance 1H kline basis (HAVE), fee-aware.

### C6 Maker program
- Reservation price: twap_digital; quoting in logit space.
- Inventory: Feil-Nendel terminal penalty gamma_T q^2 p(1-p) + GLT hard gates;
  taker unwind rule (Guilbaud-Pham) when |Abar-K| > 2 sigma_rem.
- Adverse selection: OFI/microprice/VPIN computed on Binance spot+perp (HAVE
  feeds), used ONLY as pull/widen signals; thresholds calibrated on the flow
  tape (M1 below).
- Reward objective: simulator of the public score formula: S = ((v-s)/v)^2 * b,
  per-side sums, 3:1 balance clamp, two-sided requirement outside [0.10,0.90],
  1-min sampling; maximize expected reward + rebate subject to an
  adverse-selection budget and |q_T| cap.
- Fill model: calibrated queue model (fill = f(queue position, book state)),
  NOT pro-rata; this is the piece the public farmers lack.

### Cross-cutting measurements (gate everything)
- M1 flow tape: maker/taker x price band x seconds-to-close x session, post
  Aug 14 (needs Telonex backfill).
- M2 rebate incidence: cheap-side taker share before/after May 28.
- M3 replica haircut (data step 4).
- M4 hour-boundary oracle same-side rates (Feb-Jul, HAVE).

## 3. Engine work implied (Plan 2/3 backlog additions)

1. Settlement-era model (already Plan 2 Task 9) + resolution from recorded
   TWAP values when labels absent.
2. Rebate/reward accounting: taker-rebate wV cashflow, 20% maker rebate,
   reward-score simulator as a scorecard module.
3. Maker fill model as a first-class engine mode (queue-position calibrated),
   replacing the research-only maker path that was reviewed as untested.
4. Multi-window replay (several concurrent markets per asset with shared spot
   state) for C4; the portfolio path already iterates markets, needs
   shared-clock joins.
5. RTDS/Chainlink recorder service (tiny, always-on, S3 sink) + loader.
6. Strategy trait additions: access to partial-average state and
   seconds-to-close (thin extensions of Ctx after the Plan 2 slim).

## 4. Sequencing (data-constrained critical path)

1. NOW: recorder (item 5) because history is unrecoverable; free binance
   backfills.
2. Feb-Jul testable immediately: C4 nested (snapshot era), M4 oracle rates,
   C5 diagnostic (1H klines + books to Jul 10).
3. On Telonex Pro: M1 flow tape, then C1/C2 on Aug 14+ tape as it accumulates
   (a few weeks of TWAP-era data is the binding constraint for any verdict;
   sealed-window discipline applies).
4. Maker program last: it depends on M1-M3 plus engine items 2-3, and its
   prize is the smallest; but its quote-mask sub-experiments (masks, sessions)
   can piggyback on any maker paper-run earlier.
