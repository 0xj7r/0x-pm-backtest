# Multi-Market Strategy Wiring + Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Run the clean `ConvexBookStrategy` (Plan 2) through the engine on real BTC-5m data as a directional taker, and confirm it produces sane multi-market results (trades scale, equity stays bounded — no over-deployment) — the end-to-end validation of Plans 1+2.

**Architecture:** `ConvexBookStrategy` defaults to `Posture::Taker` (the directional edge beats the spread; spread-capture maker is a different game we are not building — see spec + scratchpad "MAKER-vs-TAKER RESOLVED"). The engine-backtest driver gains a `--strategy {bonereaper_v2|convex}` switch by extracting a generic `run_engine_with<S: Strategy + Clone>(template, cfg, ...)` core (the engine, enricher, per-market trades, market_meta wiring is strategy-agnostic). The new strategy reads `ctx.model_output` (the CtxEnricher already evaluates the per-market model) and prices the convex tail on the real NO book (`ctx.no_*`, Plan 2 Task 1).

**Tech Stack:** Rust; `pm-strategy` (`ConvexBookStrategy`/`ConvexBookConfig`/`Posture`), `pm-app` (`engine_driver.rs`, `main.rs`), `pm-engine`. `cargo test`/`clippy` + a real BTC-5m run.

**Depends on:** Plan 1 (per-market hosting) + Plan 2 (ConvexBookStrategy) — both done. Data: BTC-5m May 21-28 both-legs cache local (present).

**Scope note:** This validates the directional taker on the single BTC-5m cell. The real `(token,window)` classifier + per-cell config (cell-keyed factory) + multi-cell run are **Plan 3b / Plan 4** (gated on the SOL/XRP+June data foundation). The reservation-lifecycle + edge-ranked sizing are deferred (taker does not rest orders; per-book cap + free-cash floor suffice).

---

### Task 1: ConvexBookStrategy defaults to Taker

**Files:** Modify `crates/pm-strategy/src/convex/mod.rs` (`ConvexBookConfig::default`)

- [ ] **Step 1: Write the failing test**

In `convex/mod.rs` tests:
```rust
#[test]
fn default_posture_is_taker() {
    // The directional edge is captured by crossing the spread, not resting (maker
    // spread-capture is a different game). Default must be Taker.
    assert!(matches!(ConvexBookConfig::default().posture, execution::Posture::Taker));
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p pm-strategy convex::tests::default_posture_is_taker -v`
Expected: FAIL (default is currently `Posture::Adaptive`).

- [ ] **Step 3: Change the default**

In `ConvexBookConfig::default`, change `posture: Posture::Adaptive` to `posture: Posture::Taker`. Update the doc comment to note the directional-edge rationale.

- [ ] **Step 4: Run + full convex suite**

Run: `cargo test -p pm-strategy convex 2>&1 | tail -3`
Expected: PASS (the existing tests that asserted maker/adaptive behavior call `ExecutionPolicy::new(Posture::Maker/Adaptive)` directly, so they are unaffected by the default change; only the strategy default moved).

- [ ] **Step 5: Commit**

```bash
git add crates/pm-strategy/src/convex/mod.rs
git commit -m "pm-strategy: ConvexBookStrategy defaults to Taker posture (directional edge, not spread capture)"
```

---

### Task 2: Generic `run_engine_with<S>` + `--strategy` switch

Extract the strategy-agnostic core of `run_engine_backtest` so either strategy can run, and route a `--strategy` flag.

**Files:**
- Modify: `crates/pm-app/src/engine_driver.rs` (extract generic core; `EngineBacktestCfg` gains a `strategy` field)
- Modify: `crates/pm-app/src/main.rs` (`engine-backtest` subcommand gains `--strategy`)

- [ ] **Step 1: Read the current `run_engine_backtest`**

It builds `let strat = BonereaperV2::new(BonereaperV2Config { bankroll_usdc: cfg.starting_cash, max_clip_usdc: cfg.max_clip_usdc, ..Default::default() });` then `Engine::new(strat, Portfolio::new(cfg.starting_cash), RiskGate { limits: engine_risk_limits(cfg.max_clip_usdc) }, classify_btc).with_enricher(...).with_market_meta(...).with_market_prior_ranges(...).with_market_trades(...)`, runs the `SliceFeed`+`SimClock`+`SimExchange`, and tallies the trace into an `EngineBacktestReport`.

- [ ] **Step 2: Add a `strategy` selector to the cfg + extract the generic runner**

Add to `EngineBacktestCfg`:
```rust
    pub strategy: StrategyKind,
```
with
```rust
#[derive(Debug, Clone, Copy)]
pub enum StrategyKind { BonereaperV2, Convex }
```
Extract everything from the `Engine::new(...)` line through the trace-tally + `EngineBacktestReport` build into a generic function:
```rust
fn run_engine_with<S: pm_strategy::Strategy + Clone>(
    template: S,
    cfg: &EngineBacktestCfg,
    store: &TelonexStore,
    pairs: &[(MarketId, MarketHandle, MarketHandle)],
    loaded: Vec<LoadedMarket>,   // or pass the already-built all_events + maps + spot
    // ... the prior_map, meta_map, trades_map, spot, all_events already computed
) -> EngineBacktestReport { /* the existing Engine::new(template, ...)... run ... tally */ }
```
Keep all the discovery/loading/spot/map-building in `run_engine_backtest` (strategy-agnostic), then branch:
```rust
match cfg.strategy {
    StrategyKind::BonereaperV2 => {
        let t = BonereaperV2::new(BonereaperV2Config { bankroll_usdc: cfg.starting_cash, max_clip_usdc: cfg.max_clip_usdc, ..Default::default() });
        run_engine_with(t, &cfg, /* shared inputs */)
    }
    StrategyKind::Convex => {
        let t = ConvexBookStrategy::new(ConvexBookConfig {
            position: PositionConfig { bankroll_usdc: cfg.starting_cash, max_clip_usdc: cfg.max_clip_usdc, ..Default::default() },
            ..Default::default()
        });
        run_engine_with(t, &cfg, /* shared inputs */)
    }
}
```
(Refactor the function boundary however is cleanest so the shared inputs — `all_events`, `prior_map`, `meta_map`, `trades_map`, `spot`, `markets_total` — are computed once and passed into `run_engine_with`. The CtxEnricher is built inside `run_engine_with` since it does not depend on the strategy type.)

Import `pm_strategy::{ConvexBookStrategy, ConvexBookConfig}` and `pm_strategy::convex::position::PositionConfig` (or re-export `PositionConfig` from `convex`/`lib`).

- [ ] **Step 3: Route `--strategy` in main.rs**

Add to the `EngineBacktest` clap variant:
```rust
        /// Strategy to run: bonereaper_v2 (legacy taker) or convex (new directional convex-book).
        #[arg(long, default_value = "convex")]
        strategy: String,
```
In the dispatch arm, parse it:
```rust
            let strategy = match strategy.as_str() {
                "bonereaper_v2" | "br2" => engine_driver::StrategyKind::BonereaperV2,
                "convex" => engine_driver::StrategyKind::Convex,
                other => anyhow::bail!("unknown --strategy {other} (use bonereaper_v2|convex)"),
            };
```
and set `strategy` in the `EngineBacktestCfg { ... }` literal.

- [ ] **Step 4: Build**

Run: `cargo build -p pm-app 2>&1 | tail -3`
Expected: builds. If the generic extraction has borrow/lifetime friction, pass owned values (the maps + `all_events` Vec) into `run_engine_with`.

- [ ] **Step 5: Commit**

```bash
git add crates/pm-app/src/engine_driver.rs crates/pm-app/src/main.rs
git commit -m "pm-app: engine-backtest --strategy {bonereaper_v2|convex} via generic run_engine_with"
```

---

### Task 3: Validate ConvexBookStrategy multi-market on real BTC-5m (reported)

**Files:** none (validation run)

- [ ] **Step 1: Build release + run convex on 2 days**

```bash
cargo build --release -p pm-app
RUST_LOG=error ./target/release/pm-app engine-backtest \
  --local-cache-dir data/cache --start-date 2026-05-21 --end-date 2026-05-22 \
  --meta-calibrator-snapshot-in data/snap062901.json --strategy convex 2>&1 \
  | grep -E "engine-diag|engine backtest:"
```

- [ ] **Step 2: Record + sanity-check**

Expected (sanity, not a P&L verdict — single-cell is noise per the spec):
- `markets_traded` scales with the number of markets (the per-market hosting works for the new strategy too).
- equity stays **bounded** (NOT the -101% over-deployment seen with default br2) — the convex strategy sizes off `max_clip_usdc` + per-book cap + free-cash floor, and taker does not rest, so cash is not over-committed.
- taker fills appear (no `free_cash_too_low` storm; if many free-cash rejects appear, the per-book cap / clip size needs tightening — record it).
Write the numbers (markets_traded, submit/fill counts, final equity, any reject reasons) into the scratchpad run notes. If equity is still badly negative, that is a real finding to chase (capital coordination), not expected — flag it.

---

## Self-Review
- Spec coverage: §5.3 posture (Taker default) → Task 1; §5.7 hosted strategy run end-to-end → Task 2; validation (bounded equity, trades scale) → Task 3. ✓
- Deferred (documented): real `(token,window)` classifier + per-cell config + multi-cell (Plan 3b/4); reservation lifecycle + edge-ranked sizing (not needed for taker). ✓
- Type consistency: `StrategyKind`, `run_engine_with<S: Strategy + Clone>`, `ConvexBookConfig`/`PositionConfig` match Plan 2; `Posture::Taker` matches execution.rs. ✓
- No placeholders (Task 2 Step 2 leaves the exact function-boundary to the implementer's judgment but specifies inputs/outputs precisely; this is a refactor-shape choice, not a missing requirement).

## Follow-on
- **Plan 3b / Plan 4:** real `(token,window)` classifier from market metadata; cell-keyed strategy factory (per-cell `ConvexBookConfig`, incl per-cell `BETTING_WINDOW_SECS`); SOL/XRP+June data ingest; multi-cell recorded Feed; portfolio-Sharpe validation vs champion; per-cell taker-edge-beats-spread report; tune the deferred PositionManager sizing curves.
</content>
