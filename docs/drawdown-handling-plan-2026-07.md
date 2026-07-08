# Drawdown handling plan (2026-07-08): survive, don't predict

Answers the recurring question: what is the plan for the consecutive-drawdown
days that nearly wiped the account in June? Backed by an equity simulation
across all 75 validated trading days (Feb+Mar+May+Jun, ungated, truthful
1250ms latency; 72% green, mean +$670/day at $50, worst red streak 4 days,
worst cumulative red run -$2,364).

## Principle: drawdown days are unpredictable, so the plan is survival

Every attempt to PREDICT or AVOID bad days was falsified (regime gates, dwell
gate, position management, all dead across May+June+Feb). Adverse runs are
irreducible variance in a 72%-win strategy. The plan therefore controls the
CONSEQUENCE (drawdown depth) via sizing, not the occurrence.

## Equity simulation results ($850 start, worst-case orderings)

| policy | normal end | adversarial maxDD | p95 shuffle maxDD |
|---|---|---|---|
| flat $50 (June) | RUIN in 8 days (-$6,832) | ruin | ruin |
| 1.0% fractional | $10,836 | -83% (trough $149) | -38% |
| 0.5% fractional | $10,114 | -56% (trough $376) | -26% |
| 0.75% + throttle<80%peak | $10,683 | -54% (trough $389) | -32% |

"Adversarial" = the 8 worst days of 4 months forced consecutively at the
start (worse than any of 1500 random shuffles; astronomically unlikely).
"p95 shuffle" = 95th-percentile worst drawdown over 1500 random day-orderings.

## The plan

1. RUIN IS ALREADY IMPOSSIBLE via reduce-only fractional sizing. Clip scales
   with equity, so the curve asymptotes toward zero and never reaches it: even
   the pathological worst case bottomed at $149. This is the structural fix for
   what killed June (flat $50 = linear path to ruin in 8 days).

2. SIZE AT 0.75%, NOT 1%, for micro-live. The endpoint barely moves (drift
   dominates compounding at these clip sizes) while the tail roughly halves.
   Near-free insurance. Revisit upward only after realization is proven.

3. ADD A REDUCE-ONLY DRAWDOWN THROTTLE: when equity < 80% of its running peak,
   halve the sizing fraction until recovered. A sizing policy, NOT a circuit
   breaker: it never skips a trade (skipping was falsified), only sizes down in
   drawdown. Cuts the adversarial worst case from -83% to -54% at ~1% endpoint
   cost. Implement in the executor's effective_clip_usd (reduce-only invariant
   preserved); goes through the deployment gate like any sizing change.

4. ADD A DRAWDOWN MONITOR (alert, never auto-halt): fire a notification when
   equity crosses -25% from peak. Its purpose is NOT to stop trading (the sim
   proves drawdowns recover; halting locks the loss and misses the rebound).
   Its purpose is to prompt a human check: is this normal variance, or did
   something BREAK (parity breach, feed drift, the June-class silent
   divergence)? That discrimination is the only legitimate intervention.

5. ACCEPT THE RESIDUAL. Even the best policy has ~30% drawdowns at the 1-in-20
   level and 4-day red streaks. These are intrinsic to a 72%-win edge and are
   NOT signals to act. Expectations set now so a -25% week is not a panic.

## Explicitly rejected (proven harmful)

- P&L circuit breakers / auto-halt on loss: lock in the loss, miss recovery.
- Entry gates that skip "bad" days: bad days are not forecastable; every such
  gate lost money out of sample.
- Increasing size to "make it back": the opposite of reduce-only; ruin path.

## Implementation status

- Reduce-only fractional sizing: DONE (executor effective_clip_usd).
- 0.75% default + throttle: sizing-policy change, to implement before micro-live
  through the deployment gate (executor + a unit test on the throttle math).
- Drawdown monitor: to add to the ops layer (alert on -25% from peak).
- These are the pre-micro-live sizing tasks; none change decide_entry / GATE B.
