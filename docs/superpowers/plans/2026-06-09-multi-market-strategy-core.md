# Multi-Market Strategy Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the clean, multi-market `ConvexBookStrategy` as three composed units — `Signal` (conviction from the model), `PositionManager` (stateful convex-book accumulator), `ExecutionPolicy` (maker/taker/adaptive) — hosted per-market on the engine, on real both-book prices, validated on synthetic fixtures.

**Architecture:** A new `crates/pm-strategy/src/convex/` module. `ConvexBookStrategy` implements the existing `Strategy` trait (so the engine hosts a fresh instance per market via Plan 1's mechanism). Each event: `Signal` reads `ctx.model_output` + the favourite ask to produce a `Conviction`; `PositionManager` turns conviction + real both-leg prices + time-to-close + its own per-market inventory into a `TargetIncrement` (a favourite clip and/or a cheap convex-tail clip — roughly share-balanced, downside-covered, time-evolving); `ExecutionPolicy` turns that into `OrderRequest`s with a posture (post-only maker / taker / adaptive). To get the *real* NO price (br2 used synthetic `1 - yes`), `Ctx` gains NO top-of-book fields that the engine fills from `no_book`.

**Tech Stack:** Rust; `pm-strategy` (strategy + `Strategy` trait + `Ctx`), `pm-engine` (host + `build_ctx`), `pm-model` (`ModelOutput`). Params seeded from the champion `configs/bonereaper_v2_leader.toml`; exact taper/threshold tuning is deferred to Plan 4 (the spec marks sizing functions "tune on backtest"). `cargo test`/`clippy`.

**Depends on:** Plan 1 (per-market hosting) — DONE. Data-independent (synthetic fixtures only).

**Scope note (YAGNI):** v1 `PositionManager` captures the *core* convex-book structure (favourite clip gated by model support + ask range + time; cheap tail sized by favourite-loss-coverage), seeded from champion params. The many br2 tapers (price_taper, edge_taper, fragile_taper, range_throttle, regime boosts) are refinements added during Plan 4 tuning, NOT now — building them blind to faithful backtests would overfit.

---

## File Structure

- Create `crates/pm-strategy/src/convex/mod.rs` — `ConvexBookStrategy` (composed; `impl Strategy`), `ConvexBookConfig`, re-exports.
- Create `crates/pm-strategy/src/convex/signal.rs` — `Conviction`, `SignalGate`, `evaluate()`.
- Create `crates/pm-strategy/src/convex/position.rs` — `PositionManager`, `PositionConfig`, `TargetIncrement`, `TargetLeg`, `BothBookPrices`.
- Create `crates/pm-strategy/src/convex/execution.rs` — `ExecutionPolicy`, `Posture`.
- Modify `crates/pm-strategy/src/lib.rs` — add NO top-of-book fields to `Ctx`; `pub mod convex;` + re-exports.
- Modify `crates/pm-engine/src/host.rs` — `build_ctx` takes `no_book`, fills the NO fields.
- Modify `crates/pm-engine/src/engine.rs` — pass `no_book` into `on_market` → `build_ctx`.

---

### Task 1: Ctx gains real NO top-of-book; engine fills it

The convex book is priced on the real NO ladder, not `1 - yes`. Add NO top-of-book to `Ctx` and have the engine populate it from the per-event `no_book`.

**Files:**
- Modify: `crates/pm-strategy/src/lib.rs` (Ctx fields)
- Modify: `crates/pm-engine/src/host.rs` (`build_ctx` signature + fill)
- Modify: `crates/pm-engine/src/engine.rs` (pass `no_book` to `on_market`/`build_ctx`)
- Test: `crates/pm-engine/src/host.rs` (inline)

- [ ] **Step 1: Add NO fields to `Ctx`**

In `lib.rs` `Ctx` (after `market_close_ns`), add:
```rust
    /// Real NO-leg top of book (from the opposing ladder, NOT synthetic 1-yes).
    /// 0.0 when no NO book is available (Phase-1 tests / pre-first-NO).
    pub no_bid: f32,
    pub no_ask: f32,
    pub no_mid: f32,
```
`Ctx` derives `Default`, so these default to 0.0 — existing constructors are unaffected.

- [ ] **Step 2: Write the failing test (build_ctx fills NO fields)**

In `host.rs` tests, add:
```rust
#[test]
fn ctx_carries_real_no_top_of_book() {
    use pm_types::{BookLevel, NoBook};
    let pf = Portfolio::new(1_000.0);
    let exp = ExposureState::default();
    let btc = ExposureKey { token: Token::Btc, window: 0 };
    let eth = ExposureKey { token: Token::Eth, window: 0 };
    let mut nb = NoBook::default();
    nb.bids[0] = BookLevel { price: 0.18, size: 100.0 };
    nb.asks[0] = BookLevel { price: 0.21, size: 100.0 };
    let ctx = build_ctx(&pf, MarketId(0), 1, 0, btc, eth, &exp, &nb);
    assert!((ctx.no_bid - 0.18).abs() < 1e-6);
    assert!((ctx.no_ask - 0.21).abs() < 1e-6);
    assert!((ctx.no_mid - 0.195).abs() < 1e-6);
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p pm-engine --features testkit ctx_carries_real_no -v`
Expected: FAIL to compile — `build_ctx` takes 7 args, test passes 8.

- [ ] **Step 4: Add `no_book` to `build_ctx` and fill the fields**

In `host.rs`, add `no_book: &pm_types::NoBook` as the final param. Before the `Ctx { ... }` return, compute:
```rust
    let no_bid = no_book.bids[0].price;
    let no_ask = no_book.asks[0].price;
    let no_mid = if no_bid > 0.0 && no_ask > 0.0 { 0.5 * (no_bid + no_ask) } else { 0.0 };
```
and set `no_bid, no_ask, no_mid` in the returned `Ctx` (alongside the existing fields; leave `..Ctx::default()` for the rest).

- [ ] **Step 5: Pass `no_book` from the engine**

In `engine.rs` `run`, the `EngineEvent::Market { replay, no_book }` arm calls `self.on_market(&replay, ex, clock)`. Change `on_market` to take `no_book: &NoBook` and pass it: `self.on_market(&replay, &no_book, ex, clock)`. Inside `on_market`, pass `no_book` to the `build_ctx(...)` call as the new final argument. (Import `pm_types::NoBook` if not already.)

- [ ] **Step 6: Run test + full engine suite**

Run: `cargo test -p pm-engine --features testkit 2>&1 | tail -6`
Expected: `ctx_carries_real_no_top_of_book` PASSES; all existing engine tests still PASS (the NO fields default to 0.0 for the mock `NoBook::default()` they already pass, and `build_ctx`'s extra arg is threaded through).

- [ ] **Step 7: Commit**

```bash
git add crates/pm-strategy/src/lib.rs crates/pm-engine/src/host.rs crates/pm-engine/src/engine.rs
git commit -m "engine: thread real NO top-of-book into Ctx (replaces synthetic 1-yes for the strategy)"
```

---

### Task 2: Signal unit — conviction from the model

**Files:**
- Create: `crates/pm-strategy/src/convex/signal.rs`
- Modify: `crates/pm-strategy/src/lib.rs` (`pub mod convex;`)
- Modify: `crates/pm-strategy/src/convex/mod.rs` (`pub mod signal;`) — create `mod.rs` with just the submodule declarations for now.

- [ ] **Step 1: Create the module skeleton**

Create `crates/pm-strategy/src/convex/mod.rs`:
```rust
//! Clean multi-market convex-book strategy: Signal -> PositionManager -> ExecutionPolicy.
pub mod signal;
```
Add `pub mod convex;` to `lib.rs` (near the other `pub use`/`mod` lines).

- [ ] **Step 2: Write the failing test**

Create `crates/pm-strategy/src/convex/signal.rs` with the test first:
```rust
use crate::{Ctx, Side};
use pm_model::ModelOutput;

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_model(direction: f32, calibrated_p: f32, confidence: f32, risk: f32, yes_mid: f32) -> Ctx {
        Ctx {
            model_output: Some(ModelOutput {
                direction_score: direction,
                confidence_score: confidence,
                calibrated_p,
                risk_score: risk,
            }),
            ..Ctx::default()
        }
        // note: yes_mid is passed separately to evaluate (from the event), not on Ctx
        .with_yes_mid(yes_mid)
    }
    // helper to keep the test readable; remove if evaluate takes yes_mid directly
}
```
(If a `with_yes_mid` helper is awkward, pass `yes_mid` and `fav_ask` directly to `evaluate` — see Step 3. Write the test to call `evaluate(&ctx, yes_mid, fav_ask, &gate)`.)

Concrete test:
```rust
#[test]
fn favourite_is_market_side_and_must_pass_model_gate() {
    let gate = SignalGate::default();
    // YES is favourite (yes_mid 0.8), model agrees (direction +), p high, low risk, edge ok.
    let ctx = Ctx { model_output: Some(ModelOutput {
        direction_score: 0.5, confidence_score: 0.75, calibrated_p: 0.78, risk_score: 0.3,
    }), ..Ctx::default() };
    let conv = evaluate(&ctx, 0.80 /*yes_mid*/, 0.74 /*fav_ask*/, &gate)
        .expect("supported convction");
    assert_eq!(conv.favourite, Side::BuyYes);
    assert!((conv.side_p - 0.78).abs() < 1e-6);
    assert!((conv.edge - (0.78 - 0.74)).abs() < 1e-6);

    // Same market, but model disagrees (direction negative) -> not supported -> None.
    let ctx_bad = Ctx { model_output: Some(ModelOutput {
        direction_score: -0.5, confidence_score: 0.75, calibrated_p: 0.78, risk_score: 0.3,
    }), ..Ctx::default() };
    assert!(evaluate(&ctx_bad, 0.80, 0.74, &gate).is_none(), "model disagrees with favourite");
}
```

- [ ] **Step 3: Implement `signal.rs`**

```rust
use crate::{Ctx, Side};

/// Model-support gate thresholds (seeded from champion late_favourite_*).
#[derive(Debug, Clone, Copy)]
pub struct SignalGate {
    pub min_confidence: f32,
    pub max_risk: f32,
    pub min_side_p: f32,
    pub min_edge: f32,
}
impl Default for SignalGate {
    fn default() -> Self {
        Self { min_confidence: 0.68, max_risk: 0.72, min_side_p: 0.62, min_edge: 0.03 }
    }
}

/// Directional conviction for the favoured outcome of one market.
#[derive(Debug, Clone, Copy)]
pub struct Conviction {
    pub favourite: Side, // BuyYes or BuyNo (the side to load)
    pub side_p: f32,     // calibrated prob of the favourite side
    pub edge: f32,       // side_p - favourite_ask
    pub confidence: f32,
    pub risk: f32,
}

/// Favourite side = market-implied (yes_mid >= 0.5 -> YES). Returns `Some` only
/// when the model SUPPORTS that side (agrees on direction and clears the gate),
/// mirroring br2's `model_support_for_side`. `fav_ask` is the real ask of the
/// favourite side (yes_ask for YES, no_ask for NO).
pub fn evaluate(ctx: &Ctx, yes_mid: f32, fav_ask: f32, gate: &SignalGate) -> Option<Conviction> {
    let model = ctx.model_output?;
    let favourite = if yes_mid >= 0.5 { Side::BuyYes } else { Side::BuyNo };
    let model_side_is_yes = model.direction_score >= 0.0;
    let fav_is_yes = matches!(favourite, Side::BuyYes);
    if model_side_is_yes != fav_is_yes {
        return None; // model disagrees with the market-implied favourite
    }
    let side_p = model.calibrated_p; // calibrated_p is already for the model's side == favourite
    let edge = side_p - fav_ask;
    if model.confidence_score < gate.min_confidence
        || model.risk_score > gate.max_risk
        || side_p < gate.min_side_p
        || edge < gate.min_edge
    {
        return None;
    }
    Some(Conviction { favourite, side_p, edge, confidence: model.confidence_score, risk: model.risk_score })
}
```
(Delete the placeholder helper test scaffold from Step 2; keep only `favourite_is_market_side_and_must_pass_model_gate`.)

- [ ] **Step 4: Run test**

Run: `cargo test -p pm-strategy convex::signal -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-strategy/src/lib.rs crates/pm-strategy/src/convex/mod.rs crates/pm-strategy/src/convex/signal.rs
git commit -m "pm-strategy: convex::signal - model-gated directional conviction"
```

---

### Task 3: PositionManager — convex-book accumulator

The stateful per-market core. Given conviction + real both-leg prices + time-to-close + its own inventory, return the next increment: a favourite clip (when the favourite gate + ask-range + time pass) and/or a cheap convex tail clip (sized to cover a fraction of the favourite's notional). Seeded from champion params; intricate tapers deferred.

**Files:**
- Create: `crates/pm-strategy/src/convex/position.rs`
- Modify: `crates/pm-strategy/src/convex/mod.rs` (`pub mod position;`)

- [ ] **Step 1: Write the failing tests**

In `position.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Side;

    fn conv(side: Side, side_p: f32, edge: f32) -> Conviction {
        Conviction { favourite: side, side_p, edge, confidence: 0.75, risk: 0.3 }
    }
    fn prices(yes_ask: f32, yes_bid: f32, no_ask: f32, no_bid: f32) -> BothBookPrices {
        BothBookPrices { yes_ask, yes_bid, no_ask, no_bid }
    }

    #[test]
    fn loads_favourite_late_within_ask_range() {
        let mut pm = PositionManager::new(PositionConfig::default());
        // YES favourite at 0.80 ask, late (secs_to_close 100 < start 120), edge ok.
        let inc = pm.plan(&conv(Side::BuyYes, 0.86, 0.06), &prices(0.80, 0.79, 0.21, 0.19), 100.0);
        let fav = inc.legs.iter().find(|l| l.side == Side::BuyYes).expect("favourite leg");
        assert!(fav.shares > 0.0, "favourite clip should size > 0");
    }

    #[test]
    fn no_favourite_load_before_start_secs() {
        let mut pm = PositionManager::new(PositionConfig::default());
        // Too early (secs_to_close 200 > 300-180=... i.e. secs_in < start).
        let inc = pm.plan(&conv(Side::BuyYes, 0.86, 0.06), &prices(0.80, 0.79, 0.21, 0.19), 200.0);
        assert!(inc.legs.iter().all(|l| l.side != Side::BuyYes), "no favourite before start");
    }

    #[test]
    fn adds_cheap_convex_tail_after_favourite_built() {
        let mut pm = PositionManager::new(PositionConfig::default());
        // First build a favourite position.
        let _ = pm.plan(&conv(Side::BuyYes, 0.86, 0.06), &prices(0.80, 0.79, 0.21, 0.19), 100.0);
        // Now skew is extreme and the opposite (NO) ask is cheap -> tail fires.
        let inc = pm.plan(&conv(Side::BuyYes, 0.90, 0.06), &prices(0.92, 0.91, 0.09, 0.07), 60.0);
        let tail = inc.legs.iter().find(|l| l.side == Side::BuyNo).expect("tail leg");
        assert!(tail.shares > 0.0, "cheap tail should size > 0 once favourite exists");
    }

    #[test]
    fn favourite_clips_respect_max_clips() {
        let mut pm = PositionManager::new(PositionConfig { favourite_max_clips: 1, ..PositionConfig::default() });
        let _ = pm.plan(&conv(Side::BuyYes, 0.86, 0.06), &prices(0.80, 0.79, 0.21, 0.19), 100.0);
        let inc2 = pm.plan(&conv(Side::BuyYes, 0.86, 0.06), &prices(0.80, 0.79, 0.21, 0.19), 90.0);
        assert!(inc2.legs.iter().all(|l| l.side != Side::BuyYes), "favourite capped at max_clips");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p pm-strategy convex::position -v`
Expected: FAIL to compile (types undefined).

- [ ] **Step 3: Implement `position.rs`**

```rust
use crate::Side;
use crate::convex::signal::Conviction;

const BETTING_WINDOW_SECS: f32 = 300.0;

/// Real both-leg top-of-book prices for one event.
#[derive(Debug, Clone, Copy)]
pub struct BothBookPrices {
    pub yes_ask: f32,
    pub yes_bid: f32,
    pub no_ask: f32,
    pub no_bid: f32,
}
impl BothBookPrices {
    fn ask(&self, side: Side) -> f32 {
        match side {
            Side::BuyYes | Side::SellNo => self.yes_ask,
            Side::BuyNo | Side::SellYes => self.no_ask,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PositionConfig {
    pub bankroll_usdc: f64,
    pub max_clip_usdc: f64,
    // favourite (the directional PnL engine), seeded from champion late_favourite_*
    pub favourite_start_secs: f32,   // load only after this many secs into the window
    pub favourite_min_ask: f32,
    pub favourite_max_ask: f32,
    pub favourite_clip_frac: f64,
    pub favourite_max_clips: usize,
    pub favourite_refresh_secs: f32,
    pub favourite_sweep_depth: usize,
    // convex tail (cheap opposite side), seeded from champion tail_*
    pub tail_min_ask: f32,
    pub tail_max_ask: f32,
    pub tail_min_seconds_to_close: f32,
    pub tail_max_clips: usize,
    pub tail_sweep_depth: usize,
    pub tail_refresh_secs: f32,
    pub tail_coverage_frac: f64,     // tail covers this fraction of favourite notional
    pub tail_extreme_skew: f32,      // |yes_mid-0.5| gate (approximated via fav ask)
}
impl Default for PositionConfig {
    fn default() -> Self {
        Self {
            bankroll_usdc: 1000.0, max_clip_usdc: 30.0,
            favourite_start_secs: 180.0, favourite_min_ask: 0.70, favourite_max_ask: 0.97,
            favourite_clip_frac: 1.0, favourite_max_clips: 12, favourite_refresh_secs: 4.0,
            favourite_sweep_depth: 7,
            tail_min_ask: 0.01, tail_max_ask: 0.10, tail_min_seconds_to_close: 10.0,
            tail_max_clips: 3, tail_sweep_depth: 3, tail_refresh_secs: 5.0,
            tail_coverage_frac: 0.50, tail_extreme_skew: 0.20,
        }
    }
}

/// One leg to acquire this tick. `price_ref` is the reference ask used for sizing;
/// ExecutionPolicy decides the actual order price/posture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetLeg {
    pub side: Side,
    pub shares: f64,
    pub max_depth: usize,
    pub price_ref: f32,
}

#[derive(Debug, Clone, Default)]
pub struct TargetIncrement {
    pub legs: Vec<TargetLeg>,
}

/// Stateful per-market convex-book accumulator. One instance per market (the
/// engine clones the strategy per market, so this state is naturally isolated).
pub struct PositionManager {
    cfg: PositionConfig,
    favourite_side: Option<Side>,
    favourite_clips: usize,
    favourite_shares: f64,
    favourite_notional: f64,
    last_favourite_secs: f32,
    tail_clips: usize,
    tail_notional: f64,
    last_tail_secs: f32,
}

fn shares_capped(usdc: f64, px: f32) -> f64 {
    if px <= 0.0 { return 0.0; }
    ((usdc * 0.98 / px as f64) * 1000.0).floor() / 1000.0
}

impl PositionManager {
    pub fn new(cfg: PositionConfig) -> Self {
        Self {
            cfg,
            favourite_side: None, favourite_clips: 0, favourite_shares: 0.0,
            favourite_notional: 0.0, last_favourite_secs: f32::INFINITY,
            tail_clips: 0, tail_notional: 0.0, last_tail_secs: f32::INFINITY,
        }
    }

    pub fn plan(&mut self, conv: &Conviction, prices: &BothBookPrices, secs_to_close: f32) -> TargetIncrement {
        let mut legs = Vec::new();
        let secs_in = (BETTING_WINDOW_SECS - secs_to_close).clamp(0.0, BETTING_WINDOW_SECS);

        // --- Favourite leg (directional PnL engine) ---
        let fav_ask = prices.ask(conv.favourite);
        let side_locked_ok = self.favourite_side.map_or(true, |s| s == conv.favourite);
        let refresh_ok = (self.last_favourite_secs - secs_to_close).abs() >= self.cfg.favourite_refresh_secs
            || self.favourite_clips == 0;
        if secs_in >= self.cfg.favourite_start_secs
            && self.favourite_clips < self.cfg.favourite_max_clips
            && side_locked_ok
            && refresh_ok
            && fav_ask >= self.cfg.favourite_min_ask
            && fav_ask <= self.cfg.favourite_max_ask
        {
            let clip_usdc = self.cfg.max_clip_usdc * self.cfg.favourite_clip_frac;
            let shares = shares_capped(clip_usdc, fav_ask);
            if shares > 0.0 {
                legs.push(TargetLeg {
                    side: conv.favourite, shares,
                    max_depth: self.cfg.favourite_sweep_depth, price_ref: fav_ask,
                });
                self.favourite_side = Some(conv.favourite);
                self.favourite_clips += 1;
                self.favourite_shares += shares;
                self.favourite_notional += shares * fav_ask as f64;
                self.last_favourite_secs = secs_to_close;
            }
        }

        // --- Convex tail leg (cheap opposite side) ---
        if let Some(fav) = self.favourite_side {
            let tail_side = opposite(fav);
            let tail_ask = prices.ask(tail_side);
            let tail_refresh_ok = (self.last_tail_secs - secs_to_close).abs() >= self.cfg.tail_refresh_secs
                || self.tail_clips == 0;
            if self.favourite_notional > 0.0
                && self.tail_clips < self.cfg.tail_max_clips
                && tail_refresh_ok
                && secs_to_close >= self.cfg.tail_min_seconds_to_close
                && tail_ask >= self.cfg.tail_min_ask
                && tail_ask <= self.cfg.tail_max_ask
            {
                // Target tail notional covers `coverage_frac` of the favourite's
                // notional cost (br2 tail_clip_notional): target = fav_notional * cov * tail_px.
                let target = self.favourite_notional * self.cfg.tail_coverage_frac * tail_ask as f64;
                let clip_usdc = (target - self.tail_notional).max(0.0);
                let shares = shares_capped(clip_usdc, tail_ask);
                if shares > 0.0 {
                    legs.push(TargetLeg {
                        side: tail_side, shares,
                        max_depth: self.cfg.tail_sweep_depth, price_ref: tail_ask,
                    });
                    self.tail_clips += 1;
                    self.tail_notional += shares * tail_ask as f64;
                    self.last_tail_secs = secs_to_close;
                }
            }
        }

        TargetIncrement { legs }
    }
}

fn opposite(side: Side) -> Side {
    match side {
        Side::BuyYes => Side::BuyNo,
        Side::BuyNo => Side::BuyYes,
        Side::SellYes => Side::SellNo,
        Side::SellNo => Side::SellYes,
    }
}
```
Add `pub mod position;` to `convex/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p pm-strategy convex::position -v`
Expected: all 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-strategy/src/convex/position.rs crates/pm-strategy/src/convex/mod.rs
git commit -m "pm-strategy: convex::position - stateful convex-book accumulator (favourite + cheap tail)"
```

---

### Task 4: ExecutionPolicy — posture into orders

**Files:**
- Create: `crates/pm-strategy/src/convex/execution.rs`
- Modify: `crates/pm-strategy/src/convex/mod.rs` (`pub mod execution;`)

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Side;
    use crate::convex::position::{TargetIncrement, TargetLeg};

    fn target() -> TargetIncrement {
        TargetIncrement { legs: vec![TargetLeg { side: Side::BuyYes, shares: 10.0, max_depth: 3, price_ref: 0.80 }] }
    }

    #[test]
    fn taker_emits_market_orders() {
        let p = ExecutionPolicy::new(Posture::Taker);
        let orders = p.orders(&target(), 100.0);
        assert_eq!(orders.len(), 1);
        assert!(orders[0].limit_price.is_none(), "taker => no limit price (sweep)");
    }

    #[test]
    fn maker_emits_post_only_limit_at_price_ref() {
        let p = ExecutionPolicy::new(Posture::Maker);
        let orders = p.orders(&target(), 100.0);
        assert_eq!(orders[0].limit_price, Some(0.80), "maker => limit at price_ref");
    }

    #[test]
    fn adaptive_is_maker_with_time_taker_near_close() {
        let p = ExecutionPolicy::new(Posture::Adaptive);
        assert!(p.orders(&target(), 100.0)[0].limit_price.is_some(), "adaptive far from close => maker");
        assert!(p.orders(&target(), 3.0)[0].limit_price.is_none(), "adaptive near close => taker");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p pm-strategy convex::execution -v`
Expected: FAIL to compile.

- [ ] **Step 3: Implement `execution.rs`**

```rust
use crate::OrderRequest;
use crate::convex::position::TargetIncrement;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Posture {
    /// Post-only limit at the reference price (capture rebate / better entry).
    Maker,
    /// Sweep the opposing book (guaranteed fill, pays the spread).
    Taker,
    /// Maker while there is time to wait; taker inside `taker_switch_secs` of close.
    Adaptive,
}

pub struct ExecutionPolicy {
    posture: Posture,
    /// Seconds-to-close under which Adaptive escalates to taker.
    taker_switch_secs: f32,
}

impl ExecutionPolicy {
    pub fn new(posture: Posture) -> Self {
        Self { posture, taker_switch_secs: 5.0 }
    }

    pub fn orders(&self, target: &TargetIncrement, secs_to_close: f32) -> Vec<OrderRequest> {
        let take = match self.posture {
            Posture::Taker => true,
            Posture::Maker => false,
            Posture::Adaptive => secs_to_close <= self.taker_switch_secs,
        };
        target
            .legs
            .iter()
            .map(|leg| OrderRequest {
                side: leg.side,
                shares: leg.shares,
                max_depth: leg.max_depth,
                limit_price: if take { None } else { Some(leg.price_ref) },
                tag: "convex_book",
            })
            .collect()
    }
}
```
Add `pub mod execution;` to `convex/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p pm-strategy convex::execution -v`
Expected: 3 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-strategy/src/convex/execution.rs crates/pm-strategy/src/convex/mod.rs
git commit -m "pm-strategy: convex::execution - maker/taker/adaptive posture into orders"
```

---

### Task 5: ConvexBookStrategy — compose and implement `Strategy`

**Files:**
- Modify: `crates/pm-strategy/src/convex/mod.rs` (the composed strategy)
- Modify: `crates/pm-strategy/src/lib.rs` (re-export)
- Test: `crates/pm-strategy/src/convex/mod.rs` (inline integration test)

- [ ] **Step 1: Write the failing integration test**

In `convex/mod.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Ctx, Side, Strategy};
    use pm_model::ModelOutput;
    use pm_types::{ReplayEvent, ReplayFlags, MarketId, SpotHistory, TradeHistory};

    fn event_at(secs_to_close: f32, yes_ask: f32, yes_bid: f32) -> (ReplayEvent, Ctx) {
        // close at a fixed ns; ts so that (close - ts)/1e9 == secs_to_close
        let close_ns = 1_000_000_000_000i64;
        let ts_ns = close_ns - (secs_to_close as i64) * 1_000_000_000;
        let ev = ReplayEvent {
            ts_ns, market_id: MarketId(0),
            yes_mid: (yes_ask + yes_bid) / 2.0, yes_bid, yes_ask,
            volume: 0.0, bids: Default::default(), asks: Default::default(),
            spot_price: 0.0, flags: ReplayFlags::BOOK_UPDATE,
        };
        let ctx = Ctx {
            market_close_ns: close_ns,
            no_ask: 1.0 - yes_bid, no_bid: 1.0 - yes_ask, no_mid: 1.0 - (yes_ask + yes_bid) / 2.0,
            model_output: Some(ModelOutput {
                direction_score: 0.5, confidence_score: 0.75, calibrated_p: 0.86, risk_score: 0.3,
            }),
            ..Ctx::default()
        };
        (ev, ctx)
    }

    #[test]
    fn loads_favourite_then_tail_over_market_life() {
        let mut s = ConvexBookStrategy::new(ConvexBookConfig::default());
        let spot = SpotHistory::default();
        let trades = TradeHistory::default();

        // Early: no favourite load yet (secs_in < start).
        let (e0, c0) = event_at(200.0, 0.80, 0.79);
        assert!(s.on_event(&e0, &c0, &spot, &trades).orders.is_empty(), "too early");

        // Late: favourite YES loads.
        let (e1, c1) = event_at(100.0, 0.80, 0.79);
        let out1 = s.on_event(&e1, &c1, &spot, &trades);
        assert!(out1.orders.iter().any(|o| o.side == Side::BuyYes), "favourite loads late");

        // Later + extreme skew + cheap NO: tail (BuyNo) appears.
        let (e2, c2) = event_at(60.0, 0.93, 0.92); // NO ask = 1-0.92 = 0.08 (cheap)
        let out2 = s.on_event(&e2, &c2, &spot, &trades);
        assert!(out2.orders.iter().any(|o| o.side == Side::BuyNo), "cheap convex tail appears");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p pm-strategy convex::tests::loads_favourite_then_tail -v`
Expected: FAIL to compile (`ConvexBookStrategy` undefined).

- [ ] **Step 3: Implement the composed strategy in `convex/mod.rs`**

```rust
//! Clean multi-market convex-book strategy: Signal -> PositionManager -> ExecutionPolicy.
pub mod signal;
pub mod position;
pub mod execution;

use crate::{Ctx, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};
use pm_model::ModelOutput;

use signal::{evaluate, SignalGate, Conviction};
use position::{PositionManager, PositionConfig, BothBookPrices};
use execution::{ExecutionPolicy, Posture};

#[derive(Debug, Clone)]
pub struct ConvexBookConfig {
    pub signal: SignalGate,
    pub position: PositionConfig,
    pub posture: Posture,
}
impl Default for ConvexBookConfig {
    fn default() -> Self {
        Self { signal: SignalGate::default(), position: PositionConfig::default(), posture: Posture::Adaptive }
    }
}

/// Per-market convex-book strategy. The engine clones one per market, so the
/// PositionManager's inventory state is naturally isolated.
#[derive(Clone)]
pub struct ConvexBookStrategy {
    signal: SignalGate,
    position: PositionManager,
    execution_posture: Posture,
}

impl ConvexBookStrategy {
    pub fn new(cfg: ConvexBookConfig) -> Self {
        Self {
            signal: cfg.signal,
            position: PositionManager::new(cfg.position),
            execution_posture: cfg.posture,
        }
    }
}

impl Strategy for ConvexBookStrategy {
    fn on_event(&mut self, event: &ReplayEvent, ctx: &Ctx, _spot: &SpotHistory, _trades: &TradeHistory) -> StrategyOutput {
        let secs_to_close = ((ctx.market_close_ns - event.ts_ns) as f64 / 1e9) as f32;
        if secs_to_close < 0.0 {
            return StrategyOutput::hold();
        }
        let prices = BothBookPrices {
            yes_ask: event.yes_ask, yes_bid: event.yes_bid,
            no_ask: ctx.no_ask, no_bid: ctx.no_bid,
        };
        let favourite = if event.yes_mid >= 0.5 { Side::BuyYes } else { Side::BuyNo };
        let fav_ask = prices.ask(favourite);
        let Some(conv): Option<Conviction> = evaluate(ctx, event.yes_mid, fav_ask, &self.signal) else {
            return StrategyOutput::hold();
        };
        let target = self.position.plan(&conv, &prices, secs_to_close);
        if target.legs.is_empty() {
            return StrategyOutput::hold();
        }
        let policy = ExecutionPolicy::new(self.execution_posture);
        StrategyOutput { orders: policy.orders(&target, secs_to_close) }
    }

    fn on_event_scored(&mut self, event: &ReplayEvent, ctx: &Ctx, spot: &SpotHistory, trades: &TradeHistory)
        -> (StrategyOutput, Option<ModelOutput>) {
        (self.on_event(event, ctx, spot, trades), ctx.model_output)
    }
}
```
Note: `PositionManager` must derive/implement `Clone` for `ConvexBookStrategy: Clone` (the engine requires `S: Clone`). Add `#[derive(Clone)]` to `PositionManager` in `position.rs` (all fields are `Copy`/`Clone`). Also make `BothBookPrices::ask` `pub(crate)` so `mod.rs` can call it (or inline the favourite-ask computation in `mod.rs`).

Add to `lib.rs`: `pub use convex::{ConvexBookStrategy, ConvexBookConfig};`

- [ ] **Step 4: Run the integration test + full pm-strategy suite**

Run: `cargo test -p pm-strategy convex 2>&1 | tail -6` then `cargo test -p pm-strategy 2>&1 | tail -3`
Expected: the integration test + all unit tests PASS; existing pm-strategy tests unaffected.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-strategy/src/convex/mod.rs crates/pm-strategy/src/lib.rs crates/pm-strategy/src/convex/position.rs
git commit -m "pm-strategy: ConvexBookStrategy - composed Signal/PositionManager/ExecutionPolicy (Strategy impl)"
```

---

### Task 6: Clippy + workspace build

**Files:** none expected (fix only if needed)

- [ ] **Step 1: Clippy the new module**

Run: `cargo clippy -p pm-strategy --all-targets --no-deps -- -D warnings 2>&1 | grep -E "convex/|warning|error" | head`
Expected: no warnings/errors attributable to `convex/` files. (Pre-existing warnings elsewhere, e.g. `archive/reactive.rs`, are out of scope — do not fix them here.)

- [ ] **Step 2: Build the engine + app (hosting the new strategy compiles)**

Run: `cargo build -p pm-engine --features testkit 2>&1 | tail -3` and `cargo build -p pm-app 2>&1 | tail -3`
Expected: both build (the `Ctx` NO fields + `build_ctx` arg change thread through; `ConvexBookStrategy: Strategy + Clone` satisfies the engine bound).

- [ ] **Step 3: Commit (only if fixes were needed)**

```bash
git add -A && git commit -m "pm-strategy/convex: clippy + build fixes"
```

---

## Self-Review

**Spec coverage (`2026-06-09-multi-market-strategy-design.md`):**
- §5.1 Signal (lift model → conviction) → Task 2. ✓
- §5.2 PositionManager (convex-book accumulator, favourite + cheap tail, coverage sizing, time-evolving, real both-book prices) → Tasks 1 + 3. ✓
- §5.3 ExecutionPolicy (maker/taker/adaptive) → Task 4. ✓
- §5.7 composed strategy implements `Strategy`, hosted per-market → Task 5 (uses Plan 1's hosting). ✓
- §4 "real both-book prices" → Task 1 (Ctx NO top-of-book, replaces synthetic 1-yes). ✓
- Deferred per spec §9 (intentional, not gaps): exact taper/threshold tuning, per-cell calibration (Plan 4), the classifier + pool sizing (Plan 3).

**Placeholder scan:** All code shown; the Step-2 scaffold helper in Task 2 is explicitly replaced in Step 3. No TBD/TODO.

**Type consistency:** `Conviction`/`SignalGate`/`evaluate` (Task 2) consumed by `PositionManager::plan` (Task 3) and `ConvexBookStrategy` (Task 5); `TargetIncrement`/`TargetLeg`/`BothBookPrices` (Task 3) consumed by `ExecutionPolicy::orders` (Task 4) and `mod.rs` (Task 5); `OrderRequest { side, shares, max_depth, limit_price, tag }` matches the real struct; `ModelOutput { direction_score, confidence_score, calibrated_p, risk_score }` matches pm-model. `PositionManager` derives `Clone` so `ConvexBookStrategy: Clone` (engine requirement). ✓

---

## Follow-on
- **Plan 3:** real `(token,window)` classifier + per-cell config (generalize Plan 1's clone-template to a cell-keyed factory) + shared-pool edge-ranked sizing + per-book cap + settle-driven recycling + the reservation-lifecycle fix (so resting orders reserve cash; fixes the over-deployment seen at the end of Plan 1).
- **Plan 4:** data foundation (binance SOL/XRP + June) + multi-cell backtest + portfolio-Sharpe validation vs champion; tune the deferred tapers/thresholds per cell; measure maker/taker posture per cell.
</content>
