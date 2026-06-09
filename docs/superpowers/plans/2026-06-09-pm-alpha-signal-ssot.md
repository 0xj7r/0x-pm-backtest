# pm-alpha Signal SSOT Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the canonical leakage-free exogenous signal + edge-model crate (`pm-alpha`) and a latency-modeled validation harness, then run the first honest measurement of exogenous edge vs the Polymarket book on May data.

**Architecture:** New workspace crate `pm-alpha` depending only on `pm-types`. Leakage is enforced structurally: exogenous signals receive an `ExoState` view that physically contains no Polymarket book data; the book exists only in the harness for cost/fill. The BSM binary fair value is ported from `polymarket-agent/polymarket-exec/src/signals/fair_value.rs`. The harness replays per-market book tick series with configurable entry latency, depth-walking fills, and taker fees, and reports log-loss vs the book-implied baseline plus net-of-cost EV per token x window cell, swept over latency. A `pm-app` adapter (`alpha` subcommand) feeds it from the existing telonex cache.

**Tech Stack:** Rust edition 2024, workspace crates pm-types/pm-telonex-loader/pm-app, serde, clap. Data: local cache (Binance agg_trades BTC Feb 12 to Jun 8; Polymarket book_snapshot_25 May 1 to Jun 4; markets manifest with `outcome` labels at `data/manifests/may2026_focused/markets_btc.jsonl`).

**Ground truth decisions (from spec + data audit):**
- Strike = Binance spot at window open via `SpotHistory::price_at_or_after(open_ns)` (Chainlink not in our data; documented proxy risk, spec section 13).
- Outcome label = manifest `outcome` field mapped by `outcome_label_resolved_yes` (walkforward.rs:1492); YES = "Up".
- Default latency 150 ms (agent `paper_submit_latency_ms` default); sweep {0, 50, 150, 300, 500, 1000} ms.
- Splits: tune on May 1-18, test May 19-28, final untouched holdout Jun 1-4. Nothing is fitted on the holdout.
- BTC 5m only first (only deep cell); ETH later.

---

### Task 1: Crate skeleton, ExoState contract, leakage-by-construction

**Files:**
- Modify: `Cargo.toml` (workspace members + pm-alpha in workspace deps)
- Create: `crates/pm-alpha/Cargo.toml` (deps: pm-types via workspace, serde)
- Create: `crates/pm-alpha/src/lib.rs`
- Create: `crates/pm-alpha/src/state.rs`

Core contract (state.rs):

```rust
use pm_types::SpotHistory;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token { Btc, Eth, Sol, Xrp }

#[derive(Debug, Clone, Copy)]
pub struct MarketMeta {
    pub token: Token,
    pub window_secs: u32,      // 300 or 900
    pub open_ts_ns: i64,
    pub close_ts_ns: i64,
    pub strike: f64,           // underlying level to beat (open price)
}

/// Everything an exogenous signal may see. Contains NO Polymarket book data
/// by construction; this is the leakage guarantee (type-level, not convention).
pub struct ExoState<'a> {
    pub spot: &'a SpotHistory,
    pub market: MarketMeta,
    pub now_ns: i64,
}

impl ExoState<'_> {
    pub fn time_remaining_s(&self) -> f64 { ... (close - now) in secs, floored at 0 }
    pub fn tau_fraction(&self) -> f64 { remaining / window, clamped [0,1] }
}
```

- [ ] Steps: write `state.rs` with tests (`tau_fraction` clamps; `time_remaining_s` floors at 0), add crate to workspace, `cargo test -p pm-alpha` passes, `cargo build --workspace` clean, commit `pm-alpha: crate skeleton + ExoState exogenous contract`.

### Task 2: Fair-value core (BSM binary), ported from agent

**Files:**
- Create: `crates/pm-alpha/src/fair_value.rs`
- Read first: `/Users/jackreid/go/polymarket-agent/polymarket-exec/src/signals/fair_value.rs` (port math verbatim, keep API shape)

Math: `P(Up) = Phi(ln(S/K) / sigma_remaining)`, `sigma_remaining = sigma_bar * sqrt(tau_fraction)`, A&S 7.1.26 erf CDF. Momentum variant: `P(Up) = Phi((delta + momentum*tau) / (sigma*sqrt(tau)))` with `delta = (S-K)/K`. Edge cases: t < 1s or sigma < 1e-12 -> step function; invalid inputs -> NoSignal with p=0.5.

- [ ] Steps: port `FairValueEstimate`, `FairValueModel`, `NoSignalReason`, `estimate_fair_value`, `estimate_fair_value_with_momentum`, `standard_normal_cdf`; port the agent's tests plus: ATM -> 0.5 exactly, monotone increasing in spot, higher vol pulls p toward 0.5, decided fallback at t<1s. `cargo test -p pm-alpha`. Commit `pm-alpha: BSM binary fair value ported from agent fair_value.rs`.

### Task 3: Realized-vol estimator

**Files:**
- Create: `crates/pm-alpha/src/vol.rs`

```rust
/// Realized vol of the underlying over a trailing window, expressed as the
/// stdev of log-returns over one BAR horizon, in basis points.
/// Samples last-price at fixed dt (default 1s) over `lookback_s` (default 1800s),
/// stdev of log returns * sqrt(bar_secs / dt_secs) -> bps.
pub fn realized_vol_bps_over_bar(
    spot: &SpotHistory, now_ns: i64,
    lookback_s: u32, sample_dt_s: u32, bar_secs: u32,
) -> Option<f64>
```

Returns None when < 30 valid samples. Tests: constant price -> ~0; synthetic alternating +x/-x returns recover known sigma within 10%; sparse history -> None.

- [ ] Steps: implement + tests, `cargo test -p pm-alpha`, commit `pm-alpha: trailing realized-vol estimator (bar-horizon bps)`.

### Task 4: The edge model (belief from ExoState only)

**Files:**
- Create: `crates/pm-alpha/src/model.rs`

```rust
pub struct AlphaModelConfig {
    pub vol_lookback_s: u32,       // 1800
    pub vol_sample_dt_s: u32,      // 1
    pub momentum_lookback_s: u32,  // 0 = disabled (base model)
    pub momentum_weight: f64,      // drift scale when enabled
}
pub struct Belief { pub p_up: f64, pub sigma_bar_bps: f64, pub model: FairValueModel }
pub fn belief(state: &ExoState, cfg: &AlphaModelConfig) -> Option<Belief>
```

`belief` takes ONLY `ExoState` (the leakage guarantee). Base: spot at-or-before now, strike, time remaining, realized vol -> `estimate_fair_value`. With momentum enabled: trailing_return over `momentum_lookback_s`, scaled to one-bar horizon, fed to the momentum variant. Returns None if spot/vol unavailable.

- [ ] Steps: implement + tests (no-vol -> None; deterministic; momentum 0-weight equals base), commit `pm-alpha: exogenous edge model (base + optional momentum drift)`.

### Task 5: Validation harness (latency, fills, costs, metrics)

**Files:**
- Create: `crates/pm-alpha/src/harness/mod.rs`, `harness/types.rs`, `harness/replay.rs`, `harness/metrics.rs`

```rust
// types.rs
pub struct BookTick { pub ts_ns: i64, pub yes_bid: f32, pub yes_ask: f32,
                      pub bids: [BookLevel; 5], pub asks: [BookLevel; 5] }
pub struct MarketSeries { pub meta: MarketMeta, pub resolved_yes: bool,
                          pub ticks: Vec<BookTick>, pub date: String }
pub struct HarnessConfig {
    pub latency_ms: u64,           // decision at T, fill vs book at first tick >= T+latency
    pub taker_fee_bps: f64,
    pub edge_threshold: f64,       // enter when p_exo - cost (or inverse) > threshold
    pub notional_usdc: f64,        // depth-walked
    pub decision_dt_ms: u64,       // evaluate cadence (default 1000)
    pub stop_before_close_s: u32,  // no entries in final N secs (default 10)
}
```

Replay per market: at each decision time compute `belief` (ExoState only); buy-YES edge = p_up - eff_ask, buy-NO edge = (1 - p_up) - (1 - eff_bid); one entry per market (first threshold crossing), fill by walking depth at the latency-shifted tick, fee on notional; settle at resolution ($1 or $0). Record per-decision samples (p_exo, book mid, outcome) for log-loss.

Metrics (metrics.rs): per cell (token x window) and aggregate: n_markets, n_trades, total/mean net EV, hit rate, log-loss(p_exo) vs log-loss(book mid) on identical sample set, sampled at fixed checkpoints (60s/120s/180s/240s into window). `run_sweep(series, cfgs) -> Vec<(latency_ms, CellReport)>`.

Required tests (spec section 10): synthetic dislocation -> net EV monotonically non-increasing in latency; zero-edge series with fees -> negative EV; identical inputs -> identical metrics (run twice, compare serialized); log-loss of perfect p beats book mid on a constructed series.

- [ ] Steps: types + replay + metrics with tests, `cargo test -p pm-alpha`, commit `pm-alpha: latency-modeled validation harness (fills, costs, log-loss vs book)`.

### Task 6: pm-app adapter + `alpha` subcommand

**Files:**
- Create: `crates/pm-app/src/alpha.rs`
- Modify: `crates/pm-app/src/main.rs` (new `Cmd::Alpha` variant + args)
- Modify: `crates/pm-app/src/walkforward.rs` (make `load_replay_events_for_market`, `SpotCache`, `outcome_label_resolved_yes` `pub(crate)` if not already)
- Modify: `crates/pm-app/Cargo.toml` (+ pm-alpha)

Flow: read markets jsonl (`MarketHandle`), filter slug prefix (btc-updown-5m) + date range args, for each market: replay events via `load_replay_events_for_market` (reuse --replay-event-cache-dir), spot day via `SpotCache::get_or_load`, open_ns = close_ts - window, strike = `spot.price_at_or_after(open_ns)` (skip market + count if missing), ticks = events with BOOK_UPDATE flag mapped to BookTick, resolved_yes from outcome label. Run `pm_alpha::harness::run_sweep`, print table, write `data/runs/alpha/<stamp>/report.json`.

CLI: `pm-app alpha --markets <jsonl> --date-start --date-end --latency-ms 150 --latency-sweep --edge-threshold 0.05 --fee-bps 0 --notional 50`.

- [ ] Steps: implement, `cargo build --workspace`, smoke run on May 1-3 local cache, sanity-check output (n markets > 0, log-losses finite), commit `pm-app: alpha subcommand feeding pm-alpha harness from telonex cache`.

### Task 7: The hunt (base, then families, greedy per spec section 6)

- [ ] Tune ONLY edge_threshold + vol_lookback on May 1-18 (grid: threshold {0.03,0.05,0.08,0.12}, vol_lookback {900,1800,3600}); pick by net EV at 150 ms.
- [ ] Run frozen config once on May 19-28. Report per spec section 7: log-loss vs book baseline AND net-of-cost EV, plus latency curve.
- [ ] Family A, momentum/drift: momentum_lookback {60,300}, weight {0.5,1.0}, tuned May 1-18 only; keep only if May 19-28 net EV improves.
- [ ] Family B, CEX order-flow: signed taker flow + large-print arrival + acceleration from `SpotHistory::signed_flow_and_adverse` / `spot_returns_and_accel`, as a drift/confidence adjustment. Same gate.
- [ ] Family C, vol/regime gate: stand down in low-vol chop and extreme whipsaw (gates entries, never predicts direction). Same gate.
- [ ] Write `docs/alpha-hunt-001-base.md` with the ranked answer and the configuration count (multiple-testing disclosure). Do NOT touch Jun 1-4.
- [ ] Commit results doc.

### Task 8: ML calibration layer (exogenous-only), full history

The end-state belief is calibrated ML, not raw BSM. Once the base + surviving families have a first read:

- [ ] Lift the calibrator machinery (Beta + Isotonic + GBT, `pm-model/src/lib.rs:1009`) into `pm-alpha/src/calibrator.rs` with a compact exogenous-only feature vector (moneyness-in-vol-units, tau, sigma, momentum stack, flow stats, regime). No book features, enforced by constructing features from `ExoState` only.
- [ ] Train on the full historical span (S3 has PM data + manifests back to Feb 12; local May window for iteration), walk-forward folds, against realized outcomes only.
- [ ] Gate identically: keep calibration only if it improves OOS log-loss AND net EV on May 19-28.
- [ ] Final: single untouched run on Jun 1-4 holdout for whatever survived Tasks 7-8.

Direction-agnostic note: the harness measures belief quality vs book (taker lens) first because it is the cheapest honest test, but the same calibrated `p_exo` is the input a maker/quoting engine needs (quote around belief, not around mid). If taker EV is negative everywhere but log-loss beats the book, the maker route is the next consumer (memory: br2-future-maker-direction).

**Verification:** every task ends with `cargo test -p pm-alpha` (and `--workspace` for pm-app changes) green before commit. Task 7's numbers come from actual runs, never estimated.
