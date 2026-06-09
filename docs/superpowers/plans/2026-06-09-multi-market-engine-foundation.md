# Multi-Market Engine Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `pm-engine` host a fresh strategy instance per market and release a market's exposure on settlement, so any stateful single-market strategy runs correctly across a many-market interleaved stream.

**Architecture:** The engine currently holds one shared `strategy: S` and applies exposure that only ever grows. A stateful strategy's per-market counters (e.g. br2's `late_fires`) exhaust globally, so trading stops after a few markets, and the correlated-exposure cap is unusable. This plan changes the engine to clone a strategy template per market (isolated state, dropped on settle) and to release each market's exposure contribution on settle. This is Plan 1 of the multi-market strategy redesign (spec: `docs/superpowers/specs/2026-06-09-multi-market-strategy-design.md`); it is data-independent and unblocks Plans 2–4.

**Tech Stack:** Rust, `pm-engine` crate (sync single-threaded engine), `cargo test`/`clippy`. Existing seams: `Strategy` (pm-strategy), `Engine<S>` (engine.rs), `ExposureState` (exposure.rs).

**Pre-flight:** There are uncommitted diagnostic edits in `crates/pm-engine/src/engine.rs` (risk-reject tracing) and `crates/pm-app/src/engine_driver.rs` (trace-tag tally) from the bug hunt. Commit them first as `chore: risk-reject trace diagnostics` (they are useful and harmless) so this plan starts from a clean tree:
```bash
git add crates/pm-engine/src/engine.rs crates/pm-app/src/engine_driver.rs
git commit -m "chore: risk-reject trace diagnostics in engine + driver tally"
```

---

### Task 1: BonereaperV2 derives Clone

The engine will clone a strategy template per market, so the hosted strategy must be `Clone`. `BonereaperV2` and its field types are not currently `Clone`.

**Files:**
- Modify: `crates/pm-strategy/src/bonereaper_v2.rs` (derive on `BonereaperV2`, `BonereaperV2GateStats`)
- Modify: `crates/pm-strategy/src/signals.rs` (derive on `Ring` if missing)
- Test: `crates/pm-strategy/src/bonereaper_v2.rs` (inline `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` in `bonereaper_v2.rs`:

```rust
#[test]
fn bonereaper_v2_is_clonable_with_independent_state() {
    let mut a = BonereaperV2::new(BonereaperV2Config::default());
    a.late_fires = 2;
    let b = a.clone();
    a.late_fires = 99;
    // Clone is a deep copy: mutating the original must not change the clone.
    assert_eq!(b.late_fires, 2, "clone must have independent late_fires");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p pm-strategy bonereaper_v2_is_clonable -v`
Expected: FAIL to compile — `BonereaperV2: Clone` not satisfied (`.clone()` unresolved).

- [ ] **Step 3: Add the derives**

In `signals.rs`, ensure `Ring` derives `Clone` (add `Clone` to its `#[derive(...)]`; it holds a `Vec`/buffer + indices, all `Clone`).
In `bonereaper_v2.rs`, add `#[derive(Clone)]` to `BonereaperV2GateStats` (if not already) and to `BonereaperV2` (above `pub struct BonereaperV2 {`). All fields are `Clone`: `BonereaperV2Config` (serde struct, add `Clone` to its derive if missing), `Ring`, `Option<Side>` (Side is `Copy`), numeric/bool primitives, `BonereaperV2GateStats`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p pm-strategy bonereaper_v2_is_clonable -v`
Expected: PASS.

- [ ] **Step 5: Verify nothing else broke**

Run: `cargo test -p pm-strategy 2>&1 | tail -3`
Expected: all pm-strategy tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/pm-strategy/src/bonereaper_v2.rs crates/pm-strategy/src/signals.rs
git commit -m "pm-strategy: derive Clone on BonereaperV2 (+ Ring, gate stats) for per-market hosting"
```

---

### Task 2: Engine hosts a strategy template, cloned per market

Replace the single shared `strategy: S` with a `strategy_template: S` plus a per-market `HashMap<MarketId, S>`. Each market's first event clones the template; `on_event_scored` and `on_market_resolved` route to that market's instance; the instance is dropped on settlement.

**Files:**
- Modify: `crates/pm-engine/src/engine.rs` (struct, `new`, `on_market`, settle path)
- Test: `crates/pm-engine/tests/engine_integration.rs`

- [ ] **Step 1: Write the failing test (per-market budgets are not shared)**

Add to `engine_integration.rs`. This strategy fires at most twice *per instance*; with per-market hosting, two markets must yield two fires *each* (4 total), not 2 total.

```rust
/// Fires at most twice, tracked in per-instance state. Used to prove the engine
/// gives each market its own strategy instance (budgets are not shared).
#[derive(Clone, Default)]
struct FireTwice {
    fires: u32,
}
impl Strategy for FireTwice {
    fn on_event(
        &mut self,
        _e: &ReplayEvent,
        _c: &Ctx,
        _s: &SpotHistory,
        _t: &TradeHistory,
    ) -> StrategyOutput {
        if self.fires >= 2 {
            return StrategyOutput::hold();
        }
        self.fires += 1;
        StrategyOutput::one(OrderRequest {
            side: Side::BuyYes,
            shares: 1.0,
            max_depth: 1,
            limit_price: None,
            tag: "fire",
        })
    }
}

#[test]
fn each_market_gets_its_own_strategy_instance() {
    let m1 = MarketId(0);
    let m2 = MarketId(1);
    let clock_cell = Rc::new(Cell::new(0i64));
    // 3 book events per market (so FireTwice could fire up to twice each) then close.
    let mut feed = ScriptedFeed::new(
        vec![
            ev(10, m1, false), ev(11, m1, false), ev(12, m1, false),
            ev(20, m2, false), ev(21, m2, false), ev(22, m2, false),
            ev(30, m1, true), ev(31, m2, true),
        ],
        clock_cell.clone(),
    );
    let mut ex = InstantExchange::new(0.50, 0.0);
    let clock = SimClock { ts: clock_cell };
    let mut engine = Engine::new(
        FireTwice::default(),
        Portfolio::new(1_000_000.0),
        RiskGate { limits: limits() },
        |_m| (Token::Btc, 0),
    );
    engine.run(&mut feed, &mut ex, &clock);
    // 2 fires per market * 2 markets = 4 submits. A single shared instance caps at 2.
    assert_eq!(ex.submitted.len(), 4, "expected 2 fires per market (4 total)");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p pm-engine --features testkit each_market_gets_its_own -v`
Expected: FAIL — `assert_eq` gets 2 (shared instance), not 4. (Compiles: `Engine::new` still takes an instance.)

- [ ] **Step 3: Add the `Clone` bound and per-market storage to the struct**

In `engine.rs`, change the impl/struct bound to `S: Strategy + Clone` and the strategy field:

```rust
pub struct Engine<S: Strategy + Clone> {
    /// Template cloned to create each market's isolated strategy instance.
    strategy_template: S,
    /// Per-market strategy instances (isolated mutable state). Dropped on settle.
    strategies: HashMap<MarketId, S>,
    pub portfolio: Portfolio,
    // ... (all other fields unchanged)
}
```

Update the `impl<S: Strategy> Engine<S>` line to `impl<S: Strategy + Clone> Engine<S>`.

- [ ] **Step 4: Update `Engine::new` to store the template**

```rust
pub fn new(
    strategy: S,
    portfolio: Portfolio,
    risk: RiskGate,
    classify: fn(MarketId) -> (Token, i64),
) -> Self {
    Self {
        strategy_template: strategy,
        strategies: HashMap::new(),
        portfolio,
        // ... rest unchanged (exposure, risk, markets, market_meta, next_order_id,
        // spot, trades, marks, classify, enricher, prior_ranges,
        // prior_ranges_by_market, trades_by_market, trace)
    }
}
```

- [ ] **Step 5: Route `on_market_resolved` to the market's instance on settle, then drop it**

In `on_market`, replace the `MARKET_CLOSE` block's `self.strategy.on_market_resolved(...)` with the per-market instance, and remove the instance afterward:

```rust
if e.flags.contains(ReplayFlags::MARKET_CLOSE) {
    let resolved_yes = meta.and_then(|m| m.resolved_yes).unwrap_or(e.yes_mid >= 0.5);
    self.portfolio.settle(e.market_id, resolved_yes);
    if let Some(mut strat) = self.strategies.remove(&e.market_id) {
        strat.on_market_resolved(e.yes_mid, resolved_yes);
    }
    return;
}
```

- [ ] **Step 6: Route `on_event_scored` to the market's instance**

Replace the `let (out, _model) = self.strategy.on_event_scored(...)` call. Because `self` is borrowed for `&self.spot` etc., get-or-create the instance first, then call it (the instance borrow is disjoint from `spot`/`trades`/`prior` which are read before). Use a contains/insert pattern to avoid a closure borrow of `self`:

```rust
let trades = self.trades_by_market.get(&e.market_id).unwrap_or(&self.trades);
if !self.strategies.contains_key(&e.market_id) {
    let fresh = self.strategy_template.clone();
    self.strategies.insert(e.market_id, fresh);
}
let strat = self.strategies.get_mut(&e.market_id).expect("just inserted");
let (out, _model) = strat.on_event_scored(e, &ctx, &self.spot, trades);
```

Note: `trades` (an `&` into `self`) and `&self.spot` are immutable borrows; `strat` is `&mut self.strategies` — disjoint fields, so this compiles. If the borrow checker objects to `trades` overlapping, bind `let spot = &self.spot;` and the trades slice before the `get_mut`.

- [ ] **Step 7: Run the new test + the full suite**

Run: `cargo test -p pm-engine --features testkit 2>&1 | tail -6`
Expected: `each_market_gets_its_own_strategy_instance` PASSES (4 submits); `golden_trace_is_deterministic`, `interleaves_two_markets_in_ts_order_sharing_capital`, `engine_buys_once_then_settles_yes`, `hosts_real_br2_deterministically` still PASS (per-market instances are behavior-identical for these: 1-market and position-gated strategies are unaffected).

- [ ] **Step 8: Fix any caller signatures**

`cargo build -p pm-engine --features testkit` and `cargo build -p pm-app`. The `S: Clone` bound now requires every `Engine::new` caller's strategy to be `Clone`. `BonereaperV2` is (Task 1); the test strategies need `#[derive(Clone)]` (add to `BuyOnce`, `BuyOnceEach` in `engine_integration.rs` — `BuyOnceEach` already derives `Default`; add `Clone`). Fix until both build clean.

- [ ] **Step 9: Commit**

```bash
git add crates/pm-engine/src/engine.rs crates/pm-engine/tests/engine_integration.rs
git commit -m "pm-engine: per-market strategy instances (clone template per market, drop on settle)"
```

---

### Task 3: Release a market's exposure on settlement

Today `ExposureState` only grows: every fill adds to the `(token,window)` key, and settle never subtracts. Track each market's cumulative signed exposure and release it on settle so the key returns toward zero.

**Files:**
- Modify: `crates/pm-engine/src/engine.rs` (track per-market exposure; release on settle)
- Test: `crates/pm-engine/tests/engine_integration.rs`

- [ ] **Step 1: Write the failing test**

```rust
use pm_engine::exposure::ExposureKey;

#[test]
fn exposure_is_released_on_settlement() {
    let m = MarketId(0);
    let clock_cell = Rc::new(Cell::new(0i64));
    let mut feed = ScriptedFeed::new(
        vec![ev(10, m, false), ev(30, m, true)],
        clock_cell.clone(),
    );
    let mut ex = InstantExchange::new(0.50, 0.0);
    let clock = SimClock { ts: clock_cell };
    let mut engine = Engine::new(
        BuyOnce { fired: false },
        Portfolio::new(1_000.0),
        RiskGate { limits: limits() },
        |_m| (Token::Btc, 0),
    );
    engine.run(&mut feed, &mut ex, &clock);
    // BuyOnce bought 100 YES (+100 signed). After the market settles, the
    // (Btc,0) exposure must be released back to ~0, not stuck at +100.
    let net = engine.exposure.net(ExposureKey { token: Token::Btc, window: 0 });
    assert!(net.abs() < 1e-9, "exposure must be released on settle, got {net}");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p pm-engine --features testkit exposure_is_released_on_settlement -v`
Expected: FAIL — `net` is +100 (never released).

- [ ] **Step 3: Track per-market cumulative signed exposure**

In `engine.rs`, add a field to the struct:

```rust
    /// Cumulative signed exposure contributed by each market, so it can be
    /// released on settlement (ExposureState aggregates by (token,window) only).
    market_signed_exposure: HashMap<MarketId, f64>,
```

Initialize it `HashMap::new()` in `new`. In the `run` fill loop, accumulate alongside the existing `exposure.apply`:

```rust
for fill in ex.poll_fills(clock.now()) {
    self.trace.push((fill.ts, "fill", fill.market, fill.shares));
    self.portfolio.apply_fill(&fill);
    let (token, window) = (self.classify)(fill.market);
    let signed = signed_shares(fill.side, fill.shares);
    self.exposure.apply(ExposureKey { token, window }, signed);
    *self.market_signed_exposure.entry(fill.market).or_insert(0.0) += signed;
}
```

- [ ] **Step 4: Release on settle**

In the `MARKET_CLOSE` block (from Task 2 Step 5), before/after settle, release the accumulated exposure for that market:

```rust
if e.flags.contains(ReplayFlags::MARKET_CLOSE) {
    let resolved_yes = meta.and_then(|m| m.resolved_yes).unwrap_or(e.yes_mid >= 0.5);
    let (token, window) = (self.classify)(e.market_id);
    if let Some(net) = self.market_signed_exposure.remove(&e.market_id) {
        self.exposure.apply(ExposureKey { token, window }, -net);
    }
    self.portfolio.settle(e.market_id, resolved_yes);
    if let Some(mut strat) = self.strategies.remove(&e.market_id) {
        strat.on_market_resolved(e.yes_mid, resolved_yes);
    }
    return;
}
```

(Note: `(token, window)` is already computed at the top of `on_market`; reuse that binding rather than recomputing if it is in scope.)

- [ ] **Step 5: Run the test + full suite**

Run: `cargo test -p pm-engine --features testkit 2>&1 | tail -6`
Expected: `exposure_is_released_on_settlement` PASSES; all prior tests still PASS (releasing exposure on settle does not change single-market or position-gated outcomes; the correlated cap is off by default).

- [ ] **Step 6: Commit**

```bash
git add crates/pm-engine/src/engine.rs crates/pm-engine/tests/engine_integration.rs
git commit -m "pm-engine: release per-market exposure on settlement (correlated cap usable cross-market)"
```

---

### Task 4: Clippy clean + driver still builds

**Files:**
- Possibly modify: `crates/pm-app/src/engine_driver.rs` (only if the `Engine::new` `S: Clone` bound surfaces an error — `BonereaperV2` is now `Clone`, so it should build unchanged)

- [ ] **Step 1: Clippy the engine**

Run: `cargo clippy -p pm-engine --all-targets --features testkit --no-deps -- -D warnings`
Expected: exit 0, no warnings.

- [ ] **Step 2: Build the app (driver uses the engine with BonereaperV2)**

Run: `cargo build -p pm-app 2>&1 | tail -3`
Expected: builds (0 errors). If the `S: Clone` bound errors in `run_engine_backtest`, confirm `BonereaperV2: Clone` (Task 1) — no driver change should be needed.

- [ ] **Step 3: Commit (only if engine_driver.rs needed changes)**

```bash
git add crates/pm-app/src/engine_driver.rs
git commit -m "pm-app: satisfy engine S: Clone bound in the backtest driver"
```

---

### Task 5: Validate the "stops after 3 markets" bug is gone (reported)

This is the headline outcome. Requires the BTC-5m both-legs May cache locally (May 21–28); if absent, `eprintln` skip-with-reason rather than fail.

**Files:** none (manual/reported validation using the existing `engine-backtest` subcommand)

- [ ] **Step 1: Run a 2-day BTC-5m slice and read the trace tally**

Run:
```bash
RUST_LOG=error ./target/release/pm-app engine-backtest \
  --local-cache-dir data/cache --start-date 2026-05-21 --end-date 2026-05-22 \
  --meta-calibrator-snapshot-in data/snap062901.json 2>&1 | grep -E "engine-diag|engine backtest:"
```
(Build release first: `cargo build --release -p pm-app`.)

- [ ] **Step 2: Confirm trading no longer halts after ~3 markets**

Expected: `markets_traded` and `submit`/`fill` counts now scale roughly with the number of days/markets (many more than the previous fixed 3 markets / 16 submits / 38 fills). Record the new numbers in the run notes — this is a reported result, not an asserted test (the absolute P&L is single-cell noise per the spec and is NOT a strategy verdict). If the cache is absent, note "skipped: BTC-5m May cache not local."

---

## Self-Review

**Spec coverage (against `2026-06-09-multi-market-strategy-design.md` §6 "Required engine fixes"):**
- §6.1 per-market strategy state → Tasks 1–2. ✓
- §6.2 release exposure on settlement → Task 3. ✓
- The cell-keyed factory (§5.4) is intentionally deferred to Plan 3 (cells); Plan 1 uses clone-from-template, which is the minimal change that fixes the bug. ✓ (documented above)

**Placeholder scan:** No TBD/TODO; every code step shows the code; the only "reported, not asserted" step (Task 5) is deliberate per the spec's single-cell-is-noise principle, with a concrete command and expected signal. ✓

**Type consistency:** `strategy_template`/`strategies` (Task 2), `market_signed_exposure` (Task 3), `ExposureKey { token, window }` and `signed_shares` (existing) used consistently. `Engine<S: Strategy + Clone>` bound applied to both the struct and impl. `on_market_resolved(e.yes_mid, resolved_yes)` matches the existing `Strategy` trait signature. ✓

---

## Follow-on plans (separate specs→plans cycles)
- **Plan 2 — Strategy core:** `Signal` (lift the model), `PositionManager` (convex-book accumulator), `ExecutionPolicy` (maker/taker/adaptive), composed `MarketStrategy`; built and tested on synthetic fixtures. Hosted via Task 2's per-market mechanism.
- **Plan 3 — Cells + pool:** real `(token,window)` classifier (replaces hardcoded `(Btc,0)`), per-cell config selection (generalize the template to a cell-keyed factory), shared-pool edge-ranked sizing + per-book cap + settle-driven recycling.
- **Plan 4 — Backtest wiring + validation:** multi-cell recorded Feed, portfolio-Sharpe validation vs the champion baseline, per-cell posture report. Gated on the data foundation (binance SOL/XRP + June, per-cell calibration).
</content>
</invoke>
