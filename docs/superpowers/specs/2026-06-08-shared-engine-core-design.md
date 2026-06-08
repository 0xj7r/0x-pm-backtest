# Design: shared engine-core crate (`pm-engine`) — backtest ≈ live

Date: 2026-06-08
Status: DETAILED DESIGN (supersedes the locked-decision stub of the same name)

## Problem

`polymarket-backtest` (the `pm-app` walk-forward simulator) and `polymarket-agent`
(the `polymarket-exec` live runtime) are two codebases that share only the strategy
crates. The chosen baseline strategy, br2 (`BonereaperV2` in `pm-strategy`), is driven
through its contract `on_event_scored(event, ctx, spot, trades) -> (StrategyOutput,
Option<ModelOutput>)` at **two divergent call sites**:

- Backtest: `crates/pm-app/src/runner.rs:792` builds `Ctx` from parquet `ReplayEvent`s,
  fills via an idealized depth-sweep, and prices the NO leg synthetically as
  `1 - yes` (`runner.rs:2496`).
- Live: `polymarket-exec/src/runtime/br2_live.rs` builds an equivalent context from a
  shared `BookStore` + spot WS, submits through `ExecutionAdapter`, and accounts fills
  in `InventoryState`.

Two `Ctx` builders, two order translations, two fill/portfolio accountings. That is the
literal source of "what we backtest is not what we trade" — the #1 robustness threat,
and the likely reason thin edges (br2 +4.3% OOS, worst single market −$129) do not
transfer to live.

## Decision (locked 2026-06-08)

Build a **shared engine-core crate, `pm-engine`**, that both repos depend on. One
execution path; two thin drivers. "Backtest is live with a recorded feed and a
simulated exchange." Live constraints (latency, partial fills, order lifecycle,
reconciliation, shared-capital concurrency) shape the core; backtest is a faithful
simplification, never the reverse.

### Resolved scope decisions (2026-06-08 brainstorming)

1. **Lean core.** In-core: the per-event decision→execution→fill→position cycle, the
   risk gate (per-market + portfolio gross + drawdown + correlated-exposure cap), the
   portfolio/capital state, the order lifecycle, and the multi-market scheduler.
   In-driver: all operational/IO concerns (ws/connection management, venue
   reconciliation, kill-switch file, health/staleness watchdog, auto-redeem/wrap,
   checkpointing, data loading/replay). The backtest driver simply omits the live-only
   ops; the live driver wraps the core with them.
2. **Fill fidelity = real both-book + fees, conservative, no fitted model.** Replace
   synthetic NO=1−yes with the real NO ladder (both legs now ingested for all 12 cells),
   charge real taker/maker fees, apply a latency-aware book re-read, drive maker fills
   from the real trade tape (not top-of-book *touch*). The residual vs real on-chain
   fills is a reported validation metric, not a fitted parameter. The `Exchange` trait
   leaves room to add a calibrated queue model later if a cell proves to need it.
3. **Clean-slate first, then extract.** Land the br2-clean-slate cleanup (nuke
   BTE/router/native-MM, br2 sole strategy) as its own PR; extract `pm-engine` from the
   simplified runtime using the `br2_live` path as the live-driver template. Avoids two
   tracks rewriting the same ~3500-LOC `runner.rs`.
4. **Multi-market v1.** v1 includes the time-interleaved scheduler and shared-capital
   concurrency so backtest≈live holds at the *portfolio* level immediately (the live
   system already trades many markets against one capital pool; the backtest is
   currently serial-per-market — the "engine delta"). The two-level allocator and
   per-cell meta-calibrators still layer on top in step 3.

## Architecture

```
                    ┌───────────────── pm-engine (shared) ─────────────────┐
recorded Feed ─┐    │  Scheduler (ts-ordered) → StrategyHost(Ctx-builder)   │
  sim Exchange ─┼──▶ │   → RiskGate → Exchange.submit → Fill → Portfolio    │ ◀── br2 (pm-strategy, unchanged)
   sim Clock  ─┘    │  shared: InventoryState · RiskGate · ExposureState    │
   (backtest driver)└───────────────────────────────────────────────────────┘
   ws Feed ─┐                              ▲
 CLOB Exch ─┼──────────────────────────────┘   (live driver wraps engine + ops:
 wall Clock ┘                                    kill-switch, watchdog, redeem/wrap)
```

### The engine is synchronous and single-threaded

The core consumes ONE timestamp-ordered `EngineEvent` stream and contains no `async`,
no `tokio`, no `rayon`. All concurrency lives in the driver `Feed`/`Exchange` impls. This
is the keystone determinism decision: it eliminates `tokio::select!` arrival-order
nondeterminism, confines wall-clock to the live driver (the engine reads time only via
the injected `Clock`), and removes parallel model-state updates.

### Topology

By the established precedent (the agent path-deps `pm-types`/`pm-strategy`/`pm-risk` as
`../polymarket-backtest/crates/...`), `pm-engine` is a new crate in
`polymarket-backtest/crates/pm-engine`, depending on `pm-types`, `pm-strategy`,
`pm-risk`, `pm-model`. `pm-app` and `polymarket-exec` both become thin path-dep drivers.
`pm-engine` ships the trait definitions, the engine core, AND the *sim* implementations
(sim Exchange, recorded Feed, sim Clock), because both backtest and the conformance
harness need them. The *live* implementations (ws Feed, CLOB Exchange, wall Clock) live
in `polymarket-exec` (they depend on agent-specific wire code).

## Contracts

### Time and events

```rust
pub type Ts = i64; // nanoseconds since epoch — the single time currency

pub enum EngineEvent {
    Book   { market: MarketId, ts: Ts, book: BothBook },   // real YES + NO ladders
    Spot   { ts: Ts, tick: SpotTick },                      // carries is_buyer_maker
    Trade  { market: MarketId, ts: Ts, tick: TradeTick },   // on-chain print
    MarketOpen  { market: MarketId, ts: Ts, meta: MarketMeta },
    MarketClose { market: MarketId, ts: Ts, resolved_yes: bool },
}
```

`BothBook` carries both real depth ladders. The strategy's view is unchanged: the
strategy host projects the YES side of `BothBook` into the `ReplayEvent` that br2
already consumes, and keeps the NO ladder engine-internal for the fill model. So br2 is
hosted with zero changes; both-book fidelity is added underneath it.

`SpotHistory` and `TradeHistory` (the strategy's spot/trade inputs) are maintained by
the engine as bounded rolling windows appended from `Spot`/`Trade` events — replayed in
backtest, appended from WS live. br2 binary-searches them exactly as today.

### Seams (traits)

```rust
pub trait Feed {
    /// Next market event in timestamp order, or None at end-of-stream.
    /// Backtest: k-way merge of parquet tapes by ts (sync pull).
    /// Live: blocks on a merged receiver fed by the market/spot/user ws tasks.
    fn next(&mut self) -> Option<EngineEvent>;
}

pub trait Exchange {
    fn submit(&mut self, order: OrderIntent, now: Ts) -> SubmitAck;
    fn cancel(&mut self, id: OrderId, now: Ts) -> CancelAck;
    /// Fills realized since the last poll, given current market state / venue events.
    /// sim: compute from BothBook + resting orders + trade tape + latency.
    /// live: drain fills that arrived on the user ws.
    fn poll_fills(&mut self, now: Ts) -> Vec<FillReport>;
}

pub trait Clock {
    fn now(&self) -> Ts; // sim: last event ts; live: wall clock
}
```

Making `Feed::next` a synchronous pull (the live impl blocks on a merged channel) is
what keeps the engine async-free and deterministic. The live driver merges its three ws
channels into one ordered stream *before* handing events to the engine.

### Engine loop (deterministic)

```
loop {
  ev = feed.next();                        // ordered market event, or end
  apply ev to shared + per-market state    // book/spot/trade/open/close
  if ev is Book|Trade for a tradeable market {
    ctx = build_ctx(shared, market_ctx, model_out, exposure);   // the ONE Ctx builder
    (out, model) = strategy.on_event_scored(replay_view, &ctx, &spot, &trades);
    for order in out.orders {
      if risk_gate.check(&order, &shared) == Approve { exchange.submit(order, clock.now()); }
    }
  }
  for fill in exchange.poll_fills(clock.now()) {
    apply fill to InventoryState + per-market position + ExposureState (incremental);
  }
  if ev is MarketClose { settle resolution P&L; strategy.on_market_resolved(mid, resolved_yes); }
}
```

br2 reacts to its own fills via the next event's `Ctx` (updated positions), so no
`on_fill` hook is required — the existing `pm-strategy` trait suffices.

### Multi-market scheduler & correlated-exposure cap

The `Feed` already delivers all markets' events merged by timestamp (k-way merge in
backtest, arrival order in live). The engine routes each event by `MarketId` to its
`MarketCtx` (keyed lookup, never iterated for decisions). Shared portfolio/exposure
state is updated **incrementally on every fill**, so no map-iteration order can affect a
decision. `ExposureState` is keyed by `(token, window_bucket)` (overlapping settlement
windows for the same token) and holds net shares/notional; the risk gate consults the
incremental aggregate before approving an entry. v1 wires a configurable cap; the step-3
allocator sets the budgets.

### Strategy host / Ctx builder

A single `build_ctx(shared, market_ctx, model_out, exposure) -> Ctx` replaces the two
divergent builders. It fills the already-present cross-market fields
(`btc_net_exposure_shares`, `eth_net_exposure_shares`) from `ExposureState` with
identical logic in both drivers.

## Fill model (sim Exchange)

- Holds resting (maker) orders per market, the latest `BothBook`, and a recent trade
  tape per market.
- **Taker** (`limit_price = None`): on submit, record `submit_ts`; the fill realizes
  when `clock.now() >= submit_ts + latency`. Fill price = VWAP sweeping the **real
  opposing ladder** (YES asks for BuyYes; the **real NO asks** for BuyNo — not 1−yes)
  using the book snapshot at the fill time (latency-aware re-read; the book can move
  against you in flight), capped at `max_depth`. Charge real taker fee bps. Partial fill
  if depth is insufficient.
- **Maker** (`limit_price = Some`): rest in the book; fill only when the real trade tape
  prints across the level (the existing `check_trade_driven_resting_fills` logic,
  generalized to both ladders), never on a top-of-book *touch*. Charge maker fee/rebate.
  Partial by printed volume.
- **Residual diagnostic:** where real on-chain fills exist for the same market/window,
  the harness reports realized-vs-modeled fill-price error per cell. Validation metric,
  not a fitted parameter.

## Risk & portfolio (shared state)

The live `RiskEngine`/`RiskLimits` (`core/risk.rs`) is the stricter shape and becomes
the basis for the engine's single `RiskGate`: per-order notional, per-market net,
portfolio gross, free-cash floor, equity floor, drawdown throttle, open-order caps, and
the correlated-exposure cap. The backtest's `pm-risk::PortfolioState` drawdown/exposure
accounting folds in. Close/rescue intents bypass entry caps exactly as the live engine
does today. `InventoryState` (positions, cash, reservations, realized P&L,
mark-to-market) is the shared portfolio/capital state; the backtest's per-market
cash/yes/no-shares becomes a projection of it.

## Determinism

1. Sync, single-threaded engine over one ordered event stream — no `tokio::select!`, no
   `rayon` in the core.
2. `Clock` injected; no wall-clock (`SystemTime`/`Instant`/`ingested_at_ms`) in the
   decision path — those stay in the live driver's Feed/ops only.
3. Cross-market exposure maintained incrementally — no `HashMap`-iteration feeds a
   decision.
4. The `ModelState` calibrator is updated in strict event/close order by the
   single-threaded scheduler.
5. Any RNG in br2 is seeded from a fixed engine-config seed.
6. Floating-point summation order is fixed by the single-threaded event order.

## Conformance / equivalence harness (the backtest≈live proof)

- **Part A — engine determinism (golden trace):** run the engine with (recorded Feed,
  sim Exchange, sim Clock) over a fixed both-legs window; emit a canonical trace
  (ordered submitted orders, fills, position/equity snapshots). Re-run → byte-identical.
  CI test. Proves the core is deterministic.
- **Part B — driver equivalence (replay both ways):** capture a live WS window
  (book/spot/user) to disk via a small recorder. Build the SAME `EngineEvent` stream two
  ways: (1) the **live** driver's ws-Feed normalizer fed the captured WS bytes, (2) the
  **backtest** driver's parquet Feed fed the telonex parquet for the same wall-clock
  window. Assert the two `EngineEvent` streams are identical (ts-ordering + book/spot/
  trade contents within tolerance), then run both through the shared engine + sim
  Exchange → identical golden trace. Proves the only thing that can differ
  (driver-specific event construction) does not.
- **Champion equivalence:** the new engine, on the existing May BTC-5m cache, reproduces
  the frozen 062901 br2 result (P&L/fills) within tolerance — confirming the rewrite did
  not change br2's behavior.

If A + B + champion pass, a backtested decision/fill sequence is by construction what the
live engine would produce for the same market reality.

## Migration plan (no implementation code until the plan is approved)

- **Phase 0 (prerequisite, separate PR):** br2-clean-slate cleanup — nuke BTE, router,
  native MM strategies; br2 becomes the sole live strategy. (Specced separately:
  `2026-06-08-bte-removal-br2-clean-slate-design.md` in the agent repo.)
- **Phase 1:** create `pm-engine`; define `EngineEvent`/`BothBook`/`Feed`/`Exchange`/
  `Clock`; lift the `Ctx`-builder, `RiskGate` (from live `RiskEngine` + `pm-risk`), the
  shared `InventoryState` portfolio, the order lifecycle, the multi-market scheduler, and
  the incremental `ExposureState` into it. Host br2 via `pm-strategy` unchanged.
  Unit-test the core; no driver rewrite yet.
- **Phase 2:** implement the sim Exchange (real both-book + fees + latency + trade-tape
  maker fills), the recorded Feed (k-way parquet merge), and the sim Clock. Rewrite the
  `pm-app` walk-forward as a thin backtest driver. Land Part-A (golden trace) and
  champion-equivalence tests.
- **Phase 3:** rewrite the `polymarket-exec` runtime as a thin live driver — ws Feed
  (merge market/spot/user into one ordered stream), CLOB Exchange (wrap `ExecutionAdapter`
  + user-ws fills + reconcile), wall Clock; keep live-ops (kill-switch, watchdog,
  redeem/wrap, checkpoint) as a wrapper around the engine. Build the WS-window recorder
  and land Part-B (driver equivalence).
- **Step-3 layer (later, separate spec):** the two-level allocator + per-cell
  meta-calibrators on top of the engine (the scheduler + exposure cap already exist in
  v1).

## Open items / risks

- The sync-engine / async-driver boundary: the live `Feed::next` blocks on a merged
  channel that the ws tasks feed. Confirm latency budget is acceptable (the engine tick
  is cheap; the block is just channel recv).
- `ReplayEvent` strategy-view vs `BothBook` engine-internal split: `pm-types` may need a
  small `BothBook` addition; `ReplayEvent` itself need not change.
- Part-B requires a live WS recorder (small new tool in `polymarket-exec`).
- Branch tangle: this spec lands on a dedicated `shared-engine-core` branch; the eventual
  implementation should branch fresh off `main` (the stub was committed on
  `wallet-copytrade`, ingestion work on `cross-market-data-foundation`).

## Relationship to other specs

This is the engine substrate UNDER the cross-market framework. The cross-market routing,
per-cell calibrators, and concurrency-time allocation run ON this shared core. Sequence:
finish data ingestion → land clean-slate → build/extract `pm-engine` (this spec) → then
the cross-market layer on top.
