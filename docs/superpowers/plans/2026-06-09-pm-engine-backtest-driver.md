# pm-engine Phase 2 — faithful BTC-5m backtest driver — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drive the `pm-engine` core on real recorded BTC-5m data through a thin `pm-app` backtest driver, with a real both-book fill model and a shared `CtxEnricher`, replacing the synthetic-NO walk-forward loop for this cell.

**Architecture:** Extend the `Exchange` seam so the engine streams book/trade state to it; build a `SimExchange` (real opposing-ladder VWAP, latency, fees, trade-tape maker fills) and a `CtxEnricher` (regime + model-eval + prior-range) inside `pm-engine`; in `pm-app`, pair each market's YES/NO legs, load both-legs parquet into one timestamp-ordered `EngineEvent` stream, and run the engine. Validate by determinism + a plausibility tripwire + a reported both-book P&L delta. No champion byte-reproduction.

**Tech Stack:** Rust (workspace), `pm-engine`/`pm-app`/`pm-telonex-loader`/`pm-strategy`/`pm-model`, `cargo test`, recorded telonex parquet (`data/cache/raw/telonex/...`).

---

## Prerequisites & data

- Phase 1 (`pm-engine` core) is landed on branch `shared-engine-core`; continue there.
- **Data:** the BTC-5m May both-legs `book_snapshot_25` + `trades` cache must be present under `data/cache/raw/telonex/exchange=polymarket/channel={book_snapshot_25,trades}/date=2026-05-*/asset_id=.../` (and Binance spot for the same range). If absent locally, pull from the mirror `s3://pm-research-data-prod/raw/telonex/` (use `--profile visumlabs`) per the existing `scripts/prep_cache.sh`. Tasks 1–6 use small synthetic fixtures and need NO real data; only Task 8 (the real backtest) needs the cache.
- Established Phase-1 facts: `MarketId(pub u32)` (now `Ord`, `BTreeMap`-keyed in `Portfolio`); `EngineEvent::Market{replay: ReplayEvent, no_book: NoBook}`; `Engine<S>` sync loop calls `on_market` then drains `Exchange::poll_fills`; `OrderIntent{...,kind:IntentKind}`; `RiskGate::check(order, portfolio, marks, exposure, key, signed)`.

## File structure

- Modify: `crates/pm-engine/src/seams.rs` — add `Exchange::on_book` + `on_trade` (default no-ops).
- Modify: `crates/pm-engine/src/engine.rs` — call `exchange.on_book(...)` per Market event; hold + invoke a `CtxEnricher`.
- Create: `crates/pm-engine/src/sim_exchange.rs` — `SimExchange` (real both-book fills).
- Create: `crates/pm-engine/src/enrich.rs` — `CtxEnricher` (regime + model-eval + prior-range).
- Modify: `crates/pm-engine/src/lib.rs` — module wiring.
- Create: `crates/pm-app/src/engine_driver.rs` — leg-pairing, parquet→`EngineEvent` loading, the thin driver, validation/reporting.
- Modify: `crates/pm-app/src/main.rs` — `--engine` flag routing to the new driver (keep the old loop for the delta comparison).

---

## Task 1: Extend the Exchange seam to observe book + trades

The real fill model needs the current book and trade tape. Add observation methods to the `Exchange` trait (default no-ops so the Phase-1 `InstantExchange` is unaffected) and have the engine push state before polling fills.

**Files:** Modify `crates/pm-engine/src/seams.rs`, `crates/pm-engine/src/engine.rs`; test in `crates/pm-engine/tests/engine_integration.rs`.

- [ ] **Step 1: Add the trait methods (default no-op)**

In `seams.rs`, add to `trait Exchange` (after `poll_fills`):
```rust
    /// Latest book state for a market (YES ladder via ReplayEvent + real NO ladder).
    /// Default no-op so book-agnostic exchanges (e.g. test InstantExchange) ignore it.
    fn on_book(&mut self, _market: pm_types::MarketId, _replay: &pm_types::ReplayEvent,
               _no_book: &pm_types::NoBook, _now: Ts) {}
    /// A real on-chain trade print for a market (drives maker fills).
    fn on_trade(&mut self, _market: pm_types::MarketId, _tick: &pm_types::TradeTick, _now: Ts) {}
```

- [ ] **Step 2: Wire the engine to push book state**

In `engine.rs` `on_market`, immediately after `self.marks.insert(e.market_id, e.yes_mid);` and before the decision block, call `ex.on_book(e.market_id, e, no_book, clock.now())`. (You will need `on_market` to receive `no_book`: change the `EngineEvent::Market{replay, no_book}` match arm in `run` to pass `no_book` into `on_market`, e.g. `self.on_market(&replay, &no_book, ex, clock)`.) Trade prints: Phase 2 surfaces trades via the recorded Feed in Task 7; for now `on_trade` stays uncalled (no Trade event variant yet) — leave a `// Task 7 wires trades` comment where the Feed will emit them.

- [ ] **Step 3: Verify Phase-1 tests still pass**

Run: `cargo test -p pm-engine --features testkit`
Expected: 29 passed, 1 ignored (the default no-op `on_book` keeps `InstantExchange` behavior identical).

- [ ] **Step 4: Commit**

```bash
git add crates/pm-engine/src/seams.rs crates/pm-engine/src/engine.rs
git commit -m "pm-engine: Exchange observes book/trade state (default no-op)"
```

---

## Task 2: SimExchange — taker fills against the real opposing ladder

**Files:** Create `crates/pm-engine/src/sim_exchange.rs`; wire in `lib.rs`.

- [ ] **Step 1: Write the failing test (synthetic both-book fixture)**

In `sim_exchange.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{BookLevel, MarketId, NoBook, ReplayEvent, ReplayFlags};
    use pm_engine_seams_side::*; // adjust to real module paths: crate::seams::*

    fn ev(yes_ask0: f32, yes_ask_sz: f32) -> ReplayEvent {
        let mut asks = [BookLevel::default(); 5];
        asks[0] = BookLevel { price: yes_ask0, size: yes_ask_sz };
        ReplayEvent { ts_ns: 1000, market_id: MarketId(0), yes_mid: 0.5, yes_bid: 0.49,
            yes_ask: yes_ask0, volume: 0.0, bids: Default::default(), asks,
            spot_price: 0.0, flags: ReplayFlags::BOOK_UPDATE }
    }

    #[test]
    fn buy_yes_taker_fills_at_real_ask_vwap_with_fee() {
        let mut ex = SimExchange::new(SimExchangeConfig { taker_latency_ms: 0, taker_fee_bps: 100.0, maker_rebate_bps: 0.0 });
        let e = ev(0.60, 1000.0);
        ex.on_book(MarketId(0), &e, &NoBook::default(), 1000);
        let id = OrderId(1);
        ex.submit(OrderIntent { id, market: MarketId(0), side: Side::BuyYes, shares: 100.0,
            max_depth: 1, limit_price: None, tag: "t", kind: IntentKind::Entry }, 1000);
        let fills = ex.poll_fills(1000);
        assert_eq!(fills.len(), 1);
        assert!((fills[0].price - 0.60).abs() < 1e-6);          // real YES ask, not 1-yes
        assert!((fills[0].fee_usd - 100.0*0.60*0.01).abs() < 1e-6); // 100bps on notional
    }

    #[test]
    fn buy_no_taker_fills_against_real_no_ask_not_one_minus_yes() {
        let mut ex = SimExchange::new(SimExchangeConfig { taker_latency_ms: 0, taker_fee_bps: 0.0, maker_rebate_bps: 0.0 });
        let e = ev(0.60, 1000.0);
        let mut nb = NoBook::default();
        nb.asks[0] = BookLevel { price: 0.45, size: 1000.0 }; // real NO ask 0.45, NOT 1-0.60=0.40
        ex.on_book(MarketId(0), &e, &nb, 1000);
        ex.submit(OrderIntent { id: OrderId(2), market: MarketId(0), side: Side::BuyNo, shares: 100.0,
            max_depth: 1, limit_price: None, tag: "t", kind: IntentKind::Entry }, 1000);
        let fills = ex.poll_fills(1000);
        assert_eq!(fills.len(), 1);
        assert!((fills[0].price - 0.45).abs() < 1e-6); // real NO ask
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p pm-engine sim_exchange`
Expected: FAIL (type `SimExchange` not found).

- [ ] **Step 3: Implement SimExchange taker path**

Implement `SimExchangeConfig { taker_latency_ms: u64, taker_fee_bps: f64, maker_rebate_bps: f64 }`, `SimExchange` holding per-market latest `(ReplayEvent, NoBook)` (from `on_book`), a resting-order list, and a pending-fill queue. Implement `Exchange`:
- `on_book`: store the latest `(replay, no_book)` for the market.
- `submit` (taker, `limit_price=None`): compute the VWAP across the **real opposing ladder** — `BuyYes`/`SellNo` sweep YES `asks`/`bids`; `BuyNo` sweeps the **NO `asks`**, `SellNo`... (map per `Side`), up to `max_depth`. **Port the depth-sweep VWAP from `runner.rs:2486 depth_weighted_fill`, but replace the synthetic `1 - yes` (`runner.rs:2496-2503`) with the real `NoBook` ladder.** With `taker_latency_ms==0`, enqueue a fill at `now`; else at `now + latency_ms*1_000_000` (Task 3). Charge `taker_fee_bps` on notional.
- `poll_fills(now)`: return fills whose realize-ts `<= now`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p pm-engine sim_exchange`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-engine/src/sim_exchange.rs crates/pm-engine/src/lib.rs
git commit -m "pm-engine: SimExchange taker fills against real both-book ladder + fees"
```

---

## Task 3: SimExchange — latency + partial fills

**Files:** Modify `crates/pm-engine/src/sim_exchange.rs`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn taker_fill_is_deferred_by_latency() {
    let mut ex = SimExchange::new(SimExchangeConfig { taker_latency_ms: 500, taker_fee_bps: 0.0, maker_rebate_bps: 0.0 });
    let e = ev(0.60, 1000.0);
    ex.on_book(MarketId(0), &e, &NoBook::default(), 1_000_000_000);
    ex.submit(OrderIntent { id: OrderId(1), market: MarketId(0), side: Side::BuyYes, shares: 100.0,
        max_depth: 1, limit_price: None, tag: "t", kind: IntentKind::Entry }, 1_000_000_000);
    assert!(ex.poll_fills(1_000_000_000).is_empty());          // not yet (t+0)
    assert_eq!(ex.poll_fills(1_000_000_000 + 500_000_000).len(), 1); // at t+500ms
}

#[test]
fn taker_partial_fills_when_depth_insufficient() {
    let mut ex = SimExchange::new(SimExchangeConfig { taker_latency_ms: 0, taker_fee_bps: 0.0, maker_rebate_bps: 0.0 });
    let e = ev(0.60, 40.0); // only 40 shares at top
    ex.on_book(MarketId(0), &e, &NoBook::default(), 1000);
    ex.submit(OrderIntent { id: OrderId(1), market: MarketId(0), side: Side::BuyYes, shares: 100.0,
        max_depth: 1, limit_price: None, tag: "t", kind: IntentKind::Entry }, 1000);
    let fills = ex.poll_fills(1000);
    assert_eq!(fills.len(), 1);
    assert!((fills[0].shares - 40.0).abs() < 1e-6); // only available depth filled
}
```

- [ ] **Step 2: Run to verify they fail; Step 3: implement; Step 4: verify pass**

Run: `cargo test -p pm-engine sim_exchange` (expected: 4 passed). Implement the pending-fill realize-ts gate (latency) and cap the swept shares at available depth (partial). **Latency semantics port `PendingTakerOrder` / `process_pending_takers` from `runner.rs:693-706`** (fill realizes on the first poll at/after the target ts; price uses the book at realize time — re-read from the latest `on_book`).

- [ ] **Step 5: Commit**

```bash
git add crates/pm-engine/src/sim_exchange.rs
git commit -m "pm-engine: SimExchange taker latency + partial fills"
```

---

## Task 4: SimExchange — maker fills (book-cross + trade-tape)

**Files:** Modify `crates/pm-engine/src/sim_exchange.rs`.

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn maker_fills_on_book_cross() {
    let mut ex = SimExchange::new(SimExchangeConfig { taker_latency_ms: 0, taker_fee_bps: 0.0, maker_rebate_bps: 0.0 });
    // resting BuyYes limit at 0.55; fills when yes_ask drops to <= 0.55
    ex.on_book(MarketId(0), &ev(0.60, 1000.0), &NoBook::default(), 1000);
    ex.submit(OrderIntent { id: OrderId(1), market: MarketId(0), side: Side::BuyYes, shares: 50.0,
        max_depth: 1, limit_price: Some(0.55), tag: "m", kind: IntentKind::Entry }, 1000);
    assert!(ex.poll_fills(1000).is_empty());           // ask 0.60 > 0.55, no fill
    ex.on_book(MarketId(0), &ev(0.54, 1000.0), &NoBook::default(), 2000); // ask crosses
    let fills = ex.poll_fills(2000);
    assert_eq!(fills.len(), 1);
    assert!((fills[0].shares - 50.0).abs() < 1e-6);
}

#[test]
fn maker_fills_on_real_trade_tape_cross_with_queue_priority() {
    use pm_types::TradeTick;
    let mut ex = SimExchange::new(SimExchangeConfig { taker_latency_ms: 0, taker_fee_bps: 0.0, maker_rebate_bps: 0.0 });
    ex.on_book(MarketId(0), &ev(0.60, 1000.0), &NoBook::default(), 1000);
    ex.submit(OrderIntent { id: OrderId(1), market: MarketId(0), side: Side::BuyYes, shares: 50.0,
        max_depth: 1, limit_price: Some(0.55), tag: "m", kind: IntentKind::Entry }, 1000);
    // a real aggressor trade at 0.55 AFTER submit_ts crosses the resting buy
    ex.on_trade(MarketId(0), &TradeTick { ts_ns: 1500, price: 0.55, size: 50.0, aggressor_buy: false }, 1500);
    let fills = ex.poll_fills(1500);
    assert_eq!(fills.len(), 1);
}
```
(Adjust `TradeTick` field names to the real `pm_types::TradeTick` definition — read `crates/pm-types/src/trade.rs`.)

- [ ] **Step 2–4: fail → implement → pass**

Run: `cargo test -p pm-engine sim_exchange` (expected: 6 passed). Implement resting-order storage and two fill triggers: **book-cross** (port `check_resting_fills`, `runner.rs:2020`: BuyYes fills when `yes_ask <= limit`, etc.) and **trade-tape** (port `check_trade_driven_resting_fills`, `runner.rs:1948`: an aggressor print crossing the level with `trade.ts_ns > submit_ts` fills, respecting queue priority). Credit `maker_rebate_bps`. `on_trade` records the print and may realize a resting fill.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-engine/src/sim_exchange.rs
git commit -m "pm-engine: SimExchange maker fills (book-cross + real trade-tape, queue priority)"
```

---

## Task 5: CtxEnricher — regime scores + prior-range

**Files:** Create `crates/pm-engine/src/enrich.rs`; wire `lib.rs`.

- [ ] **Step 1: Write the failing test (synthetic SpotHistory)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{SpotHistory, SpotTick}; // adjust field names to real defs

    #[test]
    fn regime_scores_are_populated_and_live_safe() {
        // build a SpotHistory with a simple up-then-down path ending before ts=T
        let ticks: Vec<SpotTick> = (0..60).map(|i| SpotTick { ts_ns: i*1_000_000_000, price: 100.0 + (i as f64).sin(), quantity: 1.0, is_buyer_maker: false }).collect();
        let spot = SpotHistory::new(ticks);
        let enr = CtxEnricher::new_without_model();
        let mut ctx = pm_strategy::Ctx::default();
        enr.fill_regime(&mut ctx, 60_000_000_000, &spot);
        // scores are within valid ranges (not all zero on a non-trivial path)
        assert!(ctx.regime_realized_vol_180s_bps >= 0.0);
        assert!(ctx.regime_path_efficiency >= 0.0 && ctx.regime_path_efficiency <= 1.0);
    }
}
```
(Adjust `SpotTick`/`SpotHistory::new` to the real signatures — read `crates/pm-types/src/spot.rs`.)

- [ ] **Step 2–4: fail → implement → pass**

Run: `cargo test -p pm-engine enrich` . Implement `CtxEnricher` with `fill_regime(&self, ctx: &mut Ctx, ts_ns: i64, spot: &SpotHistory)` that **calls `WhipsawRiskSnapshot::from_history(ts_ns, spot)` (`pm-strategy/src/regime.rs:64`)** and assigns the five `regime_*` fields exactly as `runner.rs:772-776` does. Add `fill_prior_range(&self, ctx: &mut Ctx, ranges: PriorRanges)` taking precomputed 1d/3d/7d means (the driver computes these via `prior_market_range_mean`, `walkforward.rs:1614`, and passes them in — keep the enricher pure). `new_without_model()` is a constructor used until Task 6 adds the model.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-engine/src/enrich.rs crates/pm-engine/src/lib.rs
git commit -m "pm-engine: CtxEnricher regime scores + prior-range (ports WhipsawRiskSnapshot)"
```

---

## Task 6: CtxEnricher — model evaluation + engine wiring

**Files:** Modify `crates/pm-engine/src/enrich.rs`, `crates/pm-engine/src/engine.rs`, `crates/pm-engine/src/host.rs`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn model_eval_populates_model_output_from_snapshot() {
    // load the frozen champion snapshot and confirm model_output is Some after eval
    let enr = CtxEnricher::with_model_snapshot("data/snap062901.json").expect("load");
    let spot = /* small SpotHistory as in Task 5 */;
    let e = /* a ReplayEvent */;
    let mut ctx = pm_strategy::Ctx::default();
    enr.fill_model(&mut ctx, &e, 60_000_000_000, /*secs_since_open*/ 30, &spot);
    assert!(ctx.model_output.is_some());
}
```
(Path is relative to the crate; if the test can't see `data/`, copy a tiny snapshot fixture into `crates/pm-engine/tests/fixtures/` and load that.)

- [ ] **Step 2–4: fail → implement → pass**

Implement `with_model_snapshot(path)` that loads `OnlineMetaCalibratorSnapshot` (**port `read_meta_snapshot`, `walkforward.rs:3440-3449`**) into a `ModelState`, and `fill_model(&self, ctx, event, ts_ns, secs_since_open, spot)` that **calls `ModelState::evaluate_detailed_with_market_context(...)` exactly as `runner.rs:730-747`** and assigns `ctx.model_output`/`ctx.model_attribution`. Then wire the engine: `Engine` holds an `Option<CtxEnricher>` + `&SpotHistory`/`&TradeHistory` handles; in the decision path (around `build_ctx`), if an enricher is present, call `fill_regime` + `fill_model` + `fill_prior_range` on the `Ctx` before `on_event_scored`. Keep `build_ctx` (Phase 1) for the portfolio/exposure fields; the enricher fills the rest. Phase-1 mock tests pass `None` (no enricher) and stay green.

- [ ] **Step 5: Verify + commit**

Run: `cargo test -p pm-engine --features testkit` (Phase-1 tests green + new enrich tests).
```bash
git add crates/pm-engine/src/enrich.rs crates/pm-engine/src/engine.rs crates/pm-engine/src/host.rs
git commit -m "pm-engine: CtxEnricher model eval + engine wiring (Ctx fully populated)"
```

---

## Task 7: pm-app — leg-pairing + recorded EngineEvent stream

**Files:** Create `crates/pm-app/src/engine_driver.rs` (loading half); modify `crates/pm-app/src/main.rs` for module wiring.

- [ ] **Step 1: Write the failing test (paired legs from two synthetic event vecs)**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // Given a YES-leg ReplayEvent vec and a NO-leg ReplayEvent vec for one market,
    // pair_legs should produce one ts-ordered Vec<EngineEvent::Market{replay, no_book}>
    // where each event carries the YES book and the most-recent NO ladder at/<= its ts.
    #[test]
    fn pair_legs_forward_fills_counter_leg_by_ts() {
        let yes = vec![/* ReplayEvent ts=1000 ask0=0.60, ts=3000 ask0=0.62 */];
        let no  = vec![/* ReplayEvent ts=2000 ask0=0.42 */];
        let evs = pair_legs(MarketId(0), &yes, &no);
        // event at ts=1000 has NoBook empty (no NO snapshot yet); event at ts=3000 carries NO ask 0.42
        assert_eq!(evs.len(), /* yes.len()+no.len() merged */ 3);
        // assert the ts=3000 event's no_book.asks[0].price == 0.42
    }
}
```

- [ ] **Step 2–4: fail → implement → pass**

Implement `pair_legs(market: MarketId, yes: &[ReplayEvent], no: &[ReplayEvent]) -> Vec<EngineEvent>`: merge both legs by `ts_ns`; carry the latest YES book and latest NO ladder seen so far; emit an `EngineEvent::Market{replay, no_book}` on each update (replay = latest YES `ReplayEvent`; `no_book` = latest NO ladder as a `NoBook`). Then add `load_market_paired(handle_yes, handle_no) -> Vec<EngineEvent>` that loads each leg's parquet via `load_book_snapshot_async` (`pm-telonex-loader`) and calls `pair_legs`. Discover the YES/NO `MarketHandle` pair using the `outcome` label (**`outcome_label_resolved_yes`, `walkforward.rs:1590`**). Also expose a `RecordedFeed` (a `Feed` impl wrapping a `Vec<EngineEvent>` cursor) — or reuse a `pm-engine` `SliceFeed` if you add one; `next()` pops in order. Wire trade ticks: extend the Feed/driver so the engine's `on_trade` receives the PM trade tape (load via `load_pm_trades_async`); simplest is for the driver to hand the `SimExchange` the `TradeHistory` up front and have it self-serve by ts, OR emit trades through the engine — choose the cleaner path and note it.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-app/src/engine_driver.rs crates/pm-app/src/main.rs
git commit -m "pm-app: leg-pairing + recorded EngineEvent stream for the engine driver"
```

---

## Task 8: pm-app — thin engine driver + BTC-5m validation

**Files:** Modify `crates/pm-app/src/engine_driver.rs`, `crates/pm-app/src/main.rs`.

- [ ] **Step 1: Wire the driver behind `--engine`**

Add a `run_engine_backtest(cfg)` that: discovers BTC-5m markets + pairs legs (Task 7); loads `SpotHistory`/`TradeHistory` (`walkforward.rs:2970,4355`); builds the `Engine` with `BonereaperV2` + `SimExchange` (latency 500ms, real fees) + a sim `Clock` + a `CtxEnricher::with_model_snapshot("data/snap062901.json")`; computes prior-range per market via `prior_market_range_mean`; runs the engine over the merged stream; collects per-market + portfolio P&L and the order/fill trace. Route `--engine` in `main.rs` to this instead of the old walk-forward (keep the old path as the default so both are runnable for the delta).

- [ ] **Step 2: Determinism gate (golden trace on real data)**

Add an integration test `tests/engine_btc5m_determinism.rs` that runs `run_engine_backtest` twice over a SMALL fixed slice of the real BTC-5m cache (e.g. one day, a handful of markets) and asserts the `engine.trace` is byte-identical across runs.
Run: `cargo test -p pm-app --features testkit engine_btc5m_determinism`
Expected: PASS (identical traces). If the data slice is unavailable, the test should `eprintln!` skip-with-reason and not silently pass — gate it on a `PM_ENGINE_BTC5M_FIXTURE` env path.

- [ ] **Step 3: Plausibility tripwire + both-book delta (manual/reported)**

Run the full BTC-5m May backtest both ways and record the numbers:
```bash
# old synthetic-NO path (existing champion command, summary P&L)
bash /tmp/br2_may_run.sh   # or the champion command in configs/bonereaper_v2_favourite_062901.command.txt
# new engine path
cargo run -p pm-app --release -- --engine <same flags: --local-cache-dir data/cache --markets <btc5m manifest> --meta-calibrator-snapshot-in data/snap062901.json --start-date ... --end-date ...>
```
Record: (a) the new engine trades roughly the reference count (~93 markets of ~3561) with a sane equity path and no panic — the plausibility tripwire; (b) the P&L delta new-vs-old as the measured both-book correction. Write both numbers into the run notes / scratchpad. This is a reported result, not an asserted test.

- [ ] **Step 4: Commit**

```bash
git add crates/pm-app/src/engine_driver.rs crates/pm-app/src/main.rs crates/pm-app/tests/engine_btc5m_determinism.rs
git commit -m "pm-app: thin engine backtest driver (--engine) + BTC-5m determinism gate"
```

---

## Phase 2 done — verification gate

- `cargo test -p pm-engine --features testkit` — green (Phase-1 + sim_exchange + enrich tests).
- `cargo test -p pm-app --features testkit engine_btc5m_determinism` — green (identical traces on the real slice).
- `cargo clippy -p pm-engine -p pm-app --all-targets --features testkit --no-deps -- -D warnings` — pm-engine/pm-app code clean (dependency lints out of scope).
- Recorded: the plausibility tripwire result + the both-book P&L delta vs the old synthetic-NO May run.

Deliverable: br2 runs through the shared engine on real BTC-5m data with a faithful both-book fill model, deterministically, with the both-book correction measured. This unblocks the **br2 re-formulation** cycle and the **multi-cell turn-on**.

## Follow-on (separate plans)

- **Multi-cell turn-on:** k-way-merge the recorded Feed across all 12 cells + the real `(token, window)` classifier (from market metadata, overlapping-window bucketing) + cross-market concurrency on shared capital. Mostly Feed/driver changes; the engine is unchanged.
- **br2 re-formulation:** sweep config / re-tune the edge gate + sizing / re-train the meta-calibrator against the faithful backtests (the in-driver training pass).
- **Phase 3 (live driver):** ws Feed + CLOB Exchange + wall Clock + the live `CtxEnricher` (rolling `SpotHistory`) + driver-equivalence test.
