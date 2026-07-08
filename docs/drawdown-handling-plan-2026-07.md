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

## The share-quantum floor: a HARD viability line (2026-07-08 correction)

Polymarket's CLOB enforces a 5-share minimum order (confirmed in code: the
venue rejects smaller orders with "Size lower than the minimum: 5";
polymarket-exec markets/descriptor.rs min_order_size=5.0). This BREAKS the
"clips shrink smoothly to zero" claim: there is a hard floor on clip size of
5 x entry_price dollars (~$2.25 at our 0.45 min ask, ~$2.95 median, ~$4.25 at
the 0.85 cap). Below the equity where 0.75% < that floor, fractional sizing
stops working and you are forced to flat-bet the ~$3 minimum, which reintro-
duces linear ruin (the June mechanism) at a low level.

Floor bites at equity = 5 x price / 0.0075 = 667 x price:
- cheapest entries (0.45): $300 | median (0.59): $393 | max (0.85): $567

Consequences (equity sim with the floor, 3000 shuffles, no haircut):
- $850 start: 0/3000 ruin, 0% of paths even reach the <$400 zone. SAFE.
- $600 start: 0 ruin, 2% touch the zone. $400: 36%. $300: 100%.
- $200 start: 5/3000 ruin. $150: 31/3000 (1%). Small accounts are NOT viable.
- Adversarial worst-8-first: survives from $850 (trough $203), RUINS from $300.

GOVERNANCE RULE: this strategy is viable at ~$850 with a ~35% buffer to the
floor. Treat ~$550 as a HARD stop-and-reassess line (the top of the
degradation band, where pricier entries first hit the floor). At/below it,
STOP and add capital or pause; do not grind on at forced-minimum bets. This
is a mechanical boundary (the sizing math breaks), NOT a P&L circuit breaker,
and it replaces the vague "accept 30% drawdowns" with a concrete floor.
Corollary: never start a live account below ~$600.

## GLM adversarial review corrections (2026-07-08, reviews/drawdown-glm-review.md)

An independent model review found real holes. Corrections, with re-run numbers:

1. USE THE MAX ENTRY PRICE (0.85) FOR THE FLOOR, not the median. The floor
   binds on the trade you actually place; an 0.85 entry is un-sizable below
   $567, not $393. Re-run with max-price floor + 0.6 haircut (the honest case):
   $850 start still 0/3000 ruin, but worst shuffle trough $554 (grazes the
   floor), p5 $731. Verdict SURVIVES but the buffer is 33% not 46%. At $600
   start the worst shuffle reaches $371 (inside the flat-bet zone): $600 is
   more marginal than the median-floor sim implied. This reinforces "never
   start below ~$600" and the $550 stop.

2. HEADLINE METRIC IS "% OF PATHS BREACHING THE FLOOR BAND (~$567)", not
   "% ruined". The sim's ruin line (equity <= 5*price ~ $3) is a 99.6%
   drawdown, so "0/3000 ruin" is near-vacuous. The meaningful risk is entering
   the sub-$567 band where sizing breaks; that is the number to watch.

3. THE $10 CEILING REINTRODUCES FLAT SIZING ABOVE ~$1,333. Fractional
   protection ("clips shrink with equity") only operates in the ~$567-$1,333
   band. Above it, clip is a constant $10 and losses drain linearly (the June
   mechanism, slower: ~18 consecutive average-red-days to drain from $2k, so
   low RUIN probability, but real loss of the protective property and of
   compounding). FIX: scale the ceiling up with equity as the account grows
   (e.g. keep ceiling = 1.2% of equity) so sizing stays fractional past $1,333.
   Do this before the account grows past ~$1,300.

4. THE MONTE CARLO UNDERSTATES CLUSTERED-REGIME TAILS. Shuffling 75 fixed days
   without replacement assumes exchangeability and cannot emit a losing regime
   worse than the sample; a Markov/clustered model gives ~2x longer red
   streaks (p95 9 days vs the shuffle's 4). June was exactly such a multi-day
   bleed. Mitigation is NOT the MC's comfort but the $550 mechanical stop and
   sub-Kelly sizing; treat the MC drawdown percentiles as optimistic.

5. CONCURRENCY NOT PRICED. 0.75% is per-trade; with N concurrent open markets
   (and max_clips 2) gross simultaneous exposure is ~2-4 clips. Small at $850
   (~2-3% of bankroll) but real, and larger on the 15m book. Portfolio risk >
   single-trade risk.

6. SYMMETRIC HAIRCUT IS OPTIMISTIC ON THE DOWNSIDE. Multiplying losses by 0.6
   shrinks them, but live losses (worse fills, adverse selection) may EXCEED
   replay. Downside stats should stress an asymmetric haircut (losses un-cut).

7. REPRODUCIBILITY GAP. The trade dumps are gitignored, so the safety numbers
   are not re-runnable from the committed repo. Before micro-live, snapshot the
   daily P&L series into the repo so the safety case is auditable.

Net: the $850 viability and $550 stop hold, but the analysis was biased
optimistic on three axes (median floor, no-haircut, near-zero ruin line) and
misses the ceiling flat-band. The corrections tighten rather than overturn the
plan; the ceiling-scaling fix (item 3) is a real new pre-growth task.

## Hardened numbers after the review fixes landed (2026-07-08)

The sim was rebuilt (scripts/research/drawdown_sim.py, reproducible from the
committed data/research/daily_pnl_series.csv) with the honest p99 floor price
($4.42, bites at $589; p95 = $548 which validates the $550 stop) and a
clustered (regime-persistent) day-ordering adversary. The corrected headline,
$50-telemetry days at 0.6 haircut:

| model | longest red run p95 | $850 floor-band breach | $600 breach |
|---|---|---|---|
| uniform shuffle (old, optimistic) | 5 days | 0.1% | 35% |
| CLUSTERED (realistic) | 11 days | **11.8%** | 57% |

Both models: 0/3000 ruin at $850 (no wipeout). But the clustered model,
which is the honest one (June was a multi-day bleed), shows a ~12% chance at
$850 of drawing into the sub-$589 band where the 5-share floor degrades
fractional sizing. The old uniform-shuffle 0.5% was optimistic by ~20x.

CONSEQUENCE FOR SIZING: an ~1-in-8 chance of touching the floor band is not
negligible. Two responses, both cheap: (a) START MICRO-LIVE AT 0.5%, not
0.75% (earlier sim: halves the tail for ~2% of endpoint); (b) hold reserve
capital so the effective account is above the ~$700 level where clustered
breach drops sharply. "$600 is marginal" is now quantified: 57% clustered
breach. Revise "never start below $600" UP to "never start below ~$700".

The engine ceiling fix (PM_SHADOW_CLIP_CEIL_FRAC, merged in polymarket-agent)
closes item 3: set it to ~0.012 so the ceiling scales with equity and
fractional protection holds past ~$1,300 as the account grows.

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
