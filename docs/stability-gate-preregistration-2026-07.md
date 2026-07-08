# Pre-registered stability gate: spec frozen 2026-07-02, judged on July soak

# SOAK RESET 2026-07-08 (config-consistency-audit M-1)

The Jul 2-7 soak evidence is INVALIDATED for the economic/realization verdict:
shadow-final ran stop_before_close_s=10 (CLI default) instead of the canonical
90 (fix 2745424f), so it took late-window entries the backtest never takes,
corrupting the live-vs-replay comparison. Infrastructure metrics (heartbeats,
executor integrity 0 orphans/0 mismatches, twin agreement 98.8-100%) remain
VALID (config-independent). The soak restarts on the corrected config
2026-07-08; the v1 gate verdict clock restarts from the first full corrected
day, so the earliest judgment is ~2026-07-16 (7 non-Saturday evidence days).
Do not judge the gate on pre-reset (drifted-config) data.


This freezes the gate hypothesis BEFORE the out-of-sample data exists, so the
July soak is a true test rather than another fit. Do not modify the spec or
the pass criteria after soak data starts accruing.

## Candidate gate (expressible entirely in existing frozen-config knobs)

- `min_secs_from_open = 15` (currently 0)
- `max_p_side = 0.85` (currently 0.92; equivalent to |p_exo - 0.5| <= 0.35)

No code change; promotion is a config change through the PROD.md deployment
gate. GATE B parity is unaffected (both knobs already flow through
decide_entry on all paths).

## Why this gate (evidence as of freeze date)

Twin-labeled June 15-18 data (493 markets where two identical-config engines
both entered): entry timing and belief extremity are the two decision-time
features that predict twin DISAGREEMENT. The gated subset achieves 76% twin
agreement (vs 55% baseline) and held +$1,643 of the baseline's +$254 at-touch
P&L on those chop days. On the semi-independent ewma stream the REJECTED
subset was profitable (+$2,243) on the same days: the rejected bucket is not
reliably negative, it is UNSTABLE (43-49% cross-process agreement, worse than
a coin flip on side selection). Its expected value across process draws,
after fees, is not established; the gated subset's is.

## Pass criteria on the July soak (evaluate after 7 non-Saturday soak days)

Using twin_agreement_report.jsonl and realization.jsonl, computed on the
GATED subset (entries with secs_from_open >= 15 and p_side <= 0.85):

1. Twin agreement on the gated subset >= 75% (week aggregate), and at least
   12 points above the ungated remainder.
2. Realization ratio of the gated subset vs same-day replay >= 0.85 (week
   aggregate).
3. Gated-subset at-touch P&L positive for the week.
4. Sanity: gated subset takes >= 15% of baseline entries (a gate that trades
   nothing passes trivially and is useless).

All four hold: promote both knobs to the frozen config (docs/PROD.md change,
48h paper parity, then the micro-live phase measures realization at 1% clips).
Any fails: do NOT iterate the thresholds on the same week's data; the fallback
is the dwell-gate variant (belief_dwell_s >= 30, now logged) evaluated on the
FOLLOWING week, or acceptance that only agree-subset economics are tradeable.

## Expected economics if promoted (set expectations now)

On the June chop-week labeled set the gated subset made ~$410/day at $50
clips at-touch. At 1% fractional clips on the current $850 bankroll (~$8.5),
that scales to roughly $50-70/day on comparable tape BEFORE the live haircut
that the micro-live phase will measure. The point of the gate is not this
number; it is that the number should REPRODUCE across runs.

## Candidate v2 (pre-registered 2026-07-07): belief-dwell gate

Spec: `min_belief_dwell_s = 60` on the frozen config, current (timer)
architecture. Basis: at truthful latency (1250ms) the June replay shows
dwell>=60 BEATS ungated (+$3,174 vs +$2,506, hit 67.0% vs 62.0%, on 1,841
trades) because young-dwell burst trades no longer pay by fill time at our
real reaction speed; the gate additionally removes the live phantom bucket
(cross-process agreement 43-58% there). Implementation: first-class
belief_dwell_s input in decide_entry on all paths (commit 607c62ea),
regression-tested, default off.

Pass criteria on July live evidence (dwell_split.jsonl, final stream, all
non-Saturday days from Jul 7 to judgment; judged no earlier than Jul 12):
1. dwell[0,60) bucket cumulative at-touch P&L <= 0 (the bucket the gate
   removes must not be reliably profitable live).
2. dwell[60,inf) bucket hit rate >= its June-replay analogue minus 5 points
   (>= 62%).
3. dwell[60,inf) cumulative at-touch P&L positive.
Criteria fixed now; no threshold iteration on the same window. v1 (entry
delay + extremity cap) continues to be judged on its own criteria; if both
pass, v2 is preferred at current architecture (replay-superior), v1+v2
combination requires a fresh window.

## Candidate v2 verdict: FALSIFIED pre-judgment (2026-07-07)

Pre-June validation (user-requested) on May 7-28, the protocol's own
VERIFY+HOLDOUT windows, at truthful latencies:

| May 7-28 | ungated | dwell>=60 | gate cost |
|---|---|---|---|
| 1250ms | +$17,342 | +$283 | -98% |
| 750ms | +$25,986 | +$4,964 | -81% |

In big-move regimes the young-dwell burst entries are the payload, profitable
even at slow fills; June's chop made them worthless at 1250ms, which is the
only reason the June replay preferred the gate. A standing dwell gate
amputates the fat months. v2 is DEAD as a standing config; dwell remains a
telemetry/diagnostic field. The same regime-dependence test (May at truthful
latency) is now REQUIRED for any future gate candidate before its July-style
judgment; v1 is undergoing it now.

Standing conclusions this cements: speed generalizes across regimes (fast
engine +50% in May, +200% in June); entry gates so far do not; the only
quality layer with zero replay cost by construction is consensus execution
(it drops only cross-observer disagreements, and a real burst is seen by
both twins).

## v1 May falsification: PASS (2026-07-07)

Same test that killed v2: May 7-28 at truthful latency. v1 gated keeps
+$15,571 of +$17,342 at 1250ms (-10%) and +$24,758 of +$25,986 at 750ms
(-5%). May's payload trades are sustained-trend entries arriving after 15s,
so the gate shaves only the open-second scalps (the live phantom zone).
v1 is regime-robust where v2 was regime-fragile; its pre-registered July
judgment (criteria in this doc, ~Jul 11-12) proceeds as planned.
