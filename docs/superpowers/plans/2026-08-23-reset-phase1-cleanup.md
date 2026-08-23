# Reset Phase 1: Rescue + Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rescue the two pieces of unmerged branch work, then delete all dead strategies, dead crates, dead branches, and stale docs, leaving a green workspace containing only the living core plus exo_fade (retained temporarily as the Plan 2 extraction reference).

**Architecture:** Pure subtraction plus two cherry-picks. Every task ends with the workspace green: build, full test suite, and the exo_fade equivalence gate all passing. No engine behavior changes in this plan; behavior-changing work (engine extraction, constraint enforcement, killing exo_fade) is Plan 2.

**Tech Stack:** Rust 1.95 workspace (cargo), git, bash.

**Spec:** docs/superpowers/specs/2026-08-23-framework-reset-design.md (sections 2 and 8, phases 1-2)

## Global Constraints

- Repo: /Users/jackreid/go/polymarket-backtest, branch `main`, in-place (no rewrite, no force-push).
- After every task: `cargo build --release -p pm-app` succeeds, `cargo test --workspace` passes, `cargo run -q -p pm-app --bin exo_fade_equivalence` prints PASS.
- Do NOT touch `data/` in this plan (local-only data is synced to S3 in Plan 3 before deletion).
- Do NOT modify `crates/pm-alpha` decision logic or `crates/pm-strategy/src/exo_fade.rs` behavior in this plan.
- No em dashes anywhere (commits, docs, comments). No Co-Authored-By lines in commits.
- Commit messages: prefix `reset:` for deletions, `rescue:` for cherry-picks, `docs:` for doc moves.

---

### Task 1: Baseline snapshot and safety tag

**Files:**
- No file changes. Git state only.

**Interfaces:**
- Produces: tag `pre-reset-20260823` that every later task can diff against; a recorded green baseline.

- [ ] **Step 1: Verify clean tree and record baseline**

Run:
```bash
cd /Users/jackreid/go/polymarket-backtest
git status --porcelain          # expect empty
git log -1 --oneline            # expect 6cf44530 or later on main
```

- [ ] **Step 2: Verify the workspace is green before touching anything**

Run:
```bash
cargo build --release -p pm-app 2>&1 | tail -3
cargo test --workspace 2>&1 | tail -5
cargo run -q -p pm-app --bin exo_fade_equivalence
```
Expected: build OK; all tests pass (roughly 450 across crates); equivalence prints PASS. If any of these fail at baseline, STOP and report; do not proceed with deletions on a broken baseline.

- [ ] **Step 3: Tag the pre-reset state**

```bash
git tag pre-reset-20260823
git push origin pre-reset-20260823
```

### Task 2: Rescue the warm_spot_1s fix (cherry-pick d04bdd69)

**Files:**
- Modify: `crates/pm-shadow/src/lib.rs` (via cherry-pick)

**Interfaces:**
- Produces: `warm_spot_1s()` in pm-shadow (1s-grid spot warmup so bootstrap vol is not a step function). Plan 2's pm-shadow generalization builds on the fixed version.

- [ ] **Step 1: Inspect the commit before applying**

```bash
git show d04bdd69 --stat
git show d04bdd69 | head -80
```
Expected: one commit, ~53 changed lines, only `crates/pm-shadow/src/lib.rs`.

- [ ] **Step 2: Cherry-pick**

```bash
git cherry-pick d04bdd69
```
If conflicts: resolve keeping BOTH main's current logic and the new `warm_spot_1s` path; the fix replaces the 1m-kline spot warmup with a 1s-grid one. After resolving: `git cherry-pick --continue`.

- [ ] **Step 3: Verify pm-shadow and the equivalence gate**

```bash
cargo test -p pm-shadow
cargo run -q -p pm-app --bin exo_fade_equivalence
```
Expected: all pm-shadow tests pass (37 at baseline, plus any the commit adds); equivalence PASS. Note: if equivalence FAILS here, the warmup change altered decision output; that contradicts its bootstrap-only intent. STOP and report rather than forcing it in.

- [ ] **Step 4: Commit is already created by cherry-pick; verify message and amend prefix**

```bash
git log -1 --format=%s
git commit --amend -m "rescue: warm_spot_1s 1s-grid bootstrap vol fix (cherry-pick d04bdd69 from fix-1s-spot-backfill)"
```

### Task 3: Rescue four files from cross-market-data-foundation

**Files:**
- Create: `scripts/pipeline/telonex_ingest.py` (from branch path `scripts/telonex_ingest.py`)
- Create: `scripts/pipeline/launch_ec2_ingest.sh` (from branch path `scripts/launch_ec2_ingest.sh`)
- Create: `scripts/research/leg_synthesis_distortion.py` (from branch path `scripts/leg_synthesis_distortion.py`)
- Create: `docs/telonex-data-api.md` (same path on branch)

**Interfaces:**
- Produces: the direct-Telonex downloader + in-region EC2 launcher that Plan 3's backfill jobs will adapt.

- [ ] **Step 1: Extract the files from the branch (never merge the branch)**

```bash
git show cross-market-data-foundation:scripts/telonex_ingest.py > scripts/pipeline/telonex_ingest.py
git show cross-market-data-foundation:scripts/launch_ec2_ingest.sh > scripts/pipeline/launch_ec2_ingest.sh
git show cross-market-data-foundation:scripts/leg_synthesis_distortion.py > scripts/research/leg_synthesis_distortion.py
git show cross-market-data-foundation:docs/telonex-data-api.md > docs/telonex-data-api.md
chmod +x scripts/pipeline/launch_ec2_ingest.sh
```

- [ ] **Step 2: Sanity-check the Python parses**

```bash
python3 -m py_compile scripts/pipeline/telonex_ingest.py scripts/research/leg_synthesis_distortion.py
bash -n scripts/pipeline/launch_ec2_ingest.sh
```
Expected: no output (clean parse). Do not run them; they hit paid APIs.

- [ ] **Step 3: Commit**

```bash
git add scripts/pipeline/telonex_ingest.py scripts/pipeline/launch_ec2_ingest.sh scripts/research/leg_synthesis_distortion.py docs/telonex-data-api.md
git commit -m "rescue: telonex direct ingest + ec2 launcher + api notes from cross-market-data-foundation"
```

### Task 4: Write docs/OPEN-QUESTIONS.md

**Files:**
- Create: `docs/OPEN-QUESTIONS.md`

**Interfaces:**
- Produces: the canonical record of unresolved research questions, referenced by the doc-triage banners in Task 10.

- [ ] **Step 1: Read the source material**

```bash
git show d92d08dc | head -120   # past-close resolution-marking idea (patch targets a deleted file; take the idea only)
```
Also skim `docs/deployed-config-negative-2026-07.md` (section "The experiment") and `docs/stability-gate-preregistration-2026-07.md` (the four frozen criteria).

- [ ] **Step 2: Create the doc**

Content must cover, in this order, with dates and numbers taken from the named source docs:
1. **Cheap-underdog realization (never answered).** min_entry_ask 0.45 was backtest-negative (-$2,106 swing, docs/deployed-config-negative-2026-07.md); the shadow-recommended stream was deployed 2026-07-09 to measure live realization of cheap fades; repo froze 2026-07-10; Dublin box since terminated. Partial evidence may exist in s3://pm-research-data-prod/shadow/pm-alpha/dublin/ (Jul 1-12).
2. **v1 stability-gate verdict (never rendered).** Four frozen criteria in docs/stability-gate-preregistration-2026-07.md; judgment was due ~Jul 16 after the soak reset; never judged.
3. **Past-close resolution marking (idea from d92d08dc).** Entries whose timed exit lands after market close should be marked to resolution rather than exited at a stale quote; the original patch targeted `crates/pm-app/src/shadow.rs` which no longer exists; re-evaluate against pm-backtest in Plan 2.
4. **Both-sides hold / sell-loser (queued idea).** When sequential fades hold both sides below combined cost 1, test selling the losing leg on re-reversal vs redeeming at zero (from memory note both-sides-hold-sell-loser-idea).

- [ ] **Step 3: Commit**

```bash
git add docs/OPEN-QUESTIONS.md
git commit -m "docs: open questions register (unrendered verdicts, rescued ideas)"
```

### Task 5: Delete worktrees and dead branches

**Files:**
- No tracked file changes. Git refs and `.claude/worktrees/` only.

**Interfaces:**
- Produces: exactly two local branches remain (`main` plus none) and origin carries only `main`.

- [ ] **Step 1: Remove all agent worktrees (some are locked)**

```bash
git worktree list
for wt in /Users/jackreid/go/polymarket-backtest/.claude/worktrees/*/; do git worktree remove --force "$wt"; done
git worktree prune
git worktree list   # expect only the main checkout
```

- [ ] **Step 2: Delete merged local branches**

```bash
git branch -D shared-engine-core mm-queue-model-fit wallet-copytrade adapt-15m-windows mm-vol-conditioned add-favourite-config task/config-audit task/drawdown-monitor task/sim-hardening
git branch --list 'worktree-agent-*' | xargs git branch -D
```

- [ ] **Step 3: Delete the rescued-from branches (rescues are on main now: Tasks 2-4)**

```bash
git branch -D fix-1s-spot-backfill cross-market-data-foundation
```

- [ ] **Step 4: Delete stale remote branches**

```bash
git push origin --delete cross-market-data-foundation fix-1s-spot-backfill mm-queue-model-fit shared-engine-core
git fetch --prune
git branch -a   # expect: main, remotes/origin/main (plus HEAD pointer)
```

### Task 6: Repo hygiene (scripts/archive, pycache, stray files)

**Files:**
- Delete: `scripts/archive/` (160 files), `scripts/__pycache__/`, `scripts/ops/__pycache__/`, `shadow_tail_state.json`, `logs/` (empty dir)
- Modify: `.gitignore`

**Interfaces:**
- Produces: a scripts/ tree containing only ops, pipeline, research, cloud (later), ec2.

- [ ] **Step 1: Delete archives and caches**

```bash
git rm -r scripts/archive
git rm --cached shadow_tail_state.json 2>/dev/null || true
rm -f shadow_tail_state.json
find scripts -name __pycache__ -type d -exec rm -rf {} +
rmdir logs 2>/dev/null || true
```

- [ ] **Step 2: Harden .gitignore**

Append (only lines not already present):
```
__pycache__/
*.pyc
.cook/
shadow_tail_state.json
logs/
```

- [ ] **Step 3: Verify green and commit**

```bash
cargo build --release -p pm-app 2>&1 | tail -2 && cargo test --workspace 2>&1 | tail -3
git add -A
git commit -m "reset: drop scripts/archive, pycache, stray state files; harden gitignore"
```

### Task 7: Remove pm-copytrade

**Files:**
- Delete: `crates/pm-copytrade/`
- Modify: `Cargo.toml` (workspace members + workspace deps), `crates/pm-app/Cargo.toml`, `crates/pm-app/src/main.rs` (the ~7 call-site lines)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: a workspace without pm-copytrade; the CLI subcommand that used it is gone.

- [ ] **Step 1: Find every reference**

```bash
grep -rn "pm_copytrade\|pm-copytrade" --include='*.rs' --include='*.toml' crates/ Cargo.toml
```

- [ ] **Step 2: Delete the crate and every reference found**

Remove the crate dir, the workspace member line, the workspace dependency entry, pm-app's dependency line, the `use pm_copytrade::...` imports, and the CLI subcommand arm(s) in main.rs that construct copytrade types. Delete the whole subcommand (enum variant, clap struct, match arm), not just its body.

```bash
git rm -r crates/pm-copytrade
```

- [ ] **Step 3: Verify green**

```bash
cargo build --release -p pm-app 2>&1 | tail -2
cargo test --workspace 2>&1 | tail -3
cargo run -q -p pm-app --bin exo_fade_equivalence
```
Expected: build OK, tests pass, PASS.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "reset: remove pm-copytrade (abandoned wallet-copytrade experiment)"
```

### Task 8: Remove pm-engine and the EngineBacktest path

**Files:**
- Delete: `crates/pm-engine/`, `crates/pm-app/src/engine_driver.rs`
- Modify: `Cargo.toml`, `crates/pm-app/Cargo.toml`, `crates/pm-app/src/main.rs` (EngineBacktest subcommand: clap struct, enum variant, match arm, `mod engine_driver;`)
- Possibly modify: `crates/pm-strategy/src/convex/` callers (convex is reachable only via EngineBacktest; if removing the subcommand orphans convex, delete `crates/pm-strategy/src/convex/` and its `mod convex;` line in the same commit)

**Interfaces:**
- Produces: exactly one engine remains (the walkforward/runner path that Plan 2 extracts).

- [ ] **Step 1: Map the surface**

```bash
grep -rn "engine_driver\|EngineBacktest\|pm_engine\|pm-engine" --include='*.rs' --include='*.toml' crates/ Cargo.toml
grep -rn "convex" --include='*.rs' crates/pm-strategy/src/lib.rs crates/pm-app/src/ | grep -v "^crates/pm-strategy/src/convex/"
```

- [ ] **Step 2: Delete crate, driver, subcommand, and (if orphaned) convex**

```bash
git rm -r crates/pm-engine
git rm crates/pm-app/src/engine_driver.rs
```
Then remove all references found in Step 1. If convex's only non-self references were the engine path, also:
```bash
git rm -r crates/pm-strategy/src/convex
```
and remove `mod convex;`/`pub use convex...` from `crates/pm-strategy/src/lib.rs`.

- [ ] **Step 3: Verify green (same three commands as Task 7 Step 3), expected PASS**

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "reset: remove pm-engine parallel engine, engine_driver, EngineBacktest (and orphaned convex)"
```

### Task 9: Remove the nautilus tree and QuotesS3

**Files:**
- Delete: `crates/pm-telonex-loader/src/nautilus_conv.rs`
- Modify: `crates/pm-telonex-loader/src/lib.rs` (drop `mod nautilus_conv;` and re-exports), `crates/pm-telonex-loader/Cargo.toml` (drop nautilus deps), `crates/pm-app/src/main.rs` (QuotesS3 subcommand at ~lines 3913-3924 plus its clap struct/enum/match arm), root `Cargo.toml` (all 15 `nautilus-*` workspace dependency lines)

**Interfaces:**
- Produces: a nautilus-free dependency graph; cold builds drop the nautilus tree entirely.

- [ ] **Step 1: Map the surface**

```bash
grep -rn "nautilus" --include='*.rs' --include='*.toml' crates/ Cargo.toml | grep -v Cargo.lock
```

- [ ] **Step 2: Delete nautilus_conv.rs, the QuotesS3 subcommand, and every dep line found**

```bash
git rm crates/pm-telonex-loader/src/nautilus_conv.rs
```

- [ ] **Step 3: Regenerate the lockfile and verify**

```bash
cargo update --workspace 2>&1 | tail -2   # prunes nautilus from Cargo.lock
grep -c nautilus Cargo.lock                # expect 0
cargo build --release -p pm-app 2>&1 | tail -2
cargo test --workspace 2>&1 | tail -3
cargo run -q -p pm-app --bin exo_fade_equivalence
```
Expected: 0 nautilus entries, build OK, tests pass, PASS.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "reset: remove nautilus tree (nautilus_conv, QuotesS3 demo, 15 workspace deps)"
```

### Task 10: Remove legacy strategies (all but exo_fade and Noop)

**Files:**
- Delete: `crates/pm-strategy/src/bonereaper_v2.rs`, `back_to_explore.rs`, `paired_mm.rs`, `signals.rs`, `archive/spot_momentum.rs`
- Keep: `exo_fade.rs`, `regime.rs`, `lib.rs`, `archive/trivial.rs` (NoopStrategy)
- Modify: `crates/pm-strategy/src/lib.rs` (mod/pub use lines), `crates/pm-app/src/walkforward.rs` (~344 legacy lines: `BonereaperV2Profile`, `BackToExploreProfile`, `ResolvedStrategyProfile` legacy arms, `StratId` legacy variants, `--allow-legacy-strategies` plumbing, TOML fallback parsing for dead profiles), `crates/pm-app/src/main.rs` (~100 legacy lines), `crates/pm-app/src/runner.rs` (legacy dispatch arms), `crates/pm-app/src/result_summary.rs` (6 bonereaper gate-stat refs)

**Interfaces:**
- Consumes: Task 8 already removed convex if orphaned.
- Produces: `StratId` reduced to `{ExoFade, MayJuneFade, Noop}` (MayJuneFade dies in Plan 2 with exo_fade); `Ctx` untouched in this plan (slimmed in Plan 2, since its fields are load-bearing for exo_fade compilation checks).

This is the one task where mechanical grep is insufficient. Work strategy-by-strategy, compiling between each, so errors stay attributable.

- [ ] **Step 1: Delete paired_mm (smallest, self-declared ARCHIVED)**

```bash
git rm crates/pm-strategy/src/paired_mm.rs
grep -rn "paired_mm\|PairedMm" --include='*.rs' crates/ | grep -v Binary
```
Remove every reference found (lib.rs mod/exports, StratId variant, profile struct, walkforward/main/runner arms). Then `cargo build --release -p pm-app 2>&1 | tail -2` until clean.

- [ ] **Step 2: Delete back_to_explore the same way**

```bash
git rm crates/pm-strategy/src/back_to_explore.rs
grep -rn "back_to_explore\|BackToExplore" --include='*.rs' crates/ | grep -v Binary
```
Build until clean.

- [ ] **Step 3: Delete bonereaper_v2 + signals + spot_momentum the same way**

```bash
git rm crates/pm-strategy/src/bonereaper_v2.rs crates/pm-strategy/src/signals.rs crates/pm-strategy/src/archive/spot_momentum.rs
grep -rn "bonereaper\|Bonereaper\|spot_momentum\|SpotMomentum\|signals::" --include='*.rs' crates/ | grep -v Binary
```
This includes the result_summary.rs gate-stat blocks and the pre-existing dead-code warnings (`BonereaperV2Profile::load`, `into_bonereaper_v2`). Build until clean.

- [ ] **Step 4: Remove the --allow-legacy-strategies flag and ARCHIVED plumbing in walkforward.rs**

```bash
grep -n "allow.legacy\|ARCHIVED" crates/pm-app/src/walkforward.rs crates/pm-app/src/main.rs
```
Delete the flag, the ARCHIVED list, and the rejection message; there is nothing left to allow.

- [ ] **Step 5: Full verification**

```bash
cargo build --release -p pm-app 2>&1 | tail -2
cargo test --workspace 2>&1 | tail -5
cargo run -q -p pm-app --bin exo_fade_equivalence
./target/release/pm-app walk-forward --help | head -20   # confirm CLI still parses; legacy strategy names gone from help text
```
Expected: build OK with zero warnings about dead legacy code, tests pass (test count drops by the ~180 legacy-strategy tests), PASS.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "reset: remove legacy strategies (bonereaper_v2, back_to_explore, paired_mm, signals, spot_momentum)"
```

### Task 11: Remove pm-model (trace-first, conditional)

**Files:**
- Delete (if trace allows): `crates/pm-model/`
- Modify: `Cargo.toml`, `crates/pm-app/Cargo.toml`, `crates/pm-strategy/Cargo.toml`, remaining refs in `main.rs`/`runner.rs`/`walkforward.rs`

**Interfaces:**
- Consumes: Task 10 (legacy removal should have eliminated most pm-model consumers).
- Produces: either a workspace without pm-model, or a documented deferral to Plan 2.

- [ ] **Step 1: Re-trace after Task 10**

```bash
grep -rn "pm_model\|pm-model" --include='*.rs' --include='*.toml' crates/ Cargo.toml | grep -v Cargo.lock
```
Known baseline refs: main.rs:9 (MetaTrainingConfig), main.rs:3802, runner.rs:23,819,2628, walkforward.rs:13,6462,6489. Most belong to legacy dispatch and the meta-calibrator plumbing.

- [ ] **Step 2: Decide per reference**

For each surviving ref, answer: is it reachable from the exo_fade walk-forward path (`--strategies exo_fade`)? If NO for all: delete the crate and the refs. If YES for any (likely the meta-calibrator hooks): do NOT delete; instead add a line to docs/OPEN-QUESTIONS.md ("pm-model removal deferred to Plan 2 engine extraction; live refs: <list>") and skip Steps 3-4.

- [ ] **Step 3 (delete case): Remove crate and refs**

```bash
git rm -r crates/pm-model
```
Remove workspace member, workspace dep, per-crate dep lines, and the code refs. Build until clean.

- [ ] **Step 4: Verify green (three standard commands) and commit**

```bash
git add -A && git commit -m "reset: remove pm-model (post-legacy trace showed no exo_fade-path consumers)"
```
(or, deferral case: `git add docs/OPEN-QUESTIONS.md && git commit -m "docs: pm-model removal deferred to plan 2 with live ref list"`)

### Task 12: Doc triage (archive + banners)

**Files:**
- Create: `docs/archive/2026-06/`, `docs/archive/2026-07/`
- Move (git mv) into `docs/archive/2026-06/`: `active_btc5m_experiments.md`, `alpha-hunt-001-base.md`, `alpha-hunt-002-multimarket.md`, `alpha-hunt-003-short-horizons.md`, `autoresearch-ml-loop.md`, `binance_flow_discovery_062901_30d.md`, `late-favourite-lane.md`, `mm_queue_model_2026-06-01.md`, `global_regime_classifier_router.md`, `directional_satellite_candidates.md`, `handoff/2026-06-19-regime-gates-high-variance-research.md`
- Move into `docs/archive/2026-07/`: `alpha-roadmap.md`, `data-validation-and-chop-sweep-2026-07.md`, `decision-stability-2026-07.md`, `deployed-config-negative-2026-07.md`, `live-divergence-analysis-2026-07.md`, `root-cause-track-2026-07.md`, `stability-gate-preregistration-2026-07.md`, `IMPLEMENTATION-AND-ROLLOUT-2026-07.md`, `micro-live-runbook-2026-07.md`, `june-2026-backfill-results.md`
- Keep in place (the keep-list): `PROD.md`, `WHY-LIVE-DIVERGED.md`, `deep-review-2026-07-10.md`, `latency-truth-2026-07.md`, `fill-model-calibration-2026-07.md`, `realization-baseline-correction-2026-07.md`, `drawdown-handling-plan-2026-07.md`, `drawdown-sizing-2026-07.md`, `strike-basis-experiment-2026-07.md`, `postmortem-2026-06-16-fade-live-divergence.md`, `aws-backtest-runbook.md`, `OPEN-QUESTIONS.md`, `telonex-data-api.md`, `research/`, `superpowers/`
- Modify (banners): four files listed in Step 2

**Interfaces:**
- Consumes: Task 4's OPEN-QUESTIONS.md (banners point at it).
- Produces: a docs/ root a newcomer can read top-to-bottom without being misled.

- [ ] **Step 1: Create archive dirs and git mv the files per the lists above**

```bash
mkdir -p docs/archive/2026-06 docs/archive/2026-07
# then one git mv per file, exactly the lists above
```

- [ ] **Step 2: Add banners (insert as the first lines after the title)**

To `docs/archive/2026-07/june-2026-backfill-results.md`:
```
> ARCHIVED 2026-08-23. All figures here are 250ms-latency upper bounds. Truthful-latency June is +$2,506 at 1250ms; see docs/latency-truth-2026-07.md before quoting anything from this file.
```
To `docs/archive/2026-06/handoff/...regime-gates...md` (widen the existing banner):
```
> ARCHIVED 2026-08-23. The base-gate config blocks in sections 2 and 10 (min_entry_ask 0.45, open_fav, skip_spot_misalign) were later measured backtest-NEGATIVE (-$459 June, docs/archive/2026-07/deployed-config-negative-2026-07.md). Do not copy any command line from this file.
```
To `docs/archive/2026-06/global_regime_classifier_router.md`:
```
> ARCHIVED 2026-08-23, UNVALIDATED. Fill-derived clusters, synthetic sizing, never engine-replayed; polarity flips by period. A design sketch, not a result.
```
To `docs/aws-backtest-runbook.md` (kept, but banner):
```
> NOTE 2026-08-23: ops mechanics remain useful, but this predates the truthful-latency standard and references retired strategy profiles. All new runs use 750/1250ms accounting; see docs/superpowers/specs/2026-08-23-framework-reset-design.md.
```

- [ ] **Step 3: Fix dangling references**

```bash
grep -rn "docs/deployed-config-negative\|docs/stability-gate-preregistration\|docs/june-2026-backfill-results\|docs/IMPLEMENTATION-AND-ROLLOUT\|docs/micro-live-runbook\|docs/live-divergence-analysis\|docs/decision-stability\|docs/root-cause-track\|docs/alpha-roadmap" docs/ README.md CONTEXT.md scripts/ --include='*.md' --include='*.sh' --include='*.py' -l
```
Update each hit to the new `docs/archive/2026-07/...` path.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "docs: archive dated experiment logs, banner misleading figures, fix references"
```

### Task 13: Rewrite README to the post-reset reality

**Files:**
- Modify: `README.md`, `CONTEXT.md` (Active strategies table only)

**Interfaces:**
- Consumes: everything above (README must describe the tree as it now stands).
- Produces: the re-entry doc for Plan 2's implementers.

- [ ] **Step 1: Rewrite README.md**

Keep it under ~80 lines. Must contain: one-paragraph purpose (Polymarket BTC/ETH backtesting, cloud-first, constraints-enforcing); the surviving workspace tree (pm-types, pm-risk, pm-telonex-loader, pm-alpha, pm-strategy trait + exo_fade pending Plan 2, pm-shadow, pm-app); the quickstart (build, S3 auth, discover-day, walk-forward with `--strategies exo_fade` only); a "State" section: reset executed 2026-08-23 per docs/superpowers/specs/2026-08-23-framework-reset-design.md, exo_fade retained only as the Plan 2 extraction reference, zero deployable strategies is the intended end state; pointers to CONSTRAINTS (Plan 2), PROD.md, deep-review-2026-07-10.md, OPEN-QUESTIONS.md.

- [ ] **Step 2: Update CONTEXT.md's Active strategies table**

Replace the four-row table with: exo_fade (canonical reference, scheduled for removal in Plan 2) and Noop (baseline). Leave the pipeline/layer table untouched.

- [ ] **Step 3: Final full verification**

```bash
cargo build --release -p pm-app 2>&1 | tail -2
cargo test --workspace 2>&1 | tail -5
cargo run -q -p pm-app --bin exo_fade_equivalence
git status --porcelain   # expect only README.md/CONTEXT.md staged-or-modified before commit
```

- [ ] **Step 4: Commit and push**

```bash
git add README.md CONTEXT.md
git commit -m "docs: README/CONTEXT reflect post-reset workspace"
git push origin main
```

## Deliberately NOT in this plan

- Engine extraction out of walkforward.rs, Ctx slimming, killing exo_fade/MayJuneFade, pm-shadow generalization, CONSTRAINTS.md enforcement: Plan 2.
- Anything touching `data/` (42GB local cache), S3 sync-up, Telonex Pro backfill, cloud runner: Plan 3.
- polymarket-agent fast_live.rs min_entry_ask landmine: Plan 3 (single PR in that repo).
- `target/` deletion: free to do any time (`cargo clean`), not worth a task.
