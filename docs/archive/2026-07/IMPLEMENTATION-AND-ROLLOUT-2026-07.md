# Implementation record and rollout plan

**Date:** 2026-07-02
**Status:** paper soak running; live trading halted behind `fade.kill`; bankroll ~$850
**Companion references:** each section links the detailed doc that carries the evidence.

---

## 1. Where we are in one paragraph

The June deploy cycle took the account from ~$2,357 to ~$850. Forensics showed the
strategy's decisions were not the cause: the losses decompose into a broken parallel
implementation (-$1,050), ruin-grade sizing turned on by an automation, discretionary
manual trades, and a genuine live-execution weakness on whipsaw days that no backtest
could see. The two days since have been spent rebuilding the stack so that the same
failures are structurally impossible, quantifying the live weakness precisely, and
pre-registering the fix so that the July paper soak can judge it with clean,
out-of-sample evidence. Real money does not move until the gates in section 5 pass.

---

## 2. What June taught us (the why behind everything below)

1. **The engine's math was fine.** Canonical replay of June 13-30 (fees, latency,
   official resolution outcomes): +$17,689 at $50 clips, all 15 trading days green,
   robust to parameter perturbation (docs/archive/2026-07/june-2026-backfill-results.md,
   docs/data-validation-and-chop-sweep-2026-07.md).
2. **The process was not.** A reimplemented live binary wrong-sided 61% of trades; a
   win-streak automation doubled clips into a whipsaw; the ledger reported +$278 on a
   day the chain shows -$419; deploys were rsync'd, uncommitted, and unreviewed
   (docs/postmortem-2026-06-16-fade-live-divergence.md, memory:
   june-deploy-cycle-postmortem).
3. **The one real strategy weakness is decision instability.** Two identical engines
   on independent feed connections agree on the entry side only 57.6% for entries in
   the first 15 seconds of a window (43.9% when their timings diverge). On fast tape
   near the strike, the highest-conviction early entries are feed artifacts: live
   they hit 39.6% while paying maximum fees. Measured live realization Jun 16-18 was
   0.34 vs the 0.82 break-even; on calm days it was 0.8-1.1
   (docs/archive/2026-07/live-divergence-analysis-2026-07.md, docs/archive/2026-07/decision-stability-2026-07.md).
4. **Bad windows cannot be predicted, only survived and filtered.** Regime gates were
   tried twice and failed twice (the June version blocked 99% of entries out of
   sample). P&L circuit breakers destroy the recoveries. Protection must come from
   sizing, decision-quality filters, and consensus, never from market forecasting
   (memory: fade-adverse-window-not-gateable).

---

## 3. What has been implemented, and why

### 3.1 Engine correctness (single source of truth)

| item | why |
|---|---|
| One decision engine everywhere: live = shadow = backtest all call `pm_alpha::decide_entry`; the divergent `fade_live` binary is retired | The June bug class (parallel input pipeline drifting silently) becomes structurally impossible |
| Regime gates exist but are default-off, positioned so they can never block re-arming; `only_calm` strict on unclassified windows | The overfit June gates are preserved for research without contaminating prod; two review-caught bugs fixed with regression tests |
| Experimental directional tilt inert at 0 (byte-identical fade at defaults); clamp and sizing-belief bugs fixed | Research hooks must not perturb the validated path |
| `harness/replay.rs` unit tests (8, hand-computed cents) and 128 pm-alpha tests total | The backtest execution core previously had zero unit tests |
| Equivalence harness single-sourced (`pm_alpha::equivalence`, 1,376 duplicated lines to 745), byte-identical output, runs in CI and as a bin | The guardrail against input-pipeline drift can no longer itself drift |
| Belief-dwell telemetry (`belief_dwell_s` on every entry) | Makes the stability hypothesis measurable on live tape without deploying a gate |

### 3.2 Execution and governance (the June failure modes, closed)

| item | why |
|---|---|
| Fractional sizing in the executor: clip = `PM_SHADOW_CLIP_FRAC` x venue cash, ceiling $10, reduce-only invariant, stale-balance sizes DOWN | Flat $50 clips at $850 goes to exactly $0 in replay (docs/drawdown-sizing-2026-07.md); fractional 0.5-2% never ruins |
| night_scale scale-up machinery deleted (report-only monitor) | It doubled clips into the June 17-18 whipsaw; automations may only reduce risk |
| Kill criteria: parity breach or feed breach, never P&L | Manual P&L kills amputate the recoveries the edge depends on |
| On-chain reconciliation (`onchain_reconcile.py`), ledger fixed (actual fills, qty=0 books zero, sell legs, cross-midnight redeems), output labeled "ledger-estimated" | The dashboard lie (+$278 vs -$419) can be caught daily against chain truth |
| No discretionary trades on the strategy wallet (PROD.md governance) | June 18/20 hand trades cost ~$300+ |
| Deployment gate: all-window backtest evidence + committed code + 48h paper parity before any prod config change | The June regime gates went live in <24h off 6 days of tape |

### 3.3 Infrastructure and operations

| item | why |
|---|---|
| Dublin is a git checkout (read-only deploy keys); rsync deploys retired; flat `~/scripts` copies retired (renamed away so stale refs fail loudly) | The June cycle deployed uncommitted code; two live incidents trace partly to that |
| systemd user units with `Restart=always` + boot linger for engine, twin, and paper executor; engine `ExecStartPost` bounces the executor so it always tails the current JSONL (crash restarts included); legacy restart script delegates to systemctl | nohup processes died silently and the tailer did not follow rotated files; double-spawn bug review-caught |
| Scripts reorganized: `ops/` (38), `pipeline/`(+ec2) (27), `research/` (24), `archive/2026-07-cleanup/` (137); cross-references and stale IPs fixed (fail-fast `SHADOW_SSH_HOST`) | 230 flat files mixed prod-critical with dead one-offs; the box IP changes every start |
| Rejected strategies (back_to_explore, paired_mm) archived behind `--allow-legacy-strategies` | Docs said REJECT while the CLI treated them as active |
| ECS `live-collector` placeholder stub scaled to 0 | Flapping since April on a nonexistent image |
| docs/PROD.md: the single canonical config + execution path + accounting + governance + windows reference | The prior "canonical" reference was a handoff doc containing the overfit gates |

### 3.4 Measurement apparatus (what the soak actually measures)

| instrument | cadence | what it decides |
|---|---|---|
| `paper_soak_report.py` (Dublin cron 00:10) | daily PASS/FAIL: heartbeats, entries, parity deltas (restart-aware), mismatches | Gate A of re-arm: 7 PASS days |
| `daily_replay_yesterday.sh` + `mac_daily_pipeline.sh` (Mac launchd 07:30, catch-up) | canonical replay of yesterday + shadow sync + `realization.jsonl` | Realization ratio vs replay, split by entry-second bucket |
| Decision twin `pm-shadow-final-b` + `twin_agreement_report.py` (cron 00:15) | daily twin agreement, agree/disagree-subset P&L, dwell split | Whether decisions reproduce; the capturable-edge subset |
| `consensus_tail.py` (built, tested, NOT deployed) | merges twins, forwards only same-side agreements | The execution-layer instability filter, awaiting rollout step 3 |
| Fill model calibration | done (June fills) | Canonical accounting = harness at `--fee-curve-rate 0.07 --latency-ms 250`; at-touch scorers research-only |

### 3.5 Research findings locked in as documents

- Strategy soundness: plateau not spike under parameter perturbation on the worst
  week; regime historically normal for the fade's input
  (docs/data-validation-and-chop-sweep-2026-07.md).
- Strike basis: official strikes into a Binance-basis belief INVERT the edge
  (-10.4bps basis = 1.3x the median window move); `binance_proxy` is correct as-is
  (docs/strike-basis-experiment-2026-07.md).
- Satellites: all 12 candidates audited; zero pass the validation bar; two PARTIAL
  with a path (docs/research/satellite-validation-audit-2026-07.md).
- Stability gate PRE-REGISTERED before OOS data: `min_secs_from_open=15` +
  `max_p_side=0.85`, four fixed pass criteria, no threshold iteration on the same
  week (docs/archive/2026-07/stability-gate-preregistration-2026-07.md).

### 3.6 Data

Deep history fetched and validated: Binance 1m klines spot 2019-2026 and futures
2020-2026 (zero gaps), Deribit DVOL 2021-2026 (known events reproduce). June
Telonex books/trades re-ingested and mirrored to S3. Fetchers merged for
binance.vision bulk and DVOL. All new inputs validate on TUNE/VERIFY only; July is
sealed.

---

## 4. What is left to do, and why

| item | why it remains | blocked on |
|---|---|---|
| Judge the pre-registered stability gate | The gate was designed on June data; judging it on June data is the regime-gate mistake. Four criteria fixed in advance | 7 non-Saturday soak days (~Jul 9-10) |
| Deploy consensus execution | Built and tested, but swapping the executor's input mid-soak would corrupt the baseline measurement | Gate verdict; parity monitor repoint |
| Micro-live realization measurement | Paper fills cannot measure realized-vs-replay P&L; the 0.82 break-even question needs real fills. Bounded risk: ~$8.50 clips | Gate A (7 PASS days) + explicit user go |
| Dwell-gate fallback evaluation | If the pre-registered gate fails, the dwell variant (belief_dwell_s >= 30) is judged on the FOLLOWING week, not the same data | Only if gate fails |
| Collector architecture (single shared feed for all engines) | The structural fix for feed divergence; big build, and consensus may capture most of the value cheaply | Consensus results |
| DVOL/implied-vol as belief input; deep-history vol-estimator validation | Model upgrades need the full TUNE/VERIFY protocol; nothing model-side changes during rollout | After rollout stabilizes |
| Satellite re-validation (late-favourite lane, multibook) | Only candidates with a path; each needs sealed-window validation and day-correlation-to-fade | After core is live and stable |
| Replay-core review follow-ups (maker-path bookkeeping, latency-window fill edge case) | Research-only paths, logged in scratchpad; not on the prod taker path | Opportunistic |

---

## 5. Rollout plan

Sizing at every step: clip = 1% of current bankroll, ceiling 1.2% (both enforced in
the executor); kill switch present; kill on parity/feed breach only.

**Step 0 (running now, through ~Jul 9): paper soak, ungated baseline.**
Five metrics accrue daily (health, parity, realization, dwell, twin agreement).
Exit: 7 consecutive PASS days on the scorecard. Any structural failure (feed
outage class, parity orphans) restarts the count.

**Step 1 (~Jul 9-10): gate verdict.**
Evaluate the four pre-registered criteria on the week's data. Pass: promote
`min_secs_from_open=15` + `max_p_side=0.85` to the frozen config via the
deployment gate (config change, 48h paper parity re-soak). Fail: dwell variant
gets the following week; if that fails too, restate tradeable edge as
agree-subset economics and decide whether the smaller edge justifies live at all.

**Step 2 (with step 1's config): deploy consensus execution.**
Point the executor at the consensus stream; repoint the parity monitor to the
consensus file (intentional drops are not "missed"). 48h paper on the combined
stack: gate + consensus + fractional sizing.

**Step 3 (~mid-July, needs explicit go): micro-live, 14 days.**
Remove `fade.kill`, ~$8.50 clips, caps $10/$24. This phase IS the measurement of
the one number no simulation can produce: realization ratio of the full stack vs
same-day replay. Target >= 0.85 (break-even 0.82). Worst case is a slow bleed of
tens of dollars, not hundreds. No size changes during the window; no manual
trades; kill only on parity/feed breach.

**Step 4 (~Aug): scale decision.**
If realization >= 0.85 held: raise the ceiling with bankroll (clip 1%, ceiling
1.2%, weekly manual recompute), consider topping up capital. Expected economics
at $850 and June-like tape: roughly $50-70/day gated at-touch before the measured
haircut; the honest pitch is reproducibility, not the headline. If realization
< 0.85: stop, and the collector architecture becomes the prerequisite for any
further live attempt.

**Standing rules throughout:** July stays sealed for selection; no config changes
outside the deployment gate; automations only reduce risk; every number reported
against on-chain truth.

---

## 6. Principal risks

1. **The gate passes on a calm week and meets its first real whipsaw live.** Sizing
   bounds the damage; the twin keeps measuring stability live; consensus blocks the
   artifact trades regardless of regime.
2. **Realization lands between 0.82 and 0.85.** The edge would be real but barely
   capturable at taker; the fallback direction is execution improvement (passive
   exit already validated at -43% fees) before abandoning.
3. **A macro-vol regime change (2021-style).** Untested by construction; fractional
   sizing is the only defense, and it is always on.
4. **Single-box, single-operator fragility.** Dublin is one EC2 instance; the Mac
   pipeline needs the laptop awake occasionally (catch-up bounded at 7 days). Both
   are acceptable at this capital level; revisit at scale.
