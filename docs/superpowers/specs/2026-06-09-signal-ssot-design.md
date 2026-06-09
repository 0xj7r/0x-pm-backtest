# Signal SSOT and Exogenous Alpha Hunt: Design

**Date:** 2026-06-09
**Status:** Approved design (brainstorming complete; implementation plan pending)
**Repo:** `polymarket-backtest` (canonical)

## 1. Problem

Signal and probability-model logic is forked across four repos with no single source of truth:

- `polymarket-agent` (`polymarket-exec/src/signals/`) holds a clean exogenous BSM fair-value model (`fair_value.rs`) plus a live microstructure stack, none of which exists in the backtest.
- `polymarket-backtest` (`pm-model`) holds a sophisticated but price-anchored ML model (`ModelState` + `OnlineMetaCalibrator`).
- `polymarket-research` (worktree `elastic-germain-59fba2`) holds a second BSM (`backtesting/strategies.py:_fair_value`) and a Python feature library.
- `polymarket-dashboard` is read-only.

Two consequences. First, drift: the agent and backtest can (and do) compute different beliefs from the same data, so a backtest result does not predict live behaviour. Second, and worse, the model we actually backtested has no real directional edge because its probability is price-anchored: `calibrated_p = 0.7 * model + 0.3 * yes_mid` (`pm-model/src/lib.rs:2705`, `pm-strategy/src/signals.rs:258`), and the meta-calibrator also takes book price as features (`side_market_price`, `market_mid`). A probability that tracks the price cannot detect a mispricing, so betting it just pays the spread on every trade. This is the structurally-EV-negative pattern that lost ~$1000 live.

## 2. Goal

Build a canonical, leakage-free signal-and-edge-model layer in `polymarket-backtest`, and a disciplined harness to hunt for real exogenous alpha and prove (or disprove) that it beats the market net of costs at our real latency.

## 3. Locked decisions (from brainstorming)

1. **SSOT boundary: signals + edge model.** The canonical layer owns the signal/feature library and the probability/fair-value model that turns signals into "our belief." Strategy, sizing, posture, and execution stay per-environment. Both backtest and live consume the canonical layer via thin adapters.
2. **Greenfield signals, reuse the calibrator.** Build the signal/feature side fresh with a strict contract. Reuse the existing calibrator machinery (Beta + Isotonic + gradient-boosted-tree ensemble) as the model layer, fed de-anchored inputs.
3. **Strict exogenous probability.** The probability estimate is a pure function of exogenous data (CEX spot, realized/implied vol, time, underlying order flow). Book price never enters the probability, not even as a calibrator feature. The calibrator maps the raw exogenous score to a well-calibrated probability against realized outcomes only. Price is used solely downstream as the bet cost: `edge = p_exo - price`.
4. **Latency is modeled, not assumed away.** Our live latency is already strong (Rust, AWS near exchange datacenters, raw WS). The harness fills at our real measured round-trip, not instantly, and reports the edge-vs-latency curve so our safety margin is always visible.
5. **Label off the oracle, feature off the CEX.** Markets resolve on a Chainlink aggregated price, not Binance. The outcome label and the strike are defined off the oracle; the leading signal comes from CEX spot. The predictive object is "oracle value at window close, given CEX spot now," priced faster and better than the Polymarket book.

## 4. Architecture

### 4.1 Canonical crate: `pm-alpha`

A new crate in the workspace. Name `pm-alpha` chosen to distinguish it from the legacy `pm-model` and the `pm-strategy::signals` module, both of which it supersedes (and both of which stay in place as the baseline to beat until `pm-alpha` proves out). Modules:

- `feature_state` — the typed input snapshot.
- `contract` — the `Signal` trait and `Provenance` tag.
- `signals/` — the catalogue (one file per family).
- `model` — the edge model (BSM base + calibrator).
- `harness` — the offline validation/replay rig.

### 4.2 `FeatureState` (inputs)

A typed snapshot passed to every signal. Fields:

- `cex` — per-exchange spot history (Binance, Coinbase, others as available): recent trades/prices with timestamps, enough lookback for the longest momentum/vol window.
- `oracle` — Chainlink reference: current oracle value and update history.
- `market` — metadata: token (BTC/ETH/SOL/XRP), window length (5m/15m), window open and close timestamps, strike (oracle value at open).
- `clock` — current replay/live timestamp (so time-remaining is derivable).
- `book` — Polymarket book snapshot (best bid/ask, depth). Present in the state but tagged for downstream use only; the leakage rule (below) keeps it out of the belief.

The same `FeatureState` is produced by a replay adapter (backtest) and a streaming adapter (live), so signals are computed by identical code in both. The adapters live outside `pm-alpha` (in `pm-app` for replay, in the agent for live).

### 4.3 The `Signal` contract and `Provenance` (leakage enforcement)

Every signal is a pure function over `FeatureState`. Each carries a `Provenance`:

```
enum Provenance { Exogenous, PriceAnchored }
```

The edge model accepts only `Exogenous` signals into the probability estimate. This is enforced at the type level (e.g. exogenous and price-anchored signals are distinct types, or the model's input constructor only accepts a collection of `Exogenous`-tagged signals), not by convention. Price-anchored signals (book imbalance, microprice, spread) are still allowed to exist because they are useful downstream for cost/edge and gating, but they physically cannot enter the belief.

### 4.4 The edge model (BSM base + reused calibrator)

- **Base.** The exogenous fair value: `P(oracle_close > strike)` from CEX spot, strike, realized vol, and time remaining (the BSM / log-normal core), optionally with a momentum drift term. Ported clean from `polymarket-agent/polymarket-exec/src/signals/fair_value.rs` and cross-checked against `polymarket-research` worktree `backtesting/strategies.py:_fair_value`.
- **Calibration (added after base edge is shown, not before).** Lift the calibrator machinery (`OnlineMetaCalibrator`, `pm-model/src/lib.rs:1009`: Beta + Isotonic + GBT) into `pm-alpha`, instantiated with an exogenous-only feature vector and trained against realized oracle outcomes. No price features. The calibrator refines the base probability's calibration; it does not introduce price.

Per the hunt protocol, the first measurement uses the base alone (no calibrator) so we get a clean read on raw exogenous predictiveness; calibration is a layer we add only once the base clears the bar.

### 4.5 The validation harness

An offline replay over historical data that runs any signal-or-model and reports edge-vs-book. It must model reality or it is worthless:

- **Latency.** Configurable entry latency. We observe a CEX move at time T but fill at T + latency against the book as it actually was then. Default to our real measured round-trip; sweep it to produce the edge-vs-latency curve.
- **Fills and costs.** Cross the spread (fill at the prevailing ask/bid, not the mid), apply taker fees, apply thin-book slippage.
- **Labels.** Resolve each market on the Chainlink oracle outcome.
- **Outputs.** Per token×window cell and in aggregate: log-loss of our probability vs the book-implied baseline, net-of-cost EV, hit rate (reported but not the bar), and the edge-vs-latency curve.

## 5. The signal catalogue (hunt targets)

All exogenous unless marked. Each entry is a hypothesis to test, not an assumed winner.

1. **Oracle-fair-value dislocation (base).** `P(oracle_close > strike)` from CEX spot, strike, vol, time remaining. Edge = fair − token price. The spine; everything else feeds it or gates it.
2. **CEX momentum / drift.** Multi-window spot returns (10s/30s/60s/...) and acceleration, as a drift term on the fair value. Hypothesis: an ongoing move persists long enough for the oracle to follow before the book reprices. Moderately latency-tolerant.
3. **CEX order-flow / microstructure.** Signed taker delta (cumulative delta), bid/ask imbalance, large-trade footprints on Binance/Coinbase; funding-rate and perp-OI shifts; cross-exchange premium (Binance vs Coinbase). Leads spot by seconds. Most latency-sensitive; playable because our execution is fast.
4. **Cross-asset leadership.** When BTC's window is strongly directional, correlated ETH/SOL/XRP windows often have not repriced; BTC spot informs the alt's fair value. Plausibly the most latency-tolerant edge.
5. **Volatility / regime gate (not direction).** Realized vol, ATR, z-score of recent moves. Stand down in low-vol chop (spread dominates) and extreme whipsaw (oracle path too uncertain). Sizes and gates; does not predict.

## 6. The hunt protocol

Designed to stop us fooling ourselves while testing many signals:

1. Run the base (oracle-fair-value) alone. Measure OOS edge-vs-book net of costs at our real latency.
2. Add one family at a time, greedily. Keep a family only if it improves net-of-cost OOS EV on a held-out later period, evaluated per token×window cell.
3. Time-series splits only: train on earlier weeks, test on later, walk forward. Never shuffle ticks (adjacent ticks of one market leak).
4. Keep one final untouched slice of June that nothing is fitted or selected on, as the last honest check against overfitting from multiple testing.
5. Deliverable: a ranked answer. Which exogenous signals, combined how, clear costs at our latency, in which cells.

## 7. Success criteria (the bar)

A signal/model is "real" only if, on the held-out later period and surviving the final untouched holdout:

- its log-loss beats the book-implied baseline's log-loss, AND
- simulated net-of-cost EV is positive, per token×window cell (not only in aggregate).

If nothing clears this bar, that is a valid and valuable result: we stop before wiring another EV-negative strategy, rather than after.

## 8. Data

- **CEX spot:** Binance ingested for BTC/ETH/SOL/XRP (May, plus June 1-8). Add Coinbase (and others) as the microstructure family demands. Canonical store on AWS.
- **Oracle:** Chainlink resolution values and per-window strikes are required for labels and the base signal. This is a data dependency to confirm/ingest (see Open Questions).
- **Polymarket:** book snapshots and outcomes (already used by the engine).
- **Compute:** heavy backtests and sweeps run on AWS Fargate, sharded per date/market, not on the user's laptop.

## 9. Component / file structure

```
crates/pm-alpha/
  src/
    lib.rs              # crate root, re-exports
    feature_state.rs    # FeatureState + sub-structs
    contract.rs         # Signal trait, Provenance, model input constructor (exogenous-only)
    signals/
      mod.rs
      fair_value.rs     # base: oracle-fair-value dislocation (BSM/log-normal)
      momentum.rs       # CEX momentum / drift
      microstructure.rs # CEX order-flow (delta, imbalance, footprints, funding, OI, premium)
      cross_asset.rs    # BTC -> alt leadership
      regime.rs         # vol / regime gate
    model/
      mod.rs            # edge model: base + (later) calibrator
      calibrator.rs     # lifted Beta/Isotonic/GBT, exogenous-only features
    harness/
      mod.rs            # replay, latency model, fills/costs, metrics, latency curve
```

Replay/streaming adapters that build `FeatureState` live outside the crate (`pm-app` for replay, agent for live).

## 10. Testing strategy

- **Per-signal unit tests:** pure functions, deterministic; known input -> known output for each signal.
- **Leakage test:** assert the model's input constructor rejects `PriceAnchored` signals (compile-time where possible, otherwise a test).
- **Harness determinism test:** identical inputs produce identical metrics across runs.
- **Latency-curve test:** edge is monotonically non-increasing as latency rises on a synthetic dislocation (sanity that the latency model bites).
- **Cost test:** zero-edge input yields negative net-of-cost EV (costs are actually applied).

## 11. Phasing (for the implementation plan)

1. Crate skeleton + `FeatureState` + `Signal`/`Provenance` contract + leakage test.
2. Validation harness (latency, fills, costs, metrics, latency curve) with a trivial signal to exercise it.
3. Base signal (oracle-fair-value), ported and tested; first OOS edge-vs-book measurement.
4. Catalogue families one at a time, each gated by the hunt protocol.
5. Calibrator (exogenous-only) added if/when the base clears the bar.
6. Report and decision: which signals are real, in which cells, at our latency.

## 12. Boundaries, reuse, and relationship to existing code

- **Non-goals:** no live execution shell, strategy, or posture decisions here (per-environment). No sub-300ms infra work (already handled). We reuse the calibrator, we do not rebuild it.
- **Legacy stays as baseline:** `pm-model` and `pm-strategy::signals` are untouched and serve as the baseline to beat. The convex strategy continues to read `pm-model`'s `calibrated_p` until `pm-alpha` proves out and we re-point it.
- **Ports:** BSM from agent `fair_value.rs` (+ research worktree); calibrator from `pm-model/src/lib.rs`; exogenous spot-momentum math from `pm-model` `spot_score_stack` / `pm-strategy/src/archive/spot_momentum.rs`.
- **Later consumption:** once proven, the agent consumes `pm-alpha` for its belief, killing the cross-repo drift; the convex strategy's `side_p` is re-pointed to `pm-alpha`.

## 13. Open questions / risks

- **Oracle data:** do we have Chainlink resolution values and per-window strikes in a usable form, or must we ingest them? Labels depend on this. (If unavailable, an interim proxy is the CEX aggregate at close, but the bar must be re-checked against true oracle resolution before any live use.)
- **Microstructure data depth:** funding/OI/cross-exchange premium need extra feeds beyond spot trades; scope per family as we reach it.
- **Overfitting:** many signals tested means real multiple-testing risk; the final untouched holdout is the guard, and we report how many configurations were tried.
- **Edge erosion:** documented edges are narrow and competitive; even a validated edge may decay, so the harness and bar are permanent infrastructure, not a one-time gate.
