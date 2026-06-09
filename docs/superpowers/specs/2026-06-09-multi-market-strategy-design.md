# Multi-Market Bonereaper Strategy — Design

Status: design (approved in brainstorm 2026-06-09)
Repo: polymarket-backtest (strategy + engine); shared by polymarket-agent live runtime
Supersedes the single-market `BonereaperV2` taker implementation as the strategy of record.

## 1. Motivation

Three problems converged:

1. **`BonereaperV2` is convoluted.** It is a single-market strategy carrying ~35 mutable per-market state fields, 150+ config knobs, and seven overlapping, time-gated lanes (early / mid / late / late-favourite / high-skew / tail / participation / hedged) that mutate shared counters. None of the per-market counters reset, so hosting one instance across many markets exhausts the fire budgets after ~3 markets and trading silently stops.
2. **Backtest ≠ live.** The backtest validates `bonereaper_v2` (a selective taker); the live `polymarket-exec` runtime runs `bonereaper_mm` (`POST_ONLY=true`). We maintain and validate two divergent, convoluted strategies. This is precisely the discontinuity the shared engine (`pm-engine`) was built to remove.
3. **The edge is breadth, not single-cell alpha.** The real Bonereaper wallet (`0xeebd…ba30`, per Polyanna) shows ROI +1.2% but Sharpe 5.50 — the Sharpe is a diversification artifact across 53,012 markets, 4 tokens, every duration, with many concurrent positions and ~80 trades/market. A single cell (e.g. BTC-5m) is a near-zero, high-variance line; optimizing it overfits noise. Validation must be portfolio-level.

We are not discarding the validated edge — only the implementation. The signal (the Black–Scholes binary-digital model + meta-calibrator, fed by Binance signed-flow / adverse-vol / regime features) was the +EV part (AUC 0.60–0.64; +EV Feb–Apr and on the May OOS). We lift it and rebuild everything around it as a clean, multi-market-native strategy that the backtest and live drivers share.

## 2. Goals & Non-Goals

**Goals**
- One clean strategy core, decomposed into three independently-testable units, hosted on `pm-engine`, run identically by the backtest driver and the live driver (only Feed/Exchange/Clock differ).
- Faithfully reproduce the Bonereaper position structure: a directionally-tilted, roughly share-balanced two-sided convex book (favourite + cheap convex tail), built incrementally over a market's life.
- Signal-led market selection and sizing across all cells (token × duration), with a dumb-but-safe shared-pool accounting layer.
- Make the maker-vs-taker execution posture a measured/adaptive policy, not a hardcoded identity.
- Fix the two engine bugs that block any multi-market run (per-market strategy state; exposure release on settlement).

**Non-Goals (YAGNI)**
- No two-sided spread-capture market making (the two-sidedness is convex hedging, not quoting for the spread).
- No two-level capital allocator in v1 (strategic per-cell budgets). Add only if backtests show cells need budgeting.
- No new signal/model research. The model is lifted and re-calibrated per cell, not redesigned.
- No live deployment in this spec. That is the follow-on live-driver work.

## 3. What we keep vs replace

**Keep**
- `pm-engine` — the multi-market substrate (one ts-ordered event stream, shared capital, risk gate, both-book fill model). Sound.
- The faithful both-book fill model. The convex book (cheap tail, pair-sum-under-$1 opportunities) is a property of the *real* NO ladder; it cannot be constructed or backtested on synthetic NO = 1 − yes. The engine work directly enables this strategy.
- The validated model+edge core, lifted as the Signal unit.
- The champion `bonereaper_v2_favourite_062901` config as a documented **baseline to beat**.

**Replace**
- The seven-lane single-market `BonereaperV2` execution/position logic, with the three clean units below.

## 4. Strategy shape

Per market, the strategy manages a **directionally-tilted, roughly share-balanced two-sided convex book**:

- **Favourite leg** — the high-conviction directional side, bought at its (higher) price.
- **Convex tail leg** — the cheap opposite side, bought for convexity and downside-cap. Because it is cheap, it buys many shares per dollar; the book is roughly *share*-balanced even though it is dollar-tilted toward the favourite.

Roughly-balanced shares means resolution pays ≈ $1 × shares whichever side wins, which **limits losses** (you are hedged, never blown out); the cheap tail adds **convex upside** if the unlikely side hits. Net payoff profile: small gain when the favourite wins as expected, large gain on a tail surprise, capped loss. (Concrete observed example: BTC 4h book DOWN 2,137sh @ 84¢ + UP 2,571sh @ 12¢ → +$24 if DOWN, +$457 if UP.) The edge over many cells is mostly small gains plus occasional convex tail wins, rarely a real loss → the high Sharpe.

**The book is time-evolving, not one-shot.** Caught at different life-stages:
- *Early/mid:* load near the mid, roughly balanced, accumulating while the signal is ambiguous.
- *Late:* as conviction firms and time-to-close shrinks, build the directional tilt on the favourite and add the cheap convex tail (the book matures into the wide favourite/cheap-tail shape).

So position construction is a **stateful, time-aware accumulator**: each tick it takes `(conviction, time-to-close, current inventory/tilt, both-leg prices)` and decides the next increment — load mid / tilt favourite / add cheap tail / hold. Time-to-close is a first-class input (the recent open/close timing bug was destructive precisely because it broke this axis).

**Selection and sizing are signal-led.** Every live market in every cell is scored each tick; the strategy acts only where conviction/edge clears a threshold (no edge → skip — this is why some windows are SOL-only). Size *and* structure scale with signal strength: in one observed 8:00–8:05 window, BTC (UP 80.8¢, ~$3.2K, share-balanced, cheap 13.8¢ tail) got ~13× SOL's capital and a tighter favourite / cheaper tail because its signal was strongest, while ETH and SOL got small, less-tilted books. There is no top-down "trade token X now" — the per-cell signal selects and scales.

## 5. Architecture

Three clean units composed per market, hosted by the engine:

```
                 per-market instance (one per live market, keyed by cell)
   EngineEvent ─▶ Signal ──conviction/edge──▶ PositionManager ──target book──▶ ExecutionPolicy ──orders──▶ engine/Exchange
                 (lifted model)               (stateful accumulator)          (maker/taker/adaptive)
                                                       ▲                                                   │
   shared portfolio cash pool + per-book cap + settle-driven recycling ◀── fills/settle ──────────────────┘
```

### 5.1 Signal (lifted, generalized)
- **Does:** consumes the per-market event stream + spot/trade history and produces a per-tick **conviction**: favoured side, calibrated probability, edge vs current price, confidence, risk.
- **Interface:** `score(event, ctx, spot, trades) -> Conviction`.
- **Depends on:** the lifted `pm-model` core (Black–Scholes binary digital + meta-calibrator + signed-flow/regime features), a **per-cell calibrated snapshot**, and **per-market-scoped rolling state** (recent mids/dir/imbalance), reset per market by construction (the cross-market pollution bug must be impossible, not merely avoided).
- **Change from today:** none to the model maths; only (a) per-cell snapshot selection, (b) per-market state isolation as a contract.

### 5.2 PositionManager (new, clean)
- **Does:** turns evolving conviction + both-leg prices + current inventory + time-to-close into the next **target increment** of the convex book (favourite side/size, tail side/size, tilt), implementing the time-evolution (mid-load → tilt + cheap tail) and the share-balance/downside-cap/convexity policy.
- **Interface:** `plan(conviction, prices_both_legs, inventory, time_to_close) -> TargetIncrement`.
- **Depends on:** the real both-book prices (favourite ask + tail ask) and its own per-market inventory/phase state.
- **Sizing policy:** roughly share-balanced, dollar-tilted to the favourite by conviction; tail sized to a downside-coverage / convexity target; total per-book size scales with edge/conviction, subject to the per-book cap from the portfolio layer. Parameters are per-cell-tunable; defaults seeded from the champion's `late_favourite` + `convex tail` settings.

### 5.3 ExecutionPolicy (new)
- **Does:** realizes a `TargetIncrement` into concrete orders, choosing the **posture** — post-only maker, taker, or adaptive.
- **Interface:** `orders(target, prices, time_to_close) -> Vec<OrderRequest>`.
- **Posture is measured, not assumed.** The engine already models both maker fills (real trade-tape + queue priority: a resting order fills only when a trade crosses its level *after* submit — capturing adverse selection / "the bigger boys are ahead") and taker fills (real opposing-ladder VWAP + spread/fee leak). The backtest reports, per cell, maker fill-rate + adverse selection vs taker leak vs net edge. Likely outcome is **adaptive**: rest post-only while there is time-to-close to wait for a good fill, escalate to taker as the window closes or conviction spikes and the fill must happen. Posture is a swappable, A/B-able policy.

### 5.4 Per-market hosting model (engine change)
- The engine stops holding one shared `strategy: S`. Instead it builds a **fresh per-market strategy instance from a factory**, keyed by the market's `(token, duration)` cell so each instance gets its cell's config/calibration.
- This **structurally fixes** the "stops after ~3 markets" bug: each market's Signal/PositionManager/ExecutionPolicy state is isolated by construction.
- Concretely: `Engine` takes a `fn(MarketId, Cell) -> MarketStrategy` (or equivalent factory/clone-from-template); `on_market` routes events to the current market's instance; `on_market_resolved` resolves and may drop that instance (bounding memory). Phase-1 mock strategies adapt trivially (each market gets its own).

### 5.5 Portfolio pool & accounting (Approach (a): dumb-but-safe)
- **One shared cash pool.** Each book draws share-count proportional to its edge/conviction, capped per book.
- **When many markets are eligible at once** (e.g. an 8:00–8:05 window has BTC+ETH+SOL+XRP 5m open, plus overlapping 15m/4h), they compete by edge — highest-edge books fill first if cash is tight; the per-book cap stops any one market hogging the pool.
- **Recycling is the throughput mechanism.** 5m markets resolve every few minutes; on settle, cash returns to the pool and immediately redeploys into the next windows. (The real wallet runs ~$82M volume on ~$85K peak deployed ≈ 1000× recycling.) The engine's settle → cash credit → available-for-next-market path provides this once the settle/exposure bugs are fixed.
- **Backstop:** the risk gate's free-cash floor prevents over-deployment. The correlated-exposure cap stays available but off by default.
- The allocator stays intentionally simple; the intelligence is in the per-cell signal. Add per-cell strategic budgets (Approach (b)) only if backtests show specific cells need it.

### 5.6 Per-cell config & classifier
- A real `(token, window) -> Cell` classifier derived from market metadata replaces the engine's hardcoded `|_| (Btc, 0)`. It also drives the correlated-exposure key (overlapping-window bucketing).
- Each cell carries: its calibrated model snapshot, PositionManager params, and ExecutionPolicy params. The factory (5.4) selects per market.
- Cell taxonomy must be re-derived from the fresh markets master — Polyanna confirms **1h windows exist**, contradicting the earlier 5m/15m/4h-only catalog.

### 5.7 Shared backtest/live contract
- The composed per-market strategy and the engine are the **single shared core**. The backtest driver supplies a recorded both-legs Feed + sim Exchange + sim Clock; the live driver supplies a ws Feed + CLOB Exchange + wall Clock. The strategy object is identical in both. This closes "backtest ≠ trade" by construction and retires the `bonereaper_v2` vs `bonereaper_mm` split.

## 6. Required engine fixes (in scope)
1. **Per-market strategy state** (5.4) — the hosting-model change. Fixes the multi-market "stops after ~3 markets" bug.
2. **Release exposure on settlement** — today `ExposureState` only grows (fills add, settle never releases), so the correlated-exposure cap is unusable across markets. Settlement must release the resolved market's exposure contribution. Harmless today (cap off, current strategy ignores the field) but required before the cap is enabled cross-market.

## 7. Validation
- **Objective metric: portfolio Sharpe across the full cell universe with concurrent positions** — never single-cell P&L (it is noise; optimizing it overfits).
- **Baseline to beat:** the champion `062901` on faithful both-book backtests, plus the documented real-wallet profile (thin ROI, high Sharpe from breadth) as the shape to approach.
- **Determinism gate** (already in place for the engine) extends to the new strategy: identical trace across repeated runs.
- **Posture report:** per cell, maker fill-rate + adverse selection vs taker leak vs net edge, to choose/justify the execution policy empirically.

## 8. Prerequisites (data foundation — separate effort, gates full validation)
- PM book + trades for the full universe and a fresh markets master are already in the S3 mirror (through Jun 7). Usable.
- **Binance spot gap:** mirror has only BTCUSDT + ETHUSDT, through May 28. Cross-market needs per-token signed-flow for BTC/ETH/SOL/XRP including June — ingest from data.binance.vision (confirm `is_buyer_maker` schema), or switch the model's price/flow feed to Telonex Chainlink `crypto_prices` (btc/eth/sol/xrp from Apr 2). In-region per the no-laptop constraint.
- **Per-cell calibration:** train a model snapshot per `(token, duration)` cell against faithful both-book backtests.
- **Re-catalog durations** (incl. 1h) from the fresh master; build the classifier.

## 9. Open questions / deferred
- Exact PositionManager sizing functions (share-balance target, tail coverage/convexity target, conviction→size curve) — seed from the champion, then tune per cell on faithful backtests.
- ExecutionPolicy escalation thresholds (when adaptive flips maker→taker) — measured per cell.
- Two-level allocator (Approach (b)) — deferred unless backtests show cells need budgeting.
- Memory bound for per-market instances over long windows — drop instances on settle; revisit if needed.

## 10. Build sequence (high-level; detailed plan via writing-plans)
1. Engine: per-market strategy hosting model + exposure-release-on-settle (unblocks any multi-market run; validate determinism + the multi-market "stops after 3" bug is gone).
2. Strategy core: define the three unit boundaries (`Signal`, `PositionManager`, `ExecutionPolicy`) + the composed per-market `MarketStrategy`; lift the model into `Signal`; implement `PositionManager` (convex book accumulator) and `ExecutionPolicy` (posture policy) on synthetic fixtures.
3. Classifier + per-cell config plumbing; portfolio pool sizing (edge-rank + per-book cap + recycling).
4. Backtest driver wiring; BTC-5m as the engine *fidelity* test cell; then multi-cell once data foundation lands.
5. Portfolio-level validation (Sharpe across cells) vs the champion baseline; posture report; tune per-cell.
6. (Follow-on spec) live driver + clean-slate retirement of `bonereaper_v2`/`bonereaper_mm`.
