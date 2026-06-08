# pm-engine core (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the synchronous, single-threaded `pm-engine` core crate that hosts br2 unchanged and runs the one decision→risk→submit→fill→portfolio cycle over a multi-market, timestamp-ordered event stream, driven by pluggable `Feed`/`Exchange`/`Clock` seams.

**Architecture:** A new `pm-engine` crate in the `polymarket-backtest` workspace defines the `EngineEvent` currency, the three seam traits, and the engine loop. It hosts `pm-strategy::BonereaperV2` through a single `Ctx`-builder, gates orders through a `RiskGate` (ported from the live `RiskEngine` plus an incremental correlated-exposure cap), and accounts fills in a shared `Portfolio` (ported from the live `InventoryState`). Phase 1 proves the core with in-memory mock seams; the real sim Exchange (Phase 2) and live driver (Phase 3) come in follow-on plans.

**Tech Stack:** Rust (edition 2021, workspace resolver 3, rustc 1.95), Cargo workspaces, `cargo test`, path-dependency crates `pm-types`/`pm-strategy`/`pm-risk`/`pm-model`.

---

## Prerequisite & sequencing

- **Phase 0 (clean-slate cleanup) lands first** per the design decision: it nukes BTE/router/native-MM in `polymarket-exec`, leaving br2 the sole live strategy. Phase 1 here is **additive** (a brand-new crate) and only *references* types that clean-slate does not touch (`pm-strategy`, `pm-risk`, and the live `RiskEngine`/`InventoryState` whose *logic* it ports). It can therefore be built in parallel with, or immediately after, clean-slate; the strict ordering matters for the *driver* rewrites in Phases 2–3, not for this core.
- **Follow-on plans (outlined at the end):** Phase 2 (sim Exchange + recorded Feed + backtest driver + golden-trace/champion tests) and Phase 3 (live driver + WS recorder + driver-equivalence test). Their tasks reference the concrete APIs produced here, so they are written after Phase 1 lands.

## File structure (Phase 1)

- Create: `crates/pm-engine/Cargo.toml`
- Create: `crates/pm-engine/src/lib.rs` — module wiring + re-exports
- Create: `crates/pm-engine/src/event.rs` — `Ts`, `EngineEvent`, `NoBook`, `Token`
- Create: `crates/pm-engine/src/seams.rs` — `Feed`/`Exchange`/`Clock` traits + order/fill/ack types
- Create: `crates/pm-engine/src/testkit.rs` — in-memory mock `Feed`/`Exchange`/`Clock` (compiled under `#[cfg(any(test, feature = "testkit"))]`)
- Create: `crates/pm-engine/src/exposure.rs` — `ExposureKey`, `ExposureState` (incremental cap)
- Create: `crates/pm-engine/src/risk.rs` — `RiskGate`, `RiskLimits`, `RiskDecision`
- Create: `crates/pm-engine/src/portfolio.rs` — `Portfolio`, `Position` (shared capital/position state)
- Create: `crates/pm-engine/src/host.rs` — `StrategyHost`: `build_ctx` + ReplayEvent projection + strategy drive
- Create: `crates/pm-engine/src/engine.rs` — `Engine`: the loop + per-market routing + settlement
- Create: `crates/pm-engine/tests/engine_integration.rs` — end-to-end mock-driven tests
- Modify: `crates/pm-types/src/tape.rs` — add the NO-book carrier (see Task 2)
- Modify: `Cargo.toml` (workspace root) — add `pm-engine` to `[workspace] members` and `[workspace.dependencies]`

---

## Task 1: Scaffold the `pm-engine` crate

**Files:**
- Create: `crates/pm-engine/Cargo.toml`
- Create: `crates/pm-engine/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create the crate manifest**

`crates/pm-engine/Cargo.toml`:
```toml
[package]
name = "pm-engine"
version = "0.1.0"
edition = "2021"

[features]
# Exposes the in-memory mock seams to downstream crates (drivers' tests).
testkit = []

[dependencies]
pm-types = { path = "../pm-types" }
pm-strategy = { path = "../pm-strategy" }
pm-risk = { path = "../pm-risk" }
pm-model = { path = "../pm-model" }

[dev-dependencies]
# Real strategy used in the host smoke test.
pm-strategy = { path = "../pm-strategy" }
```

- [ ] **Step 2: Create a minimal lib.rs so the crate compiles**

`crates/pm-engine/src/lib.rs`:
```rust
#![forbid(unsafe_code)]

pub mod event;
pub mod seams;
pub mod exposure;
pub mod risk;
pub mod portfolio;
pub mod host;
pub mod engine;

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
```

Create each referenced module as an empty file for now (`touch` equivalent: write `// placeholder` into each), so `cargo build` succeeds. Subsequent tasks fill them in.

- [ ] **Step 3: Register the crate in the workspace**

In the root `Cargo.toml`, add `"crates/pm-engine"` to `[workspace] members` and `pm-engine = { path = "crates/pm-engine" }` to `[workspace.dependencies]` (match the formatting of the existing `pm-strategy` line).

- [ ] **Step 4: Verify it builds**

Run: `cargo build -p pm-engine`
Expected: compiles with no errors (empty modules are fine).

- [ ] **Step 5: Commit**

```bash
git add crates/pm-engine Cargo.toml
git commit -m "pm-engine: scaffold crate + workspace registration"
```

---

## Task 2: Add the NO-book carrier to pm-types

The engine needs the real NO ladder for fills. `ReplayEvent` stays YES-centric (the strategy view); the NO book travels alongside it. Add a small fixed-depth NO ladder type mirroring the YES `bids`/`asks`.

**Files:**
- Modify: `crates/pm-types/src/tape.rs` (after the `ReplayEvent` definition, around `tape.rs:44`)
- Test: `crates/pm-types/src/tape.rs` (inline `#[cfg(test)]` module)

- [ ] **Step 1: Write the failing test**

Append to `crates/pm-types/src/tape.rs`:
```rust
#[cfg(test)]
mod no_book_tests {
    use super::*;

    #[test]
    fn no_book_defaults_to_empty_levels() {
        let nb = NoBook::default();
        assert_eq!(nb.bids[0], BookLevel::default());
        assert_eq!(nb.asks[0], BookLevel::default());
    }

    #[test]
    fn no_book_roundtrips_serde() {
        let mut nb = NoBook::default();
        nb.asks[0] = BookLevel { price: 0.42, size: 100.0 };
        let json = serde_json::to_string(&nb).unwrap();
        let back: NoBook = serde_json::from_str(&json).unwrap();
        assert_eq!(back.asks[0].price, 0.42);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p pm-types no_book`
Expected: FAIL — `cannot find type NoBook`.

- [ ] **Step 3: Add the type**

Insert after the `ReplayEvent` `const _` size assertion in `crates/pm-types/src/tape.rs`:
```rust
/// Real NO-side ladder, carried alongside a `ReplayEvent` for fill simulation.
/// Kept separate so the strategy-facing `ReplayEvent` stays YES-centric and the
/// sim Exchange prices the NO leg from real depth instead of `1 - yes`.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[repr(C)]
pub struct NoBook {
    pub bids: [BookLevel; TAPE_DEPTH],
    pub asks: [BookLevel; TAPE_DEPTH],
}
```
(`serde_json` is already a dev-dependency in `pm-types`; if not, add it under `[dev-dependencies]`.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p pm-types no_book`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/pm-types/src/tape.rs crates/pm-types/Cargo.toml
git commit -m "pm-types: add NoBook ladder carrier for both-book fills"
```

---

## Task 3: Define the engine event currency

**Files:**
- Create: `crates/pm-engine/src/event.rs`

- [ ] **Step 1: Write the failing test**

`crates/pm-engine/src/event.rs`:
```rust
use pm_types::{NoBook, ReplayEvent};

/// Single time currency: nanoseconds since the Unix epoch.
pub type Ts = i64;

/// Underlying asset of a market. Drives the correlated-exposure cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    Btc,
    Eth,
    Sol,
    Xrp,
}

/// The one event currency both drivers produce, in timestamp order.
///
/// `Market` wraps the existing `ReplayEvent` (which already carries `market_id`,
/// the YES book, `spot_price`, and event-kind `flags`) plus the real NO ladder.
/// The strategy sees only the `ReplayEvent`; the engine keeps `no_book` for fills.
#[derive(Debug, Clone, Copy)]
pub enum EngineEvent {
    Market { replay: ReplayEvent, no_book: NoBook },
}

impl EngineEvent {
    pub fn ts(&self) -> Ts {
        match self {
            EngineEvent::Market { replay, .. } => replay.ts_ns,
        }
    }
    pub fn market_id(&self) -> pm_types::MarketId {
        match self {
            EngineEvent::Market { replay, .. } => replay.market_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{MarketId, ReplayEvent};

    fn replay_at(ts: i64) -> ReplayEvent {
        let mut e = ReplayEvent {
            ts_ns: ts,
            market_id: MarketId::default(),
            yes_mid: 0.5, yes_bid: 0.49, yes_ask: 0.51, volume: 0.0,
            bids: Default::default(), asks: Default::default(),
            spot_price: 0.0, flags: Default::default(),
        };
        e.ts_ns = ts;
        e
    }

    #[test]
    fn engine_event_exposes_ts() {
        let ev = EngineEvent::Market { replay: replay_at(123), no_book: NoBook::default() };
        assert_eq!(ev.ts(), 123);
    }
}
```
(If `MarketId` has no `Default`, construct it via its real constructor — check `crates/pm-types/src/market.rs` and adjust `replay_at`. The agent reports show `MarketId` keys `HashMap`s, so it is `Copy + Eq + Hash`.)

- [ ] **Step 2: Run test to verify it fails, then passes**

Run: `cargo test -p pm-engine event::`
Expected: compiles and PASS once `event.rs` replaces its placeholder.

- [ ] **Step 3: Commit**

```bash
git add crates/pm-engine/src/event.rs
git commit -m "pm-engine: EngineEvent currency wrapping ReplayEvent + NoBook"
```

---

## Task 4: Define the seam traits and order/fill types

**Files:**
- Create: `crates/pm-engine/src/seams.rs`

- [ ] **Step 1: Write the types and traits**

`crates/pm-engine/src/seams.rs`:
```rust
use crate::event::{EngineEvent, Ts};
use pm_strategy::Side;
use pm_types::MarketId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OrderId(pub u64);

/// An order accepted by the risk gate, handed to the Exchange.
#[derive(Debug, Clone, Copy)]
pub struct OrderIntent {
    pub id: OrderId,
    pub market: MarketId,
    pub side: Side,
    pub shares: f64,
    pub max_depth: usize,
    /// `None` = taker (sweep opposing book); `Some(p)` = maker limit (YES terms).
    pub limit_price: Option<f32>,
    pub tag: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillLiquidity { Maker, Taker }

#[derive(Debug, Clone, Copy)]
pub struct FillReport {
    pub order: OrderId,
    pub market: MarketId,
    pub side: Side,
    pub shares: f64,
    /// Executed price in YES terms (NO fills are reported as their NO price).
    pub price: f32,
    pub fee_usd: f64,
    pub liquidity: FillLiquidity,
    pub ts: Ts,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SubmitAck { Accepted, Rejected }

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CancelAck { Cancelled, Unknown }

/// Produces the ordered market-event stream. Synchronous pull keeps the engine
/// deterministic; the live driver's impl blocks on a merged channel.
pub trait Feed {
    fn next(&mut self) -> Option<EngineEvent>;
}

/// Turns accepted orders into fills. sim: compute from book/tape; live: CLOB.
pub trait Exchange {
    fn submit(&mut self, order: OrderIntent, now: Ts) -> SubmitAck;
    fn cancel(&mut self, id: OrderId, now: Ts) -> CancelAck;
    /// Fills realized since the previous poll.
    fn poll_fills(&mut self, now: Ts) -> Vec<FillReport>;
}

/// Time source. sim: last event ts; live: wall clock.
pub trait Clock {
    fn now(&self) -> Ts;
}
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build -p pm-engine`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/pm-engine/src/seams.rs
git commit -m "pm-engine: Feed/Exchange/Clock seams + order/fill types"
```

---

## Task 5: In-memory testkit seams

**Files:**
- Create: `crates/pm-engine/src/testkit.rs`

- [ ] **Step 1: Write the mock seams**

`crates/pm-engine/src/testkit.rs`:
```rust
use crate::event::{EngineEvent, Ts};
use crate::seams::{CancelAck, Clock, Exchange, Feed, FillReport, OrderId, OrderIntent, SubmitAck};
use std::cell::Cell;
use std::collections::VecDeque;
use std::rc::Rc;

/// Feed that replays a fixed, pre-sorted vector of events.
pub struct ScriptedFeed {
    events: VecDeque<EngineEvent>,
    clock: Rc<Cell<Ts>>,
}

impl ScriptedFeed {
    pub fn new(mut events: Vec<EngineEvent>, clock: Rc<Cell<Ts>>) -> Self {
        events.sort_by_key(|e| e.ts());
        Self { events: events.into(), clock }
    }
}

impl Feed for ScriptedFeed {
    fn next(&mut self) -> Option<EngineEvent> {
        let ev = self.events.pop_front()?;
        self.clock.set(ev.ts()); // advance sim time to the event we hand out
        Some(ev)
    }
}

/// Clock backed by a shared cell the feed advances.
pub struct SimClock { pub ts: Rc<Cell<Ts>> }
impl Clock for SimClock { fn now(&self) -> Ts { self.ts.get() } }

/// Exchange that fills every submitted order instantly at a scripted price.
/// Phase 1 only needs deterministic fills to exercise the loop; the real
/// both-book fill model is Phase 2.
pub struct InstantExchange {
    next_fill: Vec<FillReport>,
    pub submitted: Vec<OrderIntent>,
    pub fill_price: f32,
    pub fee_usd: f64,
}

impl InstantExchange {
    pub fn new(fill_price: f32, fee_usd: f64) -> Self {
        Self { next_fill: Vec::new(), submitted: Vec::new(), fill_price, fee_usd }
    }
}

impl Exchange for InstantExchange {
    fn submit(&mut self, order: OrderIntent, now: Ts) -> SubmitAck {
        self.submitted.push(order);
        self.next_fill.push(FillReport {
            order: order.id, market: order.market, side: order.side,
            shares: order.shares, price: self.fill_price, fee_usd: self.fee_usd,
            liquidity: crate::seams::FillLiquidity::Taker, ts: now,
        });
        SubmitAck::Accepted
    }
    fn cancel(&mut self, _id: OrderId, _now: Ts) -> CancelAck { CancelAck::Unknown }
    fn poll_fills(&mut self, _now: Ts) -> Vec<FillReport> { std::mem::take(&mut self.next_fill) }
}
```

- [ ] **Step 2: Verify it builds under the test cfg**

Run: `cargo build -p pm-engine --features testkit`
Expected: compiles.

- [ ] **Step 3: Commit**

```bash
git add crates/pm-engine/src/testkit.rs
git commit -m "pm-engine: in-memory testkit seams (scripted feed, sim clock, instant exchange)"
```

---

## Task 6: ExposureState — incremental correlated-exposure cap

New code. The cap is the cross-market risk control. Net exposure per `(token, window)` is updated on every fill (never by iterating a map), so it is deterministic.

**Files:**
- Create: `crates/pm-engine/src/exposure.rs`

- [ ] **Step 1: Write the failing test**

`crates/pm-engine/src/exposure.rs`:
```rust
use crate::event::Token;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExposureKey {
    pub token: Token,
    /// Groups markets whose open windows overlap. v1: the driver supplies a
    /// bucket id (e.g. the shared close-window). Finer bucketing is a config refinement.
    pub window: i64,
}

#[derive(Debug, Default)]
pub struct ExposureState {
    net_shares: HashMap<ExposureKey, f64>,
}

impl ExposureState {
    /// Signed share delta applied on each fill (+ for net-long YES, - for net-short).
    pub fn apply(&mut self, key: ExposureKey, signed_shares_delta: f64) {
        *self.net_shares.entry(key).or_default() += signed_shares_delta;
    }
    pub fn net(&self, key: ExposureKey) -> f64 {
        self.net_shares.get(&key).copied().unwrap_or(0.0)
    }
    /// Would adding `signed_shares_delta` to `key` exceed `cap_abs_shares`?
    pub fn would_exceed(&self, key: ExposureKey, signed_shares_delta: f64, cap_abs_shares: f64) -> bool {
        (self.net(key) + signed_shares_delta).abs() > cap_abs_shares
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> ExposureKey { ExposureKey { token: Token::Btc, window: 0 } }

    #[test]
    fn net_accumulates_across_markets() {
        let mut e = ExposureState::default();
        e.apply(key(), 100.0);
        e.apply(key(), 50.0);
        assert_eq!(e.net(key()), 150.0);
    }

    #[test]
    fn cap_blocks_when_aggregate_would_exceed() {
        let mut e = ExposureState::default();
        e.apply(key(), 900.0);
        assert!(e.would_exceed(key(), 200.0, 1000.0)); // 900 + 200 > 1000
        assert!(!e.would_exceed(key(), 50.0, 1000.0)); // 900 + 50 <= 1000
    }

    #[test]
    fn opposite_sign_reduces_exposure() {
        let mut e = ExposureState::default();
        e.apply(key(), 900.0);
        assert!(!e.would_exceed(key(), -800.0, 1000.0)); // |900-800| = 100
    }
}
```

- [ ] **Step 2: Run to verify fail→pass**

Run: `cargo test -p pm-engine exposure::`
Expected: 3 tests PASS (after replacing the placeholder).

- [ ] **Step 3: Commit**

```bash
git add crates/pm-engine/src/exposure.rs
git commit -m "pm-engine: incremental ExposureState + correlated-exposure cap"
```

---

## Task 7: RiskGate — port the live RiskEngine checks + integrate the cap

The live `RiskEngine` is the stricter shape and the source of truth. **Port** its checks (do not invent new ones) and add the exposure-cap gate. Source: `polymarket-agent/polymarket-exec/src/core/risk.rs:158` (`RiskEngine::evaluate`) and `RiskLimits`.

**Files:**
- Create: `crates/pm-engine/src/risk.rs`

- [ ] **Step 1: Define the target surface and a characterization test**

`crates/pm-engine/src/risk.rs`:
```rust
use crate::exposure::{ExposureKey, ExposureState};
use crate::portfolio::Portfolio;
use crate::seams::OrderIntent;

/// Hard caps. Field names and semantics mirror the live `RiskLimits`
/// (core/risk.rs) so live and backtest gate identically. Port the exact field
/// set from there; the subset below is the v1 minimum the tests pin.
#[derive(Debug, Clone, Copy)]
pub struct RiskLimits {
    pub max_order_notional_usd: f64,
    pub max_gross_notional_usd: f64,
    pub max_net_notional_per_market_usd: f64,
    pub min_free_cash_usd: f64,
    pub min_portfolio_equity_usd: f64,
    pub max_open_orders_per_market: usize,
    /// Correlated-exposure cap (absolute net shares) per (token, window).
    pub max_correlated_net_shares: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RiskDecision {
    Approve,
    Reject(&'static str),
}

pub struct RiskGate {
    pub limits: RiskLimits,
}

impl RiskGate {
    /// Mirror `RiskEngine::evaluate`'s check order exactly (port from core/risk.rs:158):
    /// invalid order, equity floor, per-order notional, open-order count (per market),
    /// position quantity cap, sell-vs-inventory, free-cash floor, gross notional,
    /// market net notional. Close/rescue intents bypass entry caps (only free-cash +
    /// equity floor apply) — preserve that branch.
    ///
    /// Then ADD: the correlated-exposure cap via `exposure.would_exceed(...)`.
    pub fn check(
        &self,
        order: &OrderIntent,
        portfolio: &Portfolio,
        exposure: &ExposureState,
        exposure_key: ExposureKey,
        signed_shares_delta: f64,
    ) -> RiskDecision {
        // PORTED BODY: replicate core/risk.rs:158 checks against `portfolio`
        // (free_cash_usd, gross_exposure_usd, net_exposure_for_market_usd, equity).
        // Below is only the NEW cap gate the test pins; the ported checks go above it.
        if self.limits.max_correlated_net_shares > 0.0
            && exposure.would_exceed(exposure_key, signed_shares_delta, self.limits.max_correlated_net_shares)
        {
            return RiskDecision::Reject("correlated_exposure_cap");
        }
        RiskDecision::Approve
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Token;
    use crate::portfolio::Portfolio;
    use crate::seams::{OrderId, OrderIntent};
    use pm_strategy::Side;
    use pm_types::MarketId;

    fn limits() -> RiskLimits {
        RiskLimits {
            max_order_notional_usd: 1_000.0,
            max_gross_notional_usd: 10_000.0,
            max_net_notional_per_market_usd: 1_000.0,
            min_free_cash_usd: 0.0,
            min_portfolio_equity_usd: 0.0,
            max_open_orders_per_market: 8,
            max_correlated_net_shares: 1_000.0,
        }
    }

    fn order() -> OrderIntent {
        OrderIntent { id: OrderId(1), market: MarketId::default(), side: Side::BuyYes,
            shares: 200.0, max_depth: 1, limit_price: None, tag: "t" }
    }

    #[test]
    fn rejects_when_correlated_cap_would_be_exceeded() {
        let gate = RiskGate { limits: limits() };
        let pf = Portfolio::new(10_000.0);
        let mut exp = ExposureState::default();
        let key = ExposureKey { token: Token::Btc, window: 0 };
        exp.apply(key, 900.0);
        assert_eq!(gate.check(&order(), &pf, &exp, key, 200.0),
                   RiskDecision::Reject("correlated_exposure_cap"));
    }

    #[test]
    fn approves_within_cap() {
        let gate = RiskGate { limits: limits() };
        let pf = Portfolio::new(10_000.0);
        let exp = ExposureState::default();
        let key = ExposureKey { token: Token::Btc, window: 0 };
        assert_eq!(gate.check(&order(), &pf, &exp, key, 200.0), RiskDecision::Approve);
    }
}
```

- [ ] **Step 2: Run to verify fail→pass**

Run: `cargo test -p pm-engine risk::`
Expected: 2 tests PASS.

- [ ] **Step 3: Port the remaining checks**

Open `polymarket-agent/polymarket-exec/src/core/risk.rs:158` and replicate each check in `RiskGate::check` *above* the cap gate, reading from `Portfolio` (Task 8 provides `free_cash_usd`, `gross_exposure_usd`, `net_exposure_for_market_usd`, `equity_usd`, `open_orders_in_market`). Add one characterization test per ported check (e.g. `rejects_over_gross_notional`, `bypasses_caps_for_close_intent`), mirroring the live behavior. Keep the check order identical.

- [ ] **Step 4: Run the full risk suite**

Run: `cargo test -p pm-engine risk::`
Expected: all ported-check tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-engine/src/risk.rs
git commit -m "pm-engine: RiskGate (ported RiskEngine checks + correlated-exposure cap)"
```

---

## Task 8: Portfolio — shared capital/position state

Port the live `InventoryState` accounting (source: `polymarket-agent/polymarket-exec/src/core/inventory.rs`). Define the minimum surface the engine and risk gate need, pinned by tests.

**Files:**
- Create: `crates/pm-engine/src/portfolio.rs`

- [ ] **Step 1: Write the failing test + surface**

`crates/pm-engine/src/portfolio.rs`:
```rust
use crate::seams::FillReport;
use pm_strategy::Side;
use pm_types::MarketId;
use std::collections::HashMap;

#[derive(Debug, Default, Clone, Copy)]
pub struct Position {
    pub yes_shares: f64,
    pub no_shares: f64,
}

/// Shared capital + per-market positions. Ported from the live InventoryState:
/// cash, positions, realized P&L, mark-to-market. Keyed access only — never
/// iterated to make a decision.
#[derive(Debug, Clone)]
pub struct Portfolio {
    cash_usd: f64,
    realized_pnl_usd: f64,
    positions: HashMap<MarketId, Position>,
    open_orders_per_market: HashMap<MarketId, usize>,
}

impl Portfolio {
    pub fn new(starting_cash_usd: f64) -> Self {
        Self { cash_usd: starting_cash_usd, realized_pnl_usd: 0.0,
            positions: HashMap::new(), open_orders_per_market: HashMap::new() }
    }

    pub fn position(&self, m: MarketId) -> Position { self.positions.get(&m).copied().unwrap_or_default() }
    pub fn free_cash_usd(&self) -> f64 { self.cash_usd } // reservations added when order lifecycle lands
    pub fn realized_pnl_usd(&self) -> f64 { self.realized_pnl_usd }
    pub fn open_orders_in_market(&self, m: MarketId) -> usize {
        self.open_orders_per_market.get(&m).copied().unwrap_or(0)
    }

    /// Apply a fill: move cash, adjust the position. YES/NO share both at YES-terms
    /// price `p`; a NO fill's reported `price` is its NO price. Port the exact
    /// cash math from InventoryState::apply_fill.
    pub fn apply_fill(&mut self, f: &FillReport) {
        let pos = self.positions.entry(f.market).or_default();
        let notional = f.shares * f.price as f64;
        match f.side {
            Side::BuyYes => { pos.yes_shares += f.shares; self.cash_usd -= notional + f.fee_usd; }
            Side::SellYes => { pos.yes_shares -= f.shares; self.cash_usd += notional - f.fee_usd; }
            Side::BuyNo => { pos.no_shares += f.shares; self.cash_usd -= notional + f.fee_usd; }
            Side::SellNo => { pos.no_shares -= f.shares; self.cash_usd += notional - f.fee_usd; }
        }
    }

    /// Mark-to-market equity at YES mid `p` for a single market (extended per-market
    /// when the engine marks the whole book). Port the formula from InventoryState.
    pub fn equity_usd(&self, marks: &HashMap<MarketId, f32>) -> f64 {
        let mut eq = self.cash_usd;
        for (m, pos) in self.positions.iter() {
            let p = *marks.get(m).unwrap_or(&0.5) as f64;
            eq += pos.yes_shares * p + pos.no_shares * (1.0 - p);
        }
        eq
    }

    /// Settle a resolved market: winning shares pay 1.0, losing pay 0.0.
    pub fn settle(&mut self, m: MarketId, resolved_yes: bool) {
        if let Some(pos) = self.positions.remove(&m) {
            let payout = if resolved_yes { pos.yes_shares } else { pos.no_shares };
            self.cash_usd += payout;
            self.realized_pnl_usd += payout; // cost already debited at fill time
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::{FillLiquidity, OrderId};
    use pm_types::MarketId;

    fn buy_yes(m: MarketId, shares: f64, price: f32) -> FillReport {
        FillReport { order: OrderId(1), market: m, side: Side::BuyYes, shares,
            price, fee_usd: 0.0, liquidity: FillLiquidity::Taker, ts: 0 }
    }

    #[test]
    fn buy_yes_moves_cash_and_position() {
        let m = MarketId::default();
        let mut pf = Portfolio::new(1_000.0);
        pf.apply_fill(&buy_yes(m, 100.0, 0.40));
        assert_eq!(pf.position(m).yes_shares, 100.0);
        assert!((pf.free_cash_usd() - 960.0).abs() < 1e-9); // 1000 - 100*0.40
    }

    #[test]
    fn settle_pays_winning_side() {
        let m = MarketId::default();
        let mut pf = Portfolio::new(1_000.0);
        pf.apply_fill(&buy_yes(m, 100.0, 0.40)); // cash 960, 100 yes
        pf.settle(m, true);                       // +100 payout
        assert!((pf.free_cash_usd() - 1_060.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run to verify fail→pass**

Run: `cargo test -p pm-engine portfolio::`
Expected: 2 tests PASS.

- [ ] **Step 3: Reconcile the ported math**

Compare `apply_fill`, `equity_usd`, and reservation handling against `inventory.rs` (`apply_fill`, `gross_exposure_usd`, `free_cash_usd`, `net_exposure_for_market_usd`). Add the `gross_exposure_usd`/`net_exposure_for_market_usd` accessors the `RiskGate` (Task 7) needs, with a test each. Where the live version reserves cash on submit, leave a `reserve`/`release` pair stubbed with a TODO test marked `#[ignore]` until the order lifecycle lands in Phase 2 (note it explicitly so it is not silently dropped).

- [ ] **Step 4: Run the full portfolio suite**

Run: `cargo test -p pm-engine portfolio::`
Expected: all PASS (ignored reservation test reported as ignored).

- [ ] **Step 5: Commit**

```bash
git add crates/pm-engine/src/portfolio.rs
git commit -m "pm-engine: shared Portfolio state ported from InventoryState"
```

---

## Task 9: StrategyHost — the single Ctx-builder

This is the unification keystone: ONE `build_ctx` replaces the two divergent builders (backtest `runner.rs` and live `br2_live.rs`).

**Files:**
- Create: `crates/pm-engine/src/host.rs`

- [ ] **Step 1: Write the failing test + surface**

`crates/pm-engine/src/host.rs`:
```rust
use crate::exposure::{ExposureKey, ExposureState};
use crate::portfolio::Portfolio;
use pm_strategy::Ctx;
use pm_types::{MarketId, ReplayEvent};

/// Builds the strategy `Ctx` from shared + per-market state. The SINGLE source
/// of Ctx construction for both drivers. Cross-market exposure fields are filled
/// from `ExposureState`, so live and backtest see identical context.
pub fn build_ctx(
    portfolio: &Portfolio,
    market: MarketId,
    event: &ReplayEvent,
    events_seen: u64,
    btc_exposure_key: ExposureKey,
    eth_exposure_key: ExposureKey,
    exposure: &ExposureState,
) -> Ctx {
    let pos = portfolio.position(market);
    Ctx {
        events_seen,
        yes_shares: pos.yes_shares,
        no_shares: pos.no_shares,
        cash_usdc: portfolio.free_cash_usd(),
        market_close_ns: event.flags_close_ns_placeholder(), // see note
        btc_net_exposure_shares: exposure.net(btc_exposure_key),
        eth_net_exposure_shares: exposure.net(eth_exposure_key),
        ..Ctx::default()
    }
}
```
Note: `market_close_ns` comes from per-market metadata the engine tracks (set at `MARKET_OPEN`), NOT from `ReplayEvent`; replace the placeholder with the engine's `MarketCtx.close_ns` when wiring Task 10. For this unit test, pass it in. Adjust the signature to take `close_ns: i64` directly:
```rust
// final signature used by the test and the engine:
pub fn build_ctx(portfolio: &Portfolio, market: MarketId, events_seen: u64, close_ns: i64,
    btc_key: ExposureKey, eth_key: ExposureKey, exposure: &ExposureState) -> Ctx { /* as above */ }
```

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Token;
    use crate::seams::{FillLiquidity, FillReport, OrderId};
    use pm_strategy::Side;

    #[test]
    fn ctx_reflects_position_and_exposure() {
        let m = MarketId::default();
        let mut pf = Portfolio::new(1_000.0);
        pf.apply_fill(&FillReport { order: OrderId(1), market: m, side: Side::BuyYes,
            shares: 100.0, price: 0.40, fee_usd: 0.0, liquidity: FillLiquidity::Taker, ts: 0 });
        let mut exp = ExposureState::default();
        let btc = ExposureKey { token: Token::Btc, window: 0 };
        let eth = ExposureKey { token: Token::Eth, window: 0 };
        exp.apply(btc, 100.0);
        let ctx = build_ctx(&pf, m, 5, 1_700_000_000_000_000_000, btc, eth, &exp);
        assert_eq!(ctx.yes_shares, 100.0);
        assert_eq!(ctx.btc_net_exposure_shares, 100.0);
        assert_eq!(ctx.eth_net_exposure_shares, 0.0);
        assert_eq!(ctx.events_seen, 5);
    }
}
```

- [ ] **Step 2: Run to verify fail→pass**

Run: `cargo test -p pm-engine host::`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/pm-engine/src/host.rs
git commit -m "pm-engine: single Ctx-builder (StrategyHost) unifying both drivers"
```

---

## Task 10: Engine loop — route, decide, gate, submit, fill, settle

**Files:**
- Create: `crates/pm-engine/src/engine.rs`

- [ ] **Step 1: Write the engine + a scripted integration test**

`crates/pm-engine/src/engine.rs`:
```rust
use crate::event::{EngineEvent, Token};
use crate::exposure::{ExposureKey, ExposureState};
use crate::host::build_ctx;
use crate::portfolio::Portfolio;
use crate::risk::{RiskDecision, RiskGate};
use crate::seams::{Clock, Exchange, Feed, OrderId, OrderIntent};
use pm_strategy::{SpotHistoryRef, Side, Strategy}; // SpotHistory/TradeHistory passed in by ref
use pm_types::{MarketId, ReplayEvent, ReplayFlags, SpotHistory, TradeHistory};
use std::collections::HashMap;

struct MarketCtx { close_ns: i64, events_seen: u64, token: Token, window: i64 }

pub struct Engine<S: Strategy> {
    strategy: S,
    pub portfolio: Portfolio,
    pub exposure: ExposureState,
    risk: RiskGate,
    markets: HashMap<MarketId, MarketCtx>,
    next_order_id: u64,
    // shared empty histories for Phase 1; Phase 2 maintains rolling windows
    spot: SpotHistory,
    trades: TradeHistory,
    marks: HashMap<MarketId, f32>,
    /// classifier: map a market to its (token, window) for exposure keying
    classify: fn(MarketId) -> (Token, i64),
}

impl<S: Strategy> Engine<S> {
    pub fn new(strategy: S, portfolio: Portfolio, risk: RiskGate,
               classify: fn(MarketId) -> (Token, i64)) -> Self {
        Self { strategy, portfolio, exposure: ExposureState::default(), risk,
            markets: HashMap::new(), next_order_id: 1,
            spot: SpotHistory::default(), trades: TradeHistory::default(),
            marks: HashMap::new(), classify }
    }

    pub fn run<F: Feed, X: Exchange, C: Clock>(&mut self, feed: &mut F, ex: &mut X, clock: &C) {
        while let Some(ev) = feed.next() {
            match ev {
                EngineEvent::Market { replay, no_book: _ } => self.on_market(&replay, ex, clock),
            }
            for fill in ex.poll_fills(clock.now()) {
                self.portfolio.apply_fill(&fill);
                let (token, window) = (self.classify)(fill.market);
                let signed = signed_shares(fill.side, fill.shares);
                self.exposure.apply(ExposureKey { token, window }, signed);
            }
        }
    }

    fn on_market<X: Exchange, C: Clock>(&mut self, e: &ReplayEvent, ex: &mut X, clock: &C) {
        let (token, window) = (self.classify)(e.market_id);
        let mc = self.markets.entry(e.market_id).or_insert(MarketCtx {
            close_ns: e.ts_ns, events_seen: 0, token, window });
        self.marks.insert(e.market_id, e.yes_mid);

        if e.flags.contains(ReplayFlags::MARKET_CLOSE) {
            let resolved_yes = e.yes_mid >= 0.5; // engine receives explicit resolution in Phase 2
            self.portfolio.settle(e.market_id, resolved_yes);
            self.strategy.on_market_resolved(e.yes_mid, resolved_yes);
            return;
        }

        mc.events_seen += 1;
        let close_ns = mc.close_ns;
        let events_seen = mc.events_seen;
        let btc_key = ExposureKey { token: Token::Btc, window };
        let eth_key = ExposureKey { token: Token::Eth, window };
        let ctx = build_ctx(&self.portfolio, e.market_id, events_seen, close_ns,
            btc_key, eth_key, &self.exposure);

        let (out, _model) = self.strategy.on_event_scored(e, &ctx, &self.spot, &self.trades);
        for req in out.orders {
            let id = OrderId(self.next_order_id);
            self.next_order_id += 1;
            let intent = OrderIntent { id, market: e.market_id, side: req.side,
                shares: req.shares, max_depth: req.max_depth, limit_price: req.limit_price, tag: req.tag };
            let key = ExposureKey { token, window };
            let signed = signed_shares(req.side, req.shares);
            if self.risk.check(&intent, &self.portfolio, &self.exposure, key, signed) == RiskDecision::Approve {
                ex.submit(intent, clock.now());
            }
        }
    }
}

fn signed_shares(side: Side, shares: f64) -> f64 {
    match side { Side::BuyYes | Side::SellNo => shares, Side::SellYes | Side::BuyNo => -shares }
}
```
(`SpotHistoryRef` import is illustrative — use the real `&SpotHistory`/`&TradeHistory` by-ref types from `pm_types`. Remove the unused import. If `SpotHistory`/`TradeHistory` lack `Default`, construct empties via their real constructors.)

- [ ] **Step 2: Add the integration test**

`crates/pm-engine/tests/engine_integration.rs`:
```rust
use pm_engine::engine::Engine;
use pm_engine::event::{EngineEvent, Token};
use pm_engine::portfolio::Portfolio;
use pm_engine::risk::{RiskGate, RiskLimits};
use pm_engine::testkit::{InstantExchange, ScriptedFeed, SimClock};
use pm_strategy::{Ctx, OrderRequest, Side, StrategyOutput, Strategy};
use pm_types::{MarketId, NoBook, ReplayEvent, ReplayFlags};
use std::cell::Cell;
use std::rc::Rc;

/// Test strategy: buy 100 YES on the first event it sees, then hold.
struct BuyOnce { fired: bool }
impl Strategy for BuyOnce {
    fn on_event(&mut self, _e: &ReplayEvent, _c: &Ctx, _s: &pm_types::SpotHistory, _t: &pm_types::TradeHistory) -> StrategyOutput {
        if self.fired { return StrategyOutput::hold(); }
        self.fired = true;
        StrategyOutput::one(OrderRequest { side: Side::BuyYes, shares: 100.0, max_depth: 1, limit_price: None, tag: "buy" })
    }
}

fn limits() -> RiskLimits {
    RiskLimits { max_order_notional_usd: 1e9, max_gross_notional_usd: 1e9,
        max_net_notional_per_market_usd: 1e9, min_free_cash_usd: 0.0,
        min_portfolio_equity_usd: 0.0, max_open_orders_per_market: 99,
        max_correlated_net_shares: 1e9 }
}

fn ev(ts: i64, m: MarketId, close: bool) -> EngineEvent {
    let mut flags = ReplayFlags::BOOK_UPDATE;
    if close { flags = ReplayFlags::MARKET_CLOSE; }
    let replay = ReplayEvent { ts_ns: ts, market_id: m, yes_mid: 0.6, yes_bid: 0.59,
        yes_ask: 0.61, volume: 0.0, bids: Default::default(), asks: Default::default(),
        spot_price: 0.0, flags };
    EngineEvent::Market { replay, no_book: NoBook::default() }
}

#[test]
fn engine_buys_once_then_settles_yes() {
    let m = MarketId::default();
    let clock_cell = Rc::new(Cell::new(0i64));
    let mut feed = ScriptedFeed::new(
        vec![ev(10, m, false), ev(20, m, false), ev(30, m, true)],
        clock_cell.clone());
    let mut ex = InstantExchange::new(0.60, 0.0);
    let clock = SimClock { ts: clock_cell };

    let mut engine = Engine::new(BuyOnce { fired: false }, Portfolio::new(1_000.0),
        RiskGate { limits: limits() }, |_m| (Token::Btc, 0));
    engine.run(&mut feed, &mut ex, &clock);

    // One order submitted; bought 100 YES @0.60 => cash 940; resolved YES => +100 => 1040.
    assert_eq!(ex.submitted.len(), 1);
    assert!((engine.portfolio.free_cash_usd() - 1_040.0).abs() < 1e-9);
}
```

- [ ] **Step 3: Run the integration test**

Run: `cargo test -p pm-engine --features testkit --test engine_integration`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/pm-engine/src/engine.rs crates/pm-engine/tests/engine_integration.rs
git commit -m "pm-engine: engine loop (route/decide/gate/submit/fill/settle) + integration test"
```

---

## Task 11: Determinism golden-trace test (Part A, mock level)

**Files:**
- Modify: `crates/pm-engine/src/engine.rs` (add a trace recorder)
- Modify: `crates/pm-engine/tests/engine_integration.rs`

- [ ] **Step 1: Add an order/fill trace to the engine**

In `engine.rs`, add `pub trace: Vec<(i64, &'static str, MarketId, f64)>` to `Engine`, push `(clock.now(), "submit", market, shares)` on each submit and `(fill.ts, "fill", fill.market, fill.shares)` on each applied fill. Initialize empty in `new`.

- [ ] **Step 2: Write the determinism test**

Append to `engine_integration.rs`:
```rust
fn run_once() -> Vec<(i64, &'static str, MarketId, f64)> {
    let m = MarketId::default();
    let clock_cell = Rc::new(Cell::new(0i64));
    let mut feed = ScriptedFeed::new(vec![ev(10, m, false), ev(20, m, false), ev(30, m, true)], clock_cell.clone());
    let mut ex = InstantExchange::new(0.60, 0.0);
    let clock = SimClock { ts: clock_cell };
    let mut engine = Engine::new(BuyOnce { fired: false }, Portfolio::new(1_000.0),
        RiskGate { limits: limits() }, |_m| (Token::Btc, 0));
    engine.run(&mut feed, &mut ex, &clock);
    engine.trace
}

#[test]
fn golden_trace_is_deterministic() {
    assert_eq!(run_once(), run_once());
}
```

- [ ] **Step 3: Run it**

Run: `cargo test -p pm-engine --features testkit --test engine_integration golden_trace`
Expected: PASS (identical traces).

- [ ] **Step 4: Commit**

```bash
git add crates/pm-engine/src/engine.rs crates/pm-engine/tests/engine_integration.rs
git commit -m "pm-engine: deterministic golden-trace test (conformance Part A, mock level)"
```

---

## Task 12: Multi-market scheduler ordering + shared-capital test

**Files:**
- Modify: `crates/pm-engine/tests/engine_integration.rs`

- [ ] **Step 1: Write the interleaved-markets test**

Append:
```rust
#[test]
fn interleaves_two_markets_in_ts_order_sharing_capital() {
    let m1 = MarketId::default();
    // Construct a distinct second MarketId via its real constructor; if MarketId
    // wraps u32, use MarketId::from(1u32) or the crate's builder.
    let m2 = pm_types::MarketId::from(1u32);
    let clock_cell = Rc::new(Cell::new(0i64));
    // Events interleaved across markets by timestamp:
    let mut feed = ScriptedFeed::new(vec![
        ev(10, m1, false), ev(15, m2, false), ev(30, m1, true), ev(35, m2, true),
    ], clock_cell.clone());
    let mut ex = InstantExchange::new(0.50, 0.0);
    let clock = SimClock { ts: clock_cell };
    let mut engine = Engine::new(BuyOnceEach::default(), Portfolio::new(1_000.0),
        RiskGate { limits: limits() }, |m| (Token::Btc, 0));
    engine.run(&mut feed, &mut ex, &clock);
    // Both markets bought 100 @0.50 from the SAME pool: 1000 - 50 - 50 = 900,
    // then both resolve YES: +100 +100 => 1100.
    assert_eq!(ex.submitted.len(), 2);
    assert!((engine.portfolio.free_cash_usd() - 1_100.0).abs() < 1e-9);
}
```
Add a `BuyOnceEach` strategy that buys 100 YES the first time it sees each distinct `events_seen==1` market (track a small set keyed by a per-call marker; simplest: buy when `ctx.yes_shares == 0.0 && ctx.no_shares == 0.0`).

- [ ] **Step 2: Run it**

Run: `cargo test -p pm-engine --features testkit --test engine_integration interleaves`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/pm-engine/tests/engine_integration.rs
git commit -m "pm-engine: multi-market interleaving + shared-capital test"
```

---

## Task 13: Host the real br2 (BonereaperV2) smoke test

Proves the real strategy plugs in unchanged through the engine.

**Files:**
- Modify: `crates/pm-engine/tests/engine_integration.rs`

- [ ] **Step 1: Write the smoke test**

```rust
use pm_strategy::{BonereaperV2, BonereaperV2Config};

#[test]
fn hosts_real_br2_deterministically() {
    let m = MarketId::default();
    let make = || {
        let clock_cell = Rc::new(Cell::new(0i64));
        // a handful of book updates then close; br2 is selective and may not trade,
        // which is fine — we assert determinism + no panic, not a fill.
        let evs: Vec<EngineEvent> = (0..50).map(|i| ev(1_000 + i * 1_000, m, false))
            .chain(std::iter::once(ev(1_000 + 50 * 1_000, m, true))).collect();
        let mut feed = ScriptedFeed::new(evs, clock_cell.clone());
        let mut ex = InstantExchange::new(0.50, 0.0);
        let clock = SimClock { ts: clock_cell };
        let strat = BonereaperV2::new(BonereaperV2Config::default());
        let mut engine = Engine::new(strat, Portfolio::new(1_000.0),
            RiskGate { limits: limits() }, |_m| (Token::Btc, 0));
        engine.run(&mut feed, &mut ex, &clock);
        engine.trace
    };
    assert_eq!(make(), make()); // deterministic with the real strategy hosted
}
```
(Use the real `BonereaperV2` constructor/config names — verify against `crates/pm-strategy/src/bonereaper_v2.rs`; `BonereaperV2`/`BonereaperV2Config` are re-exported from `pm-strategy` lib.rs:163.)

- [ ] **Step 2: Run it**

Run: `cargo test -p pm-engine --features testkit --test engine_integration hosts_real_br2`
Expected: PASS (deterministic, no panic).

- [ ] **Step 3: Run the whole crate suite + clippy**

Run: `cargo test -p pm-engine --features testkit`
Run: `cargo clippy -p pm-engine --all-targets --features testkit -- -D warnings`
Expected: all tests PASS; clippy clean (fix any warnings).

- [ ] **Step 4: Commit**

```bash
git add crates/pm-engine/tests/engine_integration.rs
git commit -m "pm-engine: smoke test hosting real br2 deterministically through the engine"
```

---

## Phase 1 done — verification gate

Before declaring Phase 1 complete:
- `cargo test -p pm-engine --features testkit` — all green
- `cargo test -p pm-types no_book` — green
- `cargo build` (whole workspace) — green (the new crate must not break the workspace)
- `cargo clippy -p pm-engine --all-targets --features testkit -- -D warnings` — clean

The deliverable: a compiling, unit-tested `pm-engine` core that hosts br2 unchanged, runs the unified decision→risk→submit→fill→settle cycle over a multi-market timestamp-ordered feed against shared capital + a deterministic correlated-exposure cap, proven deterministic by a golden-trace test — all on mock seams.

---

## Follow-on plans (to be written when Phase 1 lands)

These reference the concrete Phase-1 APIs (`Feed`/`Exchange`/`Clock`, `EngineEvent`, `Portfolio`, `RiskGate`, `Engine`), so they are detailed only after those signatures are real.

**Phase 2 — sim Exchange + backtest driver (`docs/superpowers/plans/<date>-pm-engine-backtest-driver.md`):**
- Implement the real both-book fill model behind `Exchange`: taker VWAP across the real opposing ladder (YES asks / real NO asks, not 1−yes) with latency-aware book re-read + real fees; maker fills driven by the real trade tape (port `check_trade_driven_resting_fills` from `runner.rs:633`), not top-of-book touch; partial fills.
- Recorded `Feed`: k-way merge of the both-legs parquet tapes (book_snapshot_25 YES+NO, spot, trades) by `ts_ns`, emitting `EngineEvent::Market { replay, no_book }`; maintain rolling `SpotHistory`/`TradeHistory`.
- Rewrite `pm-app` walk-forward as a thin driver of `Engine` (replace the bespoke `run_backtest` loop).
- Tests: golden-trace on real recorded data (Part A); champion equivalence (the engine reproduces frozen 062901 br2 on the May BTC-5m cache within tolerance); per-cell residual-vs-real-fill diagnostic.

**Phase 3 — live driver + driver equivalence (`docs/superpowers/plans/<date>-pm-engine-live-driver.md`):**
- WS `Feed`: merge `market_ws` (both legs → `BothBook`), `spot_ws`, `user_ws` into one timestamp-ordered `EngineEvent` stream feeding `Engine` (the live `Feed::next` blocks on the merged receiver).
- CLOB `Exchange`: wrap the existing `ExecutionAdapter` (submit/cancel) + drain user-ws fills in `poll_fills` + reconcile.
- Wall `Clock`.
- Keep live-ops (kill-switch, health watchdog, auto-redeem/wrap, checkpoint) as a wrapper around `Engine` in `polymarket-exec`.
- Build a small WS-window recorder tool.
- Test: Part B driver equivalence — a captured live WS window normalized by both the live ws-Feed and the backtest parquet-Feed yields the identical `EngineEvent` stream, then identical engine output.
