# Pre-registered stability gate: spec frozen 2026-07-02, judged on July soak

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
