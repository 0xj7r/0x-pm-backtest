# Config-consistency audit: fade deploy stack

Read-only audit. Verifies whether the config we would DEPLOY live matches the
config that was VALIDATED, parameter by parameter, across every context that
touches the BTC-5m exogenous-fade stack. No source or config was modified.

Scope: the worktree of `polymarket-backtest` at HEAD `a090801e` (identical to
the main checkout at the same commit) plus the `polymarket-agent` repo
(`polymarket-exec` crate). Every value below is cited `file:line` from those
trees. "Harness default" means the clap `default_value` applies because the flag
is absent from the launch command.

Citation keys: **PROD** = `docs/PROD.md`; **FROZ** = `crates/pm-shadow/src/lib.rs`
(`frozen_shadow_final_args`); **GATED** = `crates/pm-shadow/src/lib.rs`
(`gated_shadow_final_args`); **DEC** = `crates/pm-alpha/src/decide.rs`
(`frozen_fade_decide_config`, the decide SSOT); **CLI** =
`crates/pm-app/src/main.rs` (shadow/alpha clap defaults); **FL** =
`polymarket-agent/polymarket-exec/src/bin/fast_live.rs`; **SVC** =
`scripts/ops/systemd/*.service`; **FG** = `scripts/ops/shadow_final_foreground.sh`;
**GF** = `scripts/ops/shadow_final_gated_flags.sh`; **DR** =
`scripts/ops/daily_replay_yesterday.sh`; **SX** =
`polymarket-agent/polymarket-exec/src/shadow_exec.rs`; **RUN** =
`docs/micro-live-runbook-2026-07.md`.

---

## 1. The matrix

Columns: **PROD.md** (declared canonical) | **frozen_args** (the SSOT Rust
defaults `fast_live` and the tests consume) | **gated_args** (frozen + the live
gate package) | **fast_live** (the in-process engine+exec binary) | **shadow-final**
(the `pm-app shadow` live paper twin, via GF→FG→`pm-shadow-final.service`) |
**daily-replay** (the realization-baseline `pm-app alpha` harness).

| Parameter | PROD.md | frozen_args | gated_args | fast_live | shadow-final (live) | daily-replay |
|---|---|---|---|---|---|---|
| `edge_threshold` | 0.12 PROD:30,51 | 0.12 FROZ:134 | 0.12 (inherited) | 0.12 FL:63 | 0.12 FG:18 | 0.12 DR:65 |
| `perp_price_weight` | 0.75 PROD:32,52 | 0.75 FROZ:139 | 0.75 (inh.) | 0.75 FL:63 | 0.75 FG:19 | 0.75 DR:62,108,128 |
| `vol_lookback_s` | 3600 PROD:33,58 | 3600 FROZ:135 | 3600 (inh.) | 3600 FL:63 | 3600 FG:25 | 3600 DR:64,109,130 |
| `vol_estimator` | realized PROD:33,57 | "realized" FROZ:147 | realized (inh.) | realized FL:63 | realized FG:24 | realized DR:63,108,129 |
| `rearm_edge` | 0.08 PROD:34,54 | 0.08 FROZ:145 | 0.08 (inh.) | 0.08 FL:63 | 0.08 FG:21 | 0.08 DR:69,111,135 |
| `max_clips` | 2 PROD:35,55 | 2 FROZ:146 | 2 (inh.) | 2 FL:63 | 2 FG:22 | 2 DR:68,111,134 |
| `min_entry_sigma_bps` | 3.0 PROD:36,56 | 3.0 FROZ:144 | 3.0 (inh.) | 3.0 FL:63 | 3.0 FG:23 | 3 DR:71,112,137 |
| `skip_saturday` | true PROD:39,59 | true FROZ:149 | true (inh.) | true FL:63 | true FG:26 | true DR:72,112,138 |
| `stop_before_close_s` | **90** PROD:38 | **90** FROZ:143 | **90** (inh.) | **90** FL:63 (via FROZ) | **5** ❗ CLI:119 default; flag omitted by FG/GF | 90 DR:73,112,139 |
| `min_marginal_edge` | 0.04 PROD:43 | N/A (not a ShadowArgs field; DEC:37=0.04) | N/A (DEC:37) | 0.04 DEC:37 (not in sync list) | 0.04 DEC:37 (not in sync list) | 0.04 DR:74,113,140 |
| `min_entry_ask` | 0.45 PROD:40,62 | 0.0 FROZ:153 | 0.45 GATED:181 | 0.45 FL:65 | 0.45 GF:12 | omitted → 0.0 (off) CLI alpha:150 pattern |
| `skip_open_fav_gap` | true PROD:41,63 | false FROZ:165 | true GATED:182 | true FL:66 | true GF:13 | omitted → false (off) |
| `open_fav_p_min` | 0.88 PROD:41,64 | 0.90 FROZ:166 | 0.88 GATED:183 | 0.88 FL:67 | 0.88 GF:14 | omitted (off) |
| `open_fav_ask_max` | 0.62 PROD:41,65 | 0.60 FROZ:167 | 0.62 GATED:184 | 0.62 FL:68 | 0.62 GF:15 | omitted (off) |
| `open_fav_secs` | 300 PROD:41,66 | 5 FROZ:168 | 300 GATED:185 | 300 FL:69 | 300 GF:16 | omitted (off) |
| `skip_spot_misalign_s` | **30** PROD:42,61 | 0 FROZ:155 | **30** GATED:180 | **30** FL:64 | **30** GF:11 | omitted → 0 (off) CLI:421 |
| `min_secs_from_open` (v1 gate) | absent (ungated) PROD:44 | 0 FROZ:157 | 0 (inh.) | 0 FL:63 (not set) | 0 CLI:162 (omitted) | omitted → 0 (off) |
| `max_p_side` (v1 gate) | absent (ungated) PROD:44 | 1.0 FROZ:159 | 1.0 (inh.) | 1.0 FL:63 (not set) | 1.0 CLI:168 (omitted) | omitted → 1.0 (off) |
| `exit_after_s` | 0 PROD:31 | 0 FROZ:136 | 0 (inh.) | 0 FL:63 | 0 FG:20 | 0 DR:60,108,127 |
| notional / clip sizing | 1% via `PM_SHADOW_CLIP_FRAC`; ceiling 1.2% ($10 @ $850) PROD:99 | `SHADOW_NOTIONAL_USDC` (lib.rs:824) | same | env `PM_SHADOW_CLIP_FRAC` SX:83 (unset in SVC) | env `PM_SHADOW_CLIP_FRAC` SX:83 (in on-box env file SVC:12) | `--notional-usdc 50` (flat $50) DR:66,110,132 |
| `ceil_frac` (`PM_SHADOW_CLIP_CEIL_FRAC`) | 1.2% PROD:99 | N/A | N/A | env SX:82 (unset in SVC) | env SX:82 (on-box env) | N/A (flat notional) |
| `decide_interval_ms` | (not declared) | 1000 FROZ:169 | 1000 (inh.) | 100 SVC:14 (CLI default 100 FL:30) | 1000 FG:28 (`${...:-1000}`; SVC does not override) | N/A (batch backtest) |
| `decide_on_event` | (not declared) | false FROZ:170 | false (inh.) | true SVC:14 (`--decide-on-event`) | false (flag omitted by FG) | N/A |
| `fee_curve_rate` | 0.07 PROD:90 | N/A (no field) | N/A | N/A (live fees, real) | N/A (live fees, real) | 0.07 DR:75,113,141 |
| `latency_ms` | 250 PROD:90 | N/A (`latency_probe_ms`=150 FROZ:137, different concept) | N/A | N/A (live latency, real) | N/A (live latency, real) | 250 DR:67,133; **1250** DR:110 (second block) |

Rows where a cell reads "N/A" mean the parameter does not exist in that context
(e.g. `fee_curve_rate`/`latency_ms` are alpha-harness accounting knobs, not
shadow-engine fields; the live path pays real fees at real latency). "omitted →"
means the launch command does not pass the flag, so the clap `default_value`
applies.

---

## 2. Mismatches (every drift, with both locations and the canonical value)

### M-1 (CRITICAL): `stop_before_close_s` — live shadow-final twin runs **5**, canonical is **90**

- Canonical 90: PROD:38, `frozen_shadow_final_args` FROZ:143, `gated_shadow_final_args`
  (inherits) GATED:178-187, `fast_live` (via FROZ) FL:63, daily-replay DR:73,112,139.
- Live shadow-final value **5**: `shadow_final_foreground.sh` (FG:16-29) and
  `shadow_final_restart_gated.sh` BASE_FLAGS both omit `--stop-before-close-s`, and
  `shadow_final_gated_flags.sh` (GF:10-17) does not set it. The shadow subcommand's
  clap default for the field is `default_value = "5"` (CLI:119). That value flows
  unmodified into the decide config: `shadow_config_from_args` copies
  `args.stop_before_close_s` (FROZ:794), and `sync_decide_cfg` (called every
  `decide()` tick, FROZ:1303) overwrites `decide_cfg.stop_before_close_s =
  self.cfg.stop_before_close_s` (FROZ:1272). The decide gate then blocks entries
  only in the final `stop_before_close_s` seconds (DEC:441-442), so the twin enters
  up to 5 s before close instead of the validated 90 s window.

Root cause (git-blamed): the clap default `5` dates to `01de6b5f1` (2026-06-11,
a lane-mode default). The `sync_decide_cfg` overwrite at FROZ:1272 was added in
`750989e98` (2026-06-16); before that commit the decide config kept the
`frozen_fade_decide_config` value of 90 (DEC:45) because it was never overwritten.
The 2026-06-16 change made the CLI value propagate, silently breaking the
assumption documented in the field's own doc-comment ("fade mode keeps its
validated 90s constant regardless of this flag", CLI:117-118) — that comment is
now false for the CLI-driven shadow path.

Materiality: high. A 5 s deadline admits a different, far riskier entry set than
the 90 s window every backtest, the daily replay, `fast_live`, and PROD.md use.
This is precisely the class of live/backtest divergence the June postmortem
warned about. No test catches it: `frozen_shadow_final_args_matches_fade_ssot`
(FROZ:3233) exercises the Rust function (90), not the CLI path (5).

### M-2 (HIGH): sizing env values are not pinned in committed deploy config; PROD.md and the runbook disagree on the fraction

- Code wiring is correct and gate-independent: `effective_clip_usd`/`clip_from_parts`
  reads `PM_SHADOW_CLIP_USD`/`PM_FADE_CLIP_USD` (ceiling, default 15.0 SX:81),
  `PM_SHADOW_CLIP_CEIL_FRAC` (SX:82), `PM_SHADOW_CLIP_FRAC` (SX:83). These are
  executor env vars, not `ShadowArgs` decision fields, so they are independent of
  the gate config. Reduce-only invariant holds (SX:72-107; tests SX:791-819).
- But the actual values are not in the repo. `pm-fast-live.service` sets NO sizing
  env and explicitly has no EnvironmentFile (SVC pm-fast-live:9-14); `fast_live`
  falls back to `dotenvy::dotenv()` (FL:51) reading `/home/ubuntu/.env`, which is
  not committed. `pm-shadow-exec-paper.service` reads
  `EnvironmentFile=/home/ubuntu/.config/polymarket-exec/shadow_exec_tail.env`
  (SVC pm-shadow-exec-paper:12), also not committed. End-to-end sizing cannot be
  verified from source.
- The two docs disagree: PROD:99 declares `PM_SHADOW_CLIP_FRAC` = 1% ("Clip = 1%
  of current bankroll") with ceiling 1.2% ($10 @ $850). The runbook (updated
  2026-07-08, after TUNE+GLM review) declares `PM_SHADOW_CLIP_FRAC=0.005` (0.5%)
  and `PM_SHADOW_CLIP_CEIL_FRAC=0.012` (RUN:24,27). PROD.md self-declares SSOT
  (PROD:3-4) but was last updated 2026-07-03; the runbook is newer. So the
  canonical fraction is ambiguous from docs: 1% (PROD, stale) vs 0.5% (runbook,
  current). `ceil_frac` 0.012 (1.2%) is consistent across both.

### M-3 (HIGH): the gated package is absent from the validation/replay SSOTs (barer config than deploy)

- The deployed gated package — `skip_spot_misalign_s=30`, `min_entry_ask=0.45`,
  `skip_open_fav_gap=true` (p_min 0.88 / ask_max 0.62 / secs 300) — is applied only
  at deploy: `gated_shadow_final_args` GATED:180-185, `fast_live` manual overrides
  FL:64-69, and `shadow_final_gated_flags.sh` GF:11-16.
- It is NOT in the frozen SSOT: `frozen_shadow_final_args` has `skip_spot_misalign_s=0`,
  `min_entry_ask=0.0`, `skip_open_fav_gap=false`, `open_fav_secs=5` (FROZ:155,153,165,168).
- It is NOT in the daily realization replay: `daily_replay_yesterday.sh` omits all
  of `--skip-spot-misalign-s`, `--min-entry-ask`, `--skip-open-fav-gap` (DR:56-83,
  103-117), so they take the alpha harness defaults (off: `skip_spot_misalign_s`
  default 0 CLI:421; `min_entry_ask` default 0.0; `skip_open_fav_gap` default false).
- Consequence: the headline validated P&L (the ungated frozen champion) and the
  realization-ratio baseline both run a barer config than what deploys. The gates
  only remove entries, so deploy P&L ≤ validated P&L, but the deployed entry SET
  differs from the validated one. The re-arm gate's realization ratio (live vs
  daily-replay) compares live-gated entries against an ungated replay denominator,
  conflating gate-filtering with execution slippage (analogous to the latency
  conflation the 1250 ms block was added to fix, DR:97-101).

### M-4 (MEDIUM): `fast_live` re-implements the gated package by hand instead of calling `gated_shadow_final_args`

- `fast_live` calls `pm_shadow::frozen_shadow_final_args` then manually re-applies
  six overrides (FL:63-69). A comment claims parity with
  `scripts/shadow_final_gated_flags.sh (== pm_shadow::gated_shadow_final_args)`
  (FL:61-62), but it does not actually call `gated_shadow_final_args`. Today the
  six literals match GATED:180-185 exactly, so there is no current value drift,
  but the duplication is the exact shape that produced the June `fade_live`
  divergence: two code paths that must be hand-kept in sync. Any future edit to
  `gated_shadow_final_args` will not propagate to `fast_live`.

### M-5 (LOW, inert): `align_min_mid` and `enter_within_close_s` frozen values differ from CLI defaults

- `frozen_shadow_final_args` sets `align_min_mid=0.55` (FROZ:141) and
  `enter_within_close_s=0` (FROZ:142); the shadow CLI defaults are 0.85 (CLI:112)
  and 120 (CLI:115). Because `shadow_final_foreground.sh` omits both flags, the
  live twin runs 0.85 / 120, not 0.55 / 0. Both are lane-mode parameters and are
  inert in fade mode (`lane_late_fav=false`; `sync_decide_cfg` forces
  `enter_within_close_s=0` and `EntryMode::Fade` when lane is off, FROZ:1296-1299),
  so there is no behavioral effect today. Flagged because it is a latent
  frozen-vs-CLI mismatch of the same kind as M-1.

---

## 3. The specific claim under audit: `skip_spot_misalign_s`

The claim: canonical PROD uses 30; the shadow-final live paper twin runs 120;
`fast_live` correctly uses 30.

Verdict: **partially refuted.**

- Canonical 30: confirmed. PROD:42,61; `gated_shadow_final_args` GATED:180;
  `shadow_final_gated_flags.sh` GF:11.
- shadow-final live twin: **30, not 120.** The twin runs `shadow_final_foreground.sh`
  (SVC pm-shadow-final:9; SVC pm-shadow-final-b:10), which sources
  `shadow_final_gated_flags.sh` and applies `${SHADOW_FINAL_GATED_FLAGS[@]}`
  containing `--skip-spot-misalign-s 30` (GF:11, FG:9, FG:29). No 120 exists
  anywhere in the live deploy path. The only `120` values in the tree are
  research-only momentum horizons (`scripts/research/shadow_gate_closeout_sweep.py`,
  mom 0/30/60/120) and lane-mode `enter_within_close_s` defaults (CLI:115) — neither
  is `skip_spot_misalign_s` and neither is on the shadow-final command line.
- `fast_live` 30: confirmed. FL:64 (`shadow_args.skip_spot_misalign_s = 30`).

So the "shadow-final runs 120" part of the claim is wrong; both live paths run 30
on this parameter. The audit did, however, surface a different and more serious
shadow-final drift on a neighboring parameter — see M-1 (`stop_before_close_s` 5
vs 90), which is the real "June-style" divergence the claim was reaching for.

---

## 4. v1 stability gate (`min_secs_from_open`, `max_p_side`)

Confirmed ABSENT from every live config (ungated), present only where expected.

- `frozen_shadow_final_args`: `min_secs_from_open=0`, `max_p_side=1.0` (off)
  FROZ:157,159.
- `gated_shadow_final_args`: does not set them (GATED:178-187) → inherited 0/1.0.
- `shadow_final_gated_flags.sh`: neither flag (GF:10-17). shadow CLI defaults are
  0 (CLI:162) and 1.0 (CLI:168), so the live twin runs them off.
- `fast_live`: does not set them (FL:63-72) → 0/1.0.
- `pm-fast-live.service` ExecStart: neither flag (SVC:14).
- daily-replay: omits both (DR:56-83) → alpha defaults 0/1.0.

The gate is intentionally not applied yet: the v1 verdict is pending
(PROD:8, "judged ~Jul 9-10"; RUN:31-33, "FINAL gate on/off decided by Friday's
v1 verdict"). The only places these fields appear are the struct definitions
(FROZ:95,99; CLI:162,168), the wiring (FROZ:1280,1282), and archived research
sweeps (`scripts/archive/2026-07-cleanup/open_entry_sweep.sh`,
`mayjune_strategy_calibrate.sh`), which is exactly where expected. No live unit
sneaks either gate in.

---

## 5. Fractional sizing and ceiling-scaling wiring

- `PM_SHADOW_CLIP_FRAC` (fractional sizing): wired in `effective_clip_usd`/`clip_from_parts`
  (SX:83, 94-106). When set, clip = `frac × venue_cash`, capped by the ceiling;
  unknown or stale (>30 min) balance sizes DOWN to `ceiling.min(10.0)` (SX:105),
  never up. Independent of the gate config (read from executor env, not
  `ShadowArgs`).
- `PM_SHADOW_CLIP_CEIL_FRAC` (new ceiling-scaling): wired (SX:82, 99-103). When
  set, `effective_ceiling = ceiling.max(ceil_frac × cash)`, so the fractional
  clip is not flattened once `frac × cash` exceeds the fixed floor; the floor
  stays authoritative for small/stale balances. Unit-tested end-to-end
  (SX:809-818, incl. the $850 → 6.375 case and stale-fallback).
- End-to-end in deploy: NOT verifiable from source. The values live in uncommitted
  on-box env files (`shadow_exec_tail.env` for the tailer, SVC pm-shadow-exec-paper:12;
  `/home/ubuntu/.env` via dotenvy for `fast_live`, FL:51). `pm-fast-live.service`
  sets no sizing env and has no EnvironmentFile (SVC pm-fast-live:9-14), so
  `fast_live` in paper would fall back to the hard ceiling default 15.0 (SX:81)
  unless the dotenv file sets the fraction. The two docs also disagree on the
  fraction (1% PROD:99 vs 0.5% RUN:24) — see M-2.

So: the mechanism is wired and gate-independent; the deployed VALUES are not
auditable from the repo and the docs conflict.

---

## 6. Integration topology

Four deployed units (SVC) form two disjoint streams; they are NOT integrated into
the single "fast_live + consensus" stack the runbook describes.

- `pm-shadow-final.service` (SVC:9) runs `shadow_final_foreground.sh` → `pm-app shadow`,
  writing `would_enter` JSONL to `~/data/pm-alpha/shadow-final/`. On start it
  restarts the executor (ExecStartPost, SVC pm-shadow-final:13).
- `pm-shadow-final-b.service` (SVC pm-shadow-final-b:9,10) runs the same script
  with `SHADOW_FINAL_OUT=.../shadow-final-b` — an independent decision twin
  ("identical config, independent feeds; measures decision stability").
- `pm-shadow-exec-paper.service` (the executor) runs `shadow_exec_tail --paper`
  (SVC pm-shadow-exec-paper:19) and is bound to `pm-shadow-final.service`
  (`After=` + `PartOf=`, SVC:3,7). The tailer reads the shadow-final dir
  (`PM_SHADOW_JSONL_PATH`/`PM_SHADOW_FINAL_DIR`, default `"shadow-final"`,
  `shadow_jsonl.rs:183-186`; its own header: "follows `shadow-final`"). So the
  executor tails **shadow-final**, not consensus.
- `pm-shadow-consensus.service` runs `consensus_tail.py --a-dir shadow-final
  --b-dir shadow-final-b --out-dir shadow-consensus --window-s 45`
  (SVC pm-shadow-consensus:9), depending on both twins (SVC:3,4). It writes
  twin-agreed entries to `shadow-consensus/`. **Nothing consumes `shadow-consensus`**:
  no agent binary references it (grep of `polymarket-exec/src` for "consensus"
  returns nothing); the executor tails shadow-final; `fast_live` runs its own
  in-process engine.
- `pm-fast-live.service` (SVC:14) runs `fast_live --paper --out-dir shadow-fast
  --decide-interval-ms 100 --decide-on-event`. `fast_live` spawns its own engine
  (`pm_shadow::run_shadow_with_sink`, FL:120) AND its own execution loop
  (FL:111-118) in one process — no JSONL tail, no consensus input. It writes to
  `shadow-fast/` and paper-executes internally.

So the runbook's go-topology — "`fast_live` drives the executor" + "executor
consumes `shadow-consensus`" (RUN:29-30, 34-37) — is **not wired** in the deployed
units. Specifically: (a) `fast_live` does not consume consensus (it has no
consensus input); (b) the executor does not consume consensus (it tails
shadow-final); (c) `fast_live` does not drive the separate `shadow_exec_tail`
executor (it runs its own in-process executor); (d) the consensus output is
orphaned (measurement only). The live paper stack today is two independent
streams (shadow-final→executor; fast_live self-contained) plus an orphaned
consensus sidecar.

---

## 7. VERDICT

**No — the deploy config is NOT equal to the validated config.** The validated
core (edge/perp/vol/rearm/clips/sigma/saturday/exit/marginal-edge) is consistent
across every context, and the headline `skip_spot_misalign_s` claim is refuted
(both live paths run 30, not 120). But the audit found one critical drift and
several high/medium gaps that must close before any 48 h combined soak or go-live.

Gaps ranked by materiality:

1. **M-1 — `stop_before_close_s` 5 vs 90 on the live shadow-final twin (CRITICAL,
   go-blocking).** The deployed `pm-app shadow` twin enters up to 5 s before close
   instead of the validated 90 s, because `shadow_final_foreground.sh` /
   `shadow_final_restart_gated.sh` omit `--stop-before-close-s` and rely on a
   doc-comment ("fade keeps 90 regardless", CLI:117-118) that commit `750989e98`
   (2026-06-16) silently invalidated. Fix: add `--stop-before-close-s 90` to the
   gated flags SSOT (GF) so all shadow-final launches are explicit, and/or change
   the shadow CLI default to 90 and add a CLI-path parity test. Until this closes,
   the shadow-final stream is measuring a different entry window than backtest and
   must not be armed or used as the realization denominator.
2. **M-3 — gated package absent from validation/replay SSOTs (HIGH).** Deploy runs
   frozen+gated; the frozen champion and the daily realization replay run ungated.
   Add the gated package to `daily_replay_yesterday.sh` (or document that the
   realization ratio is intentionally gated-vs-ungated) so the re-arm gate compares
   like-for-like entries.
3. **M-2 — sizing values uncommitted + PROD/runbook disagree on the fraction
   (HIGH).** Commit the actual `PM_SHADOW_CLIP_FRAC` / `PM_SHADOW_CLIP_CEIL_FRAC`
   / ceiling values to a tracked env file (or the service units), reconcile PROD:99
   (1%) with RUN:24 (0.5%), and confirm `fast_live` actually loads them (it has no
   EnvironmentFile). Sizing is the only ruin-grade control; it cannot live in an
   uncommitted on-box file.
4. **Topology — "fast_live + consensus" is not wired (HIGH, go-blocking for the
   runbook's stated stack).** Decide explicitly which stream arms: either (a)
   `fast_live` self-contained with NO consensus filter (current wiring, simplest),
   or (b) wire the executor to tail `shadow-consensus` and make `fast_live`
   consume it (the runbook's intent). Today the consensus sidecar is orphaned and
   `fast_live` does not filter on it; arming under the runbook's stated topology
   without closing this would arm a different risk model than intended.
5. **M-4 — `fast_live` hand-duplicates the gated package (MEDIUM).** Have
   `fast_live` call `pm_shadow::gated_shadow_final_args` instead of six manual
   overrides (FL:64-69) to remove the sync hazard that bit `fade_live` in June.
6. **M-5 — inert frozen-vs-CLI mismatches on lane params (LOW).** `align_min_mid`
   (0.55 vs 0.85) and `enter_within_close_s` (0 vs 120) are inert in fade mode but
   are the same latent shape as M-1; fix by making the foreground script explicit
   or aligning defaults.

Go/no-go: **NO-GO for a 48 h combined soak / go-live** until M-1, M-2, M-3, and
the topology decision (item 4) are closed. M-4 and M-5 can follow but should not
ship armed. The `skip_spot_misalign_s=120` claim is refuted; the real shadow-final
drift is `stop_before_close_s=5`.
