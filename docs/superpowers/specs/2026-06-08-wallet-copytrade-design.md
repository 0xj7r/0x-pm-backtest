# Wallet Copy-Trade: Historical + Live Performance Tracking

Date: 2026-06-08
Branch: `wallet-copytrade`
Status: Approved design, pre-implementation

## Goal

Measure the performance of copying a specific Polymarket wallet, under realistic
execution latency. Two run modes share one P&L core:

1. **Historical** replay of the leader wallet's past trades, pricing each copy
   entry at `fill_time + latency`, settled to market resolution.
2. **Live (forward paper)** tracking: as the leader's new trades arrive over a
   real-time feed, record a modelled copy entry and settle on resolution.

First target wallet: `0xb55fa1296e6ec55d0ce53d93b9237389f11764d4`.

The central question this answers: **how much of the leader's edge survives the
latency a copier actually faces.**

## Scope and non-goals

In scope: a new `pm-copytrade` crate plus a `pm-app copy-trade` subcommand;
data acquisition from live HTTP APIs; a per-trade copy ledger; proportional
position sizing against a reconstructed leader-equity curve; settlement to
resolution; a results summary consistent with existing JSONL outputs; a live
paper-tracker fed by the Polymarket real-time feed.

Out of scope: executing real orders / signing transactions (paper only);
integration with the S3 `book_snapshot` replay engine and `run_backtest`
(see "Why not the replay engine"); generic leaderboard discovery of which
wallets to copy (the wallet is supplied by the caller).

## Why not the existing replay engine

The repo's backtester (`pm-app walk-forward` -> `runner.rs::run_backtest`)
replays S3 `book_snapshot` tapes for BTC-5m markets, with the wallet-attributed
`onchain_fills` archive covering roughly Feb 12 - Apr 28 2026.

The target wallet does not fit this path on three independent axes:

- **Time.** The wallet's first trades are ~mid-May 2026; it is entirely after
  the `onchain_fills` archive window. Its fills are not in S3.
- **Markets.** It trades 15-minute UP/DOWN markets across ETH/SOL/BTC/XRP, not
  BTC-5m. `book_snapshot` tapes for these markets are unconfirmed and the
  discovery/spot-symbol path is BTC/ETH-only (`infer_spot_symbol_from_slug` in
  `crates/pm-app/src/discovery.rs` returns `None` for SOL/XRP).
- **Cadence.** It trades hundreds of times per day; a faithful copy needs the
  leader's fill stream, not a market book tape.

So the historical copy is built on **live HTTP sources**, not S3 replay. This
also dissolves the "multi-asset coverage" problem: we follow whatever markets
the wallet actually traded, discovered through its activity feed, rather than
extending S3 discovery filters.

What we still reuse from the repo: `pm-types` (`Outcome`, value types),
`pm-risk::PortfolioState` (bankroll equity, drawdown, per-market outlay caps,
`fractional_kelly_stake`), the existing `reqwest` client conventions
(`discovery.rs::fetch_availability` shows the retry/429 pattern), and the
atomic JSONL result-writing convention used by walk-forward.

## Leader behaviour (verified 2026-06-08)

Over the last ~500 activity events: 459 `TRADE` rows (all `BUY`), 0 `SELL`,
41 `REDEEM`; 0 assets show both a buy and a sell. The wallet **buys and holds
every position to resolution**, then redeems winners. This makes copy P&L a
sequence of independent binary bets held to settlement: no exit-mirroring,
no early-close logic.

## Data sources

| Need | Source | Notes |
|------|--------|-------|
| Leader's fills (entries) | Data API `GET /activity?user=<w>&type=TRADE` | Window-walked; offset capped at 3000/window. The `wallet_profiler` repo's `sources/activity.py` is the reference implementation of the time-bucket walk. |
| Copy entry price at `fill_ts + L` | Data API `GET /trades?market=<token>` (market trade prints) | Primary. Find the print nearest to `fill_ts + L`. Fine-grained, unlike prices-history. |
| Entry price fallback | CLOB `GET /prices-history?market=<token>` | ~60s granularity at `fidelity=1`; fallback when no print in the latency window. |
| Market resolution / winning outcome | Gamma `GET /markets?...` `outcomePrices` | After resolution, `["1","0"]`-style payoff per outcome. |
| Live leader fills | RTDS `wss://ws-live-data.polymarket.com`, topic `activity` type `trades` | Carries `proxyWallet`. Per-wallet server-side filter is broken (real-time-data-client issue #34): subscribe to the firehose, filter by `proxyWallet` client-side. |
| Live alternative | On-chain `OrderFilled` logs via Polygon WS RPC, indexed `maker` topic = wallet | Server-side per-wallet filter, lowest latency. Heavier to decode. Phase 2 option. |

CLOB `/prices-history` retention only needs to cover the wallet's ~3-4 week
history; verify during implementation but low risk.

## Architecture

New workspace crate `crates/pm-copytrade`, plus one subcommand in `pm-app`.

```
crates/pm-copytrade/
  src/
    lib.rs          # public entrypoints: run_historical(cfg), LiveTracker
    sources/
      activity.rs   # window-walked leader fills (Data API)
      prices.rs     # market trade prints + prices-history fallback -> price_at(token, ts)
      resolution.rs # Gamma outcome lookup + cache
      rtds.rs       # live firehose client, proxyWallet filter (live mode)
    model.rs        # LeaderFill, CopyEntry, CopyResult, RunConfig
    equity.rs       # reconstruct leader equity curve from fills + resolutions
    sizing.rs       # proportional stake from leader fraction x our bankroll
    ledger.rs       # CopyLedger: applies entries, settles, wraps PortfolioState
    summary.rs      # aggregate stats + JSONL writer
  Cargo.toml        # deps: pm-types, pm-risk, reqwest, tokio, serde, serde_json,
                    #       chrono, anyhow, thiserror, clap (re-exported types only)

crates/pm-app/src/main.rs  # add `CopyTrade` subcommand -> pm_copytrade::run_historical / live
```

### Core types (`model.rs`)

```rust
pub struct LeaderFill {
    pub ts: i64,                 // unix seconds
    pub token_id: String,        // ERC1155 outcome token (the "asset" field)
    pub condition_id: String,
    pub slug: String,
    pub outcome: String,         // e.g. "Up"/"Down"/"Yes"/"No"
    pub side: Side,              // always Buy for this wallet, but modelled generally
    pub price: f64,              // leader's own fill price
    pub size: f64,               // shares
    pub usdc: f64,               // leader notional
}

pub struct CopyEntry {
    pub leader: LeaderFill,
    pub latency_s: f64,
    pub entry_price: f64,        // price_at(token, ts + latency)
    pub our_stake_usdc: f64,     // from sizing
    pub our_shares: f64,         // stake / entry_price
}

pub struct CopyResult {
    pub entry: CopyEntry,
    pub won: bool,               // bought outcome resolved true
    pub payout_usdc: f64,        // won ? our_shares : 0
    pub pnl_usdc: f64,           // payout - our_stake
    pub resolved_ts: i64,
}
```

### Data flow: historical

1. **Acquire** leader fills via window-walked `/activity` over `[start, end]`,
   deduped, sorted ascending by `ts`. Group by `condition_id` for resolution
   lookup and by `token_id` for price lookup.
2. **Reconstruct leader equity** (`equity.rs`): seed with `--leader-seed-usdc`
   (default configurable; sensitivity reported), then walk the leader's fills
   and resolution payoffs in time order to produce `leader_equity(ts)`. Each
   leader buy subtracts `usdc`; each resolved win adds `shares` (=`1` payout per
   winning share). This is self-consistent with data we already pull.
3. **Size** (`sizing.rs`): for each leader fill, `fraction = usdc / leader_equity(ts)`;
   `our_stake = fraction * our_bankroll_now`, clamped by `PortfolioLimits`
   (per-market and daily caps) via `PortfolioState::can_open_position`.
4. **Price the copy** (`prices.rs`): `entry_price = price_at(token, ts + L)` for
   each swept latency `L`. Runs are produced per latency value.
5. **Settle** (`ledger.rs`): on the market's resolution, `won = (bought outcome
   is the winning outcome)`; `pnl = won ? our_shares*(1 - entry_price) :
   -our_stake`. Mark `PortfolioState` equity forward; compounding flows into the
   next trade's `our_bankroll_now`.
6. **Summarise** (`summary.rs`): per-latency ROI, win rate, total P&L, Sharpe,
   max drawdown, average hold time, and breakdown by asset (BTC/ETH/SOL/XRP) and
   duration (15m/1h/4h). Write per-trade ledger JSONL + a summary JSON, atomic.

### Data flow: live (forward paper)

A `LiveTracker` connects to RTDS, subscribes to `activity/trades` (empty
filter), and keeps only messages where `proxyWallet == target`. For each kept
buy it: reads current best ask (CLOB `/book` or the message price), applies the
configured latency `L` (wait `L` then re-read price), computes `our_stake` from
the live-reconstructed leader equity, appends a `CopyEntry`, and schedules a
resolution check. A resolution poller (Gamma, or on-chain `ConditionResolution`
reusing the existing `resolution_watcher` pattern from the research repo) closes
open entries and appends `CopyResult`s to the same ledger file the historical
mode writes, so both modes share one analysis surface.

### Sizing math (proportional, leader-equity-anchored)

```
fraction_t   = leader_fill_usdc_t / leader_equity_t      # leader's conviction
our_stake_t  = clamp( fraction_t * our_equity_t , limits )
our_shares_t = our_stake_t / entry_price_t
```

`leader_equity_t` is reconstructed (chosen over the `/value` snapshot and the
fixed-fraction alternatives). `our_equity_t` is tracked by `PortfolioState` and
compounds with realised P&L. The leader seed is a config knob; the summary
reports results across a small seed sweep so the reader sees seed sensitivity.

## Configuration (`RunConfig`)

```
--wallet <addr>              target leader (required)
--mode historical|live
--start / --end              historical window (defaults: wallet first trade .. now)
--latency-s 0,2,5,15         swept; one result set per value
--our-bankroll 100           our starting equity (mirrors engine --starting-cash default)
--leader-seed-usdc 1000      seed for leader equity reconstruction (+ sweep for sensitivity)
--max-clip-usdc / --daily-cap / --per-market-cap   -> PortfolioLimits
--out-ledger / --out-summary paths (JSONL + JSON, atomic writes)
--price-source trades|prices-history|auto   (default auto: trades then fallback)
```

## Error handling

- HTTP: reuse the `discovery.rs` retry/backoff + 429 `Retry-After` pattern;
  bounded concurrency for the activity walk and per-token price fetches
  (the wallet's high cadence means thousands of fills).
- Missing price at `ts + L`: fall back to prices-history, then to leader fill
  price flagged `priced_from="leader_fill"`; count and report these.
- Unresolved / still-open markets in the historical window: excluded from
  realised stats, counted separately as open exposure.
- Live disconnects: exponential backoff reconnect; on reconnect, backfill any
  missed leader fills via a `/activity` catch-up query so the ledger has no gap.
- Fail loud on a bad wallet (no activity), a 400 on the first activity page, or
  an empty Gamma resolution for a market that should be resolved.

## Testing

- `sources/activity`: window-walk pagination, offset-ceiling stop, dedup across
  overlapping windows (fixture pages).
- `prices::price_at`: nearest-print selection around `ts + L`; fallback ordering;
  empty-window behaviour.
- `equity`: reconstruction on a hand-built fill+resolution sequence; known
  equity curve.
- `sizing`: fraction math, limit clamping, compounding.
- `ledger`: win/loss settlement P&L, drawdown, multi-market interleave.
- `summary`: aggregate stats on a fixed ledger fixture (ROI, win rate, Sharpe,
  per-asset breakdown).
- `rtds`: proxyWallet filtering on a captured message fixture; reconnect/backfill
  logic with a fake socket (mirror the research repo's `resolution_watcher` test
  style).
- End-to-end: a small recorded fixture (a handful of the leader's real fills +
  their resolutions) run through `run_historical`, asserting a stable summary.

## Outputs

- `--out-ledger` JSONL: one `CopyResult` per line (leader fill, latency, entry
  price + source, stake, shares, won, pnl).
- `--out-summary` JSON: per-latency block with ROI, total P&L, win rate, Sharpe,
  max drawdown, trade count, priced-from-fallback count, and asset/duration
  breakdowns.
- Console table via a `summarize` path consistent with `result_summary.rs`.

## Key risks / open items

- **Latency-price granularity** is the crux of the modelled-latency number.
  Market trade prints (`/trades?market=`) are the primary source for exactly
  this reason; if their per-market coverage is thin, sub-15s latency results
  carry a documented caveat.
- **Leader-equity seed sensitivity:** mitigated by sweeping the seed and
  reporting the spread, not a single number.
- **Live firehose volume:** the RTDS `activity/trades` stream is platform-wide;
  client-side `proxyWallet` filtering is cheap but the connection must keep up.
  On-chain `OrderFilled` (server-side filtered) is the phase-2 upgrade if needed.
- **Resolution timing in live mode:** a market resolves minutes-to-hours after
  the leader's entry; the resolution poller must persist open entries across
  process restarts (ledger is the source of truth on restart).

## Build sequence (high level; detailed plan via writing-plans)

1. `pm-copytrade` crate skeleton + `model.rs` types + Cargo wiring.
2. `sources/activity.rs` + `sources/prices.rs` (+ tests) — acquire fills, price at latency.
3. `sources/resolution.rs` (+ cache, tests).
4. `equity.rs` + `sizing.rs` (+ tests).
5. `ledger.rs` settlement over `PortfolioState` (+ tests).
6. `summary.rs` + JSONL/JSON writers (+ tests).
7. `pm-app copy-trade` historical subcommand; end-to-end fixture run on the target wallet.
8. `sources/rtds.rs` + `LiveTracker` + live subcommand (+ tests); resolution poller with restart-safe ledger.
9. Multi-asset summary normalisation (per-asset/duration breakdown; no BTC-5m `MARKETS_PER_DAY` assumption).
