# Design: shared engine-core crate (backtest == live)

Date: 2026-06-08
Status: DECISION LOCKED, detailed design TODO (resume fresh)

## Problem

`polymarket-backtest` (pm-app walk-forward, serial sim) and `polymarket-agent`
(polymarket-exec live runtime) are **two codebases** that share only the
strategy crates (`pm-strategy`: br2/BackToExplore). The execution/order-lifecycle,
fill model, risk layer, and portfolio/capital state are **duplicated and
divergent**. Result: what we backtest is NOT what we trade — the #1 robustness
threat for a quant system, and the likely reason thin edges (br2 +4.3%, one
market −$129) don't transfer.

## Decision (locked 2026-06-08)

Build a **shared engine-core crate** that both repos depend on. One execution
path; two thin drivers. "Backtest is live with a recorded feed and a simulated
exchange." Live constraints (latency, partial fills, order lifecycle,
reconciliation) shape the core; backtest is a faithful simplification, never the
other way around.

## Proposed shape (to refine next session)

Core crate (working name `pm-engine`) owns:
- **Strategy host** — drives `pm-strategy` strategies (already shared).
- **Risk layer** — hard, strategy-independent: per-market + portfolio gross,
  drawdown throttle, and the cross-market **correlated-exposure cap** per
  (token, overlapping window) from the cross-market spec.
- **Portfolio / capital state** — shared equity, per-cell P&L attribution.
- **Order lifecycle** — intent -> risk-check -> submit -> fill -> position update.

Pluggable seams (traits):
- **`Feed`** — yields timestamped market events. Backtest: recorded telonex
  replay (the both-legs data we just ingested). Live: market_ws / spot_ws.
- **`Exchange`/`Fill`** — turns accepted orders into fills. Backtest: fill
  simulator (faithful: queue position, latency, partials, fees — NOT the current
  synthetic NO=1-yes shortcut). Live: signed CLOB submit + reconcile.
- **`Clock`** — sim time (driven by feed) vs wall time.

Drivers (thin):
- **Backtest driver** = core + recorded Feed + sim Exchange + sim Clock
  (replaces pm-app walk-forward's bespoke loop).
- **Live driver** = core + ws Feed + CLOB Exchange + wall Clock
  (replaces polymarket-exec's bespoke runtime loop).

## Migration direction

Extract the core FROM the live-shaped engine (`polymarket-exec/src/runtime` +
`core/`), since live is the stricter shape, into `pm-engine`. Then make pm-app's
walk-forward a Backtest driver of it. Keep `pm-strategy` as-is. Prove equivalence
with a conformance test (same recorded feed -> identical decisions/fills across a
"replay both ways" harness).

## Open design questions for the detailed session

1. Exact trait signatures for `Feed` / `Exchange` / `Clock` and the event enum.
2. Fill-model fidelity tier for backtest (queue/latency/partials/fees) and how to
   calibrate it against real fills (we have both-legs + trades data now).
3. Where the both-real-book pricing lives (replaces NO=1-yes synthesis) — in the
   sim Exchange.
4. Where the cross-market portfolio/allocator + per-cell calibrators sit relative
   to the core (engine layer vs a wrapper).
5. How `polymarket-exec`'s existing live safety (kill switch, reconcile, caps)
   maps onto the core risk layer.
6. Repo topology: `pm-engine` in polymarket-backtest workspace (path-dep'd by the
   agent, like pm-strategy) vs a shared third repo.
7. Determinism + the conformance/equivalence harness design.

## Relationship to other specs

This is the engine substrate UNDER the cross-market framework
(`2026-06-08-cross-market-framework-design.md`). The cross-market routing,
per-cell calibrators, and concurrency all run ON this shared core. Sequence:
finish data ingestion -> build/extract the shared core -> then the cross-market
layer on top.
