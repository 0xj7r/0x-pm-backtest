# Micro-live runbook (turnkey; execute only on user go)

The go/no-go decision is the USER's alone (~Jul 14-15). This document makes
the execution mechanical once they say go, so there is no scrambling. Nothing
here is armed until the user explicitly authorizes it.

## Preconditions (all must hold before presenting the go-decision)

1. v1 gate verdict written into docs/stability-gate-preregistration-2026-07.md
   (pass -> frozen config gains the gate; fail -> ungated stays). This fixes
   WHICH config goes live. Do not proceed without a written verdict.
2. 48h combined paper soak of the exact live stack (winning config + consensus
   + chosen engine stream + fractional sizing) shows: heartbeats healthy,
   entries > 0, zero executor orphans/mismatches, realization ratio computed.
3. Realization ratio of the chosen stream >= 0.82 (break-even) on the soak
   window; ideally >= 0.85. If below, DO NOT go live; the gap is execution,
   fix it first (see "if realization is low").
4. Which engine stream drives the executor is decided from the soak's
   realization comparison: fast_live (100ms) if its realization beats the
   timer twin's, else pm-shadow-final.

## The go configuration (frozen values, updated 2026-07-08 after TUNE + GLM review)

- Capital: $850 venue cash. Sizing PM_SHADOW_CLIP_FRAC=0.005 (0.5% = ~$4.25/clip)
  after the clustered-drawdown analysis (0.5% halves the tail vs 0.75/1%;
  clustered floor-band breach at $850 is ~12%, so buy the buffer). Ceiling
  scaling ON: PM_SHADOW_CLIP_CEIL_FRAC=0.012 (keeps sizing fractional as the
  account grows, fixes the flat-band). Reduce-only. NEVER flat $50 (June ruin).
- Engine/driver: fast_live (event-driven, 100ms) drives the executor. Proven
  live to halve the latency bleed vs the 1s timer on identical trades.
- Config: UNGATED frozen fade is the lean (profitable all 4 TUNE months;
  v1 gate cost is regime-variable -6% to -34%, worst in April). FINAL gate
  on/off decided by Friday's v1 verdict on the soak realization evidence.
- Consensus: executor consumes shadow-consensus (twin-agreed entries only) as
  the phantom filter. This is the CHEAP risk control (~3-6% of entries dropped,
  the unstable near-threshold ones) vs the gate's -6/-34%. Preferred over the
  v1 gate unless the soak shows the gate clearly pays its cost.
- Duration: 14 calendar days, NO config/threshold changes mid-window.
- Hard stop: equity <= $550 (5-share-floor viability line) => stop and reassess,
  add capital or pause. Mechanical, not a P&L breaker. Monitored by
  scripts/ops/drawdown_monitor.py (alert at -25% and $550). Never START below $700.
- Kill: `touch ~/live.kill` (proven kill-switch); disarms both live+paper.
- NO P&L circuit breakers. Sizing + the mechanical floor are the only controls.

## Arming sequence (only after user go)

1. Confirm fade.kill / live.kill present, then user authorizes removal.
2. Set env on the executor unit: PM_SHADOW_PAPER_MODE=false,
   PM_SHADOW_LIVE_TRADE=true, PM_SHADOW_CLIP_FRAC=0.005,
   PM_SHADOW_CLIP_CEIL_FRAC=0.012, caps configured
   (max_order_notional, max_market_notional). Verify LiveArm::from_env()
   reports armed in the banner.
3. Wallet funded to ~$850 USDC on Polymarket; confirm balance cache reads it.
4. Remove the kill file. Watch the FIRST five entries live trade-by-trade:
   confirm side + size + fill match the shadow decision (decision parity is
   the June lesson - a live path that diverges is the failure mode).
5. Daily: realization report, per-day P&L, drawdown vs $850. Weekly review.

## Success / stop criteria (pre-registered now)

- SUCCESS after 14 days: realization ratio >= 0.85 aggregate, no single day
  worse than -5% of bankroll, live-vs-shadow side agreement >= 90%. -> scale
  sizing with bankroll; stand up 15m book live; discuss capital top-up.
- STOP: realization < 0.82 aggregate, OR any decision-parity breach (live
  side != shadow side > 10% of entries), OR a day worse than -8% bankroll.
  Kill, redeem, capital intact, diagnose before any retry.

## If realization is low (the likely failure mode)

The gap is fills, not signal (backtest is validated across regimes). Levers,
in order: (a) confirm fast_live is the driver, not the 1s timer; (b) event-
driven decide (dirty-flag on book/spot events, removes remaining timer phase);
(c) marketable-limit aggressiveness / repricing; (d) accept the strategy is
not capturable at our latency and stop. Each is a measured change through the
deployment gate, never a live knob-twiddle.

## Expected economics (set expectations, do not anchor)

Regime-weighted ~+$405/day at $50 telemetry scale (docs/chop-is-not-the-enemy).
At 1% of $850 that is ~$40-70/day BEFORE the realization haircut, i.e.
plausibly $15-50/day net live if realization lands 0.5-0.85. Small in dollars;
the point of micro-live is to PROVE the realization number at trivial risk
before scaling. Worst-case 14-day loss at this sizing is tens of dollars.
