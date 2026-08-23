# Decision stability: the engine disagrees with itself on fast tape

**Date:** 2026-07-02
**Trigger:** mining the parallel June shadow streams (found while inventorying
AWS/ECS log archives) to replicate the early-entry coin-flip finding.

## The finding

shadow-final and shadow-vol ran the SAME decision config on the SAME box with
independent WebSocket connections. Jun 15-18, 493 common markets, first-clip
side agreement between the two processes:

| bucket | n | same side |
|---|---|---|
| both entered <15s | 66 | 57.6% |
| both entered >=15s | 181 | 69.1% |
| entry timings diverged | 246 | **43.9%** |

Median entry-time difference between the twins: **38 seconds** (p90 160s).

An independent third stream (shadow-hold-ewma, different vol config)
replicates the early-entry penalty directly: early entries hit 52.4% vs 64.5%
late ($3.48 vs $8.82 per trade).

## Reinterpretation

The June live-vs-replay gap is not "live feeds vs recorded feeds". It is
**decision instability**: near the entry threshold on fast tape, tiny
feed-microstate differences (connection timing, tick arrival order) flip both
WHEN the engine enters and WHICH side it takes. The recorded-feed backtest is
one draw from a distribution of possible runs; each live process is another
draw. P&L attributable to unstable decisions is not capturable by any single
live process; the capturable edge is the stable-decision subset (which is
where live realization measured 0.8-1.1).

This strengthens the dwell/stability gates (layer 2) over the blanket entry
delay (layer 1): the delay only lifts twin agreement from 58% to 69%, so
timing alone does not make decisions stable. It also reframes the collector
architecture (layer 5): a single shared feed makes live consistent with
itself, but a decision that flips under a 1-tick perturbation is still not
robustly capturing backtest edge; the gate must require decisions that are
invariant to microstate.

## Instrumentation deployed (2026-07-02)

- **Decision twin**: `pm-shadow-final-b.service` runs an identical frozen-
  config engine on Dublin with its own feed connections. Daily
  `twin_agreement_report.py` (cron 00:15 UTC) logs: agreement rate, P&L of
  the agree vs disagree subsets, and agreement split by belief dwell
  (validating whether dwell >= 30s predicts stability).
- Combined with the existing soak scorecard and realization reports, the July
  week now measures, per day: process health, parity, realization ratio,
  entry-second buckets, dwell, and twin agreement.

## Decision rule addition (extends PROD.md gate b)

A gate is promotable when it selects a subset with BOTH >= 0.85 realization
against replay AND materially higher twin agreement. If no gate achieves
this, the honest conclusion is that only the stable-decision edge is
tradeable, and expected live P&L must be restated as the agree-subset P&L
(roughly the calm-day economics), not the full backtest number.

## Side finding: ECS

The `live-collector` ECS service (pm-research-prod, us-east-1) was a
scaffolding stub: image tag `placeholder`, never ran, flapping on image pull
since April. Scaled to desired-count 0 on 2026-07-02. The useful archives are
S3 (`polymarket-agent-archive.../whale-pair-exec` April paper-exec tarballs,
`rust-replay/v=1` Feb-Mar collector-format replays) and the Dublin local
stream logs used above.
