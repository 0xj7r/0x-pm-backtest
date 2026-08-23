# Reset Phase 2: Engine Extraction, Constraints as Code, The Kill

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract the backtest engine into a clean `pm-backtest` crate proven byte-identical against a pinned tape, bake the ten spec constraints into code (including TWAP-era settlement and the TWAP belief primitive), then delete exo_fade and all remaining legacy surface so the repo ends with zero strategies on proven plumbing.

**Architecture:** Three movements. (1) Golden-tape harness first, then verbatim module moves out of walkforward.rs/runner.rs into pm-backtest, gated by byte-identity after every move. (2) Constraint enforcement built INTO pm-backtest (latency floor + fantasy watermark, always-on fees, jitter, scorecard, fingerprint, TWAP eras) with unit tests per rule. (3) The kill: exo_fade and MayJuneFade deleted, Ctx slimmed, pm-shadow generalized over the Strategy trait, pm-model removed after decoupling, golden test re-anchored on a tests-only fixture strategy.

**Tech Stack:** Rust 1.95 workspace (cargo), git, bash, jq. Local data cache (through 2026-07-10) for the golden tape; no network needed.

**Spec:** docs/superpowers/specs/2026-08-23-framework-reset-design.md (sections 3, 4 incl. constraints 9-10, 7, and 8 phases 3-5)

## Global Constraints

- Repo /Users/jackreid/go/polymarket-backtest, branch `main`, in-place; controller pushes after each task's review gate.
- Gates after EVERY task: `cargo build --release -p pm-app` (zero warnings), `cargo test --workspace` green, and from Task 1 onward `bash scripts/research/golden_replay.sh check` prints `GOLDEN: IDENTICAL` (until Task 12 re-anchors it, after which the new fixture golden must pass). Until Task 12, `cargo run -q -p pm-app --bin exo_fade_equivalence` must also print PASS.
- Behavior preservation is the bar for Tasks 1-6 (movements are verbatim; only visibility/paths change). Behavior ADDITIONS (Tasks 7-11) must not change results when their features are off/default-compatible, proven by the golden check.
- `data/` is read-only (the golden tape reads from it; nothing writes or deletes).
- No em dashes anywhere; no Co-Authored-By; commit prefixes: `engine:` (extraction), `constraint:` (enforcement), `reset:` (kill), `docs:`.
- Bash timeouts up to 600000 ms for build/test/replay steps.

---

### Task 1: Pinned-tape golden harness

**Files:**
- Create: `scripts/research/golden_replay.sh`
- Create: `tests/golden/README.md`, `tests/golden/day-2026-06-25.sha256` (committed hash, not the full JSON)

**Interfaces:**
- Produces: `golden_replay.sh {record|check}`. `record` runs the canonical replay and writes hash + a copy of the JSON to `/tmp/golden-run.json`; `check` re-runs and diffs the hash against the committed one, printing `GOLDEN: IDENTICAL` or `GOLDEN: DIVERGED` (exit 1). Every later task's gate calls `check`.

- [ ] **Step 1: Build the day manifest**

```bash
cd /Users/jackreid/go/polymarket-backtest
mkdir -p tests/golden
jq -c 'select(.slug | test("2026-06-25"))' data/manifests/canonical/btc-updown-5m_up.jsonl > /tmp/golden-markets.jsonl
wc -l /tmp/golden-markets.jsonl   # expect on the order of 288 markets; if 0, inspect the manifest's date encoding (read 2 lines) and adjust the filter to match its actual date field, documenting the chosen filter in the script
```

- [ ] **Step 2: Write scripts/research/golden_replay.sh**

The script (bash, set -euo pipefail) must:
1. Rebuild the day manifest exactly as Step 1 (self-contained).
2. Run: `./target/release/pm-app walk-forward --markets /tmp/golden-markets.jsonl --strategies exo_fade --starting-cash 1000 --max-clip-usdc 50 --spot-symbol BTCUSDT --use-outcome-label --fee-curve-rate 0.07 --latency-ms 1250 --local-cache-dir data/cache --out-markets /tmp/golden-run.jsonl --out-summary /tmp/golden-run.json` (verify each flag exists via `walk-forward --help` first; if a flag differs, use the actual spelling and record it in the script header).
3. Normalize: strip any timestamp/duration/host fields from the outputs (`jq 'del(.timings, .generated_at, .wall_*, .host)'` style; discover the actual volatile fields by running twice and diffing, then delete exactly those).
4. `record` mode: write `shasum -a 256` of normalized out-markets+summary to `tests/golden/day-2026-06-25.sha256`.
5. `check` mode: recompute and compare; print `GOLDEN: IDENTICAL` / `GOLDEN: DIVERGED` and exit 0/1.

- [ ] **Step 3: Prove determinism**

Run the replay twice back-to-back; the two normalized hashes must match. If they differ, find the nondeterminism (HashMap iteration order is the usual suspect), fix the NORMALIZATION (sort JSON keys and arrays by market id via `jq -S` and a stable sort), never the engine, and document what was normalized in tests/golden/README.md.

- [ ] **Step 4: Record the golden hash and gate**

```bash
cargo build --release -p pm-app 2>&1 | tail -2
bash scripts/research/golden_replay.sh record
bash scripts/research/golden_replay.sh check   # expect GOLDEN: IDENTICAL
cargo test --workspace 2>&1 | tail -3
```

- [ ] **Step 5: Commit**

```bash
git add scripts/research/golden_replay.sh tests/golden/
git commit -m "engine: pinned-tape golden harness (2026-06-25, canonical accounting, 1250ms)"
```

### Task 2: Extraction map

**Files:**
- Create: `docs/superpowers/plans/2026-08-23-phase2-extraction-map.md`

**Interfaces:**
- Produces: the authoritative item-by-item move list Tasks 3-5 execute. Each row: item (fn/struct/impl/const), current location (file:line), destination (`pm-backtest::engine` | `pm-backtest::fills` | `pm-backtest::accounting` | `pm-backtest::scorecard` | `pm-backtest::config` | stays in pm-app CLI | delete), and dependencies that force ordering.

- [ ] **Step 1: Inventory walkforward.rs, runner.rs, result_summary.rs**

Enumerate every top-level item (`grep -n "^pub fn\|^fn\|^pub struct\|^struct\|^pub enum\|^enum\|^impl\|^pub const\|^const\|^pub mod" crates/pm-app/src/walkforward.rs crates/pm-app/src/runner.rs crates/pm-app/src/result_summary.rs`) and classify each into the destination taxonomy above. Read the surrounding code for anything whose role is unclear from the name. The engine loop, event ordering, fill model (latency shift, book walk, fee application), portfolio state wiring, and per-market accounting go to pm-backtest; clap structs, manifest loading glue, and output-file writing stay in pm-app; anything reachable only from deleted code is marked delete.

- [ ] **Step 2: Identify the seams**

Explicitly answer in the doc: (a) where pm_model is evaluated (runner.rs ~730-747, Ctx population ~780-795) and what trait boundary would make it pluggable later; (b) which items runner.rs and walkforward.rs share; (c) what pm-shadow imports from pm-alpha that the new crate must not break; (d) the exo_fade dispatch path that must survive until Task 12.

- [ ] **Step 3: Order the moves**

Split the map into three tranches matching Tasks 3, 4, 5 such that each tranche compiles and passes the golden check on its own.

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/plans/2026-08-23-phase2-extraction-map.md
git commit -m "engine: extraction map for pm-backtest carve-out"
```

### Task 3: Create pm-backtest, move the fill/engine core (tranche 1)

**Files:**
- Create: `crates/pm-backtest/Cargo.toml`, `crates/pm-backtest/src/lib.rs`, `crates/pm-backtest/src/engine.rs`, `crates/pm-backtest/src/fills.rs` (module names per the map)
- Modify: root `Cargo.toml` (workspace member + dep), `crates/pm-app/Cargo.toml`, `crates/pm-app/src/runner.rs` (items removed, replaced by `use pm_backtest::...`)

**Interfaces:**
- Consumes: Task 2's map, tranche 1.
- Produces: `pm-backtest` crate compiling with the moved items re-exported under the module layout the map names. pm-app's runner.rs calls into it; behavior byte-identical.

- [ ] **Step 1: Scaffold the crate** (Cargo.toml mirroring pm-app's relevant deps: pm-types, pm-alpha, pm-risk, pm-model for now, serde, tracing)

- [ ] **Step 2: Move tranche 1 items verbatim** (cut from runner.rs, paste into the destination module, adjust `pub` and `use` only; no logic edits; keep item order stable to keep diffs reviewable)

- [ ] **Step 3: Gate**

```bash
cargo build --release -p pm-app 2>&1 | tail -2
bash scripts/research/golden_replay.sh check     # GOLDEN: IDENTICAL required
cargo test --workspace 2>&1 | tail -3
cargo run -q -p pm-app --bin exo_fade_equivalence
```
If GOLDEN diverges: stop, bisect the tranche (move half back), find the behavioral edit, fix. Never re-record the golden hash in this task.

- [ ] **Step 4: Commit** `engine: pm-backtest crate, fill/engine core moved (tranche 1, golden-identical)`

### Task 4: Move orchestration and accounting (tranche 2)

**Files:**
- Create: `crates/pm-backtest/src/portfolio.rs`, `crates/pm-backtest/src/accounting.rs` (per map)
- Modify: `crates/pm-app/src/walkforward.rs` (orchestration moved out), `crates/pm-app/src/runner.rs`, `crates/pm-backtest/src/lib.rs`

Same procedure as Task 3 for tranche 2 (walk-forward market iteration, portfolio mode, per-market accounting): move verbatim, gate with build + GOLDEN: IDENTICAL + workspace tests + equivalence PASS, commit `engine: orchestration and accounting moved (tranche 2, golden-identical)`.

### Task 5: Collapse the remnants (tranche 3)

**Files:**
- Modify: `crates/pm-app/src/walkforward.rs` reduced to thin CLI glue (arg structs, manifest load, call pm-backtest, write outputs) or deleted with its glue folded into `crates/pm-app/src/main.rs`; `crates/pm-app/src/result_summary.rs` moved to `pm-backtest::scorecard` per map.

Same procedure: move/delete per map tranche 3, gate (build zero warnings, GOLDEN: IDENTICAL, tests, equivalence PASS), verify `wc -l crates/pm-app/src/walkforward.rs` is now either absent or under ~400 lines of glue, commit `engine: walkforward monolith collapsed (tranche 3, golden-identical)`.

### Task 6: Config fingerprint (constraint 6)

**Files:**
- Create: `crates/pm-backtest/src/fingerprint.rs`
- Modify: `crates/pm-backtest/src/lib.rs`, output-writing paths (summary JSON gains `config_fingerprint`), `crates/pm-shadow/src/lib.rs` (banner line logs the same fingerprint), `crates/pm-app/src/main.rs`

**Interfaces:**
- Produces: `pub fn config_fingerprint(cfg: &ResolvedConfig) -> String` (16-hex prefix of sha256 over the canonical serde_json serialization with sorted keys of the FULLY RESOLVED config, defaults included).

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn fingerprint_is_stable_across_construction_paths() {
    let a = ResolvedConfig::default();
    let b = ResolvedConfig::from_cli_defaults(); // however defaults enter via clap
    assert_eq!(config_fingerprint(&a), config_fingerprint(&b));
}
#[test]
fn fingerprint_changes_when_any_field_changes() {
    let a = ResolvedConfig::default();
    let mut b = a.clone();
    b.latency_ms += 1;
    assert_ne!(config_fingerprint(&a), config_fingerprint(&b));
}
```
(Adapt type names to the actual resolved-config struct the extraction produced; if clap defaults and struct defaults are separate sources, unify them first: clap must derive its defaults FROM the struct's Default impl, which is itself part of constraint 6.)

- [ ] **Step 2-5:** red, implement, green, stamp the fingerprint into summary output + shadow banner, run gates (golden UNCHANGED because normalization must strip the new field OR the golden hash is re-recorded once with the ledgered justification "additive field", controller pre-approves the re-record for this task only), commit `constraint: config fingerprint stamped end-to-end, single-source defaults`.

### Task 7: Truthful-latency floor and fantasy watermark (constraint 1) + fee always-on (constraint 2)

**Files:**
- Modify: `crates/pm-backtest/src/config.rs` (or where latency/fee params resolved), `crates/pm-app/src/main.rs` (add `--fantasy` flag), summary output

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn latency_below_750_without_fantasy_is_rejected() {
    let err = validate_run_params(500, false).unwrap_err();
    assert!(err.to_string().contains("--fantasy"));
}
#[test]
fn fantasy_runs_are_watermarked() {
    let meta = run_metadata(500, true);
    assert_eq!(meta.watermark.as_deref(), Some("FANTASY"));
}
```

- [ ] **Step 2:** Implement: `--latency-ms` default 750; any value below 750 errors unless `--fantasy`; with `--fantasy`, summary JSON gains `"watermark": "FANTASY"` and out-file names gain a `FANTASY-` prefix. Audit for any flag that disables the fee curve (`grep -rn "fee" crates/pm-app/src/main.rs crates/pm-backtest/src` for a zero/off path); the fee coefficient stays configurable (0.07 default) but there must be no code path that skips fee application entirely; if one exists, remove it.

- [ ] **Step 3:** Gates. The golden script pins `--latency-ms 1250` explicitly, so it must still pass UNCHANGED. Commit `constraint: truthful-latency floor with FANTASY watermark; fee application not optional`.

### Task 8: Jittered replay and scorecard (constraints 3, 4, 5)

**Files:**
- Create: `crates/pm-backtest/src/jitter.rs`, `crates/pm-backtest/src/scorecard.rs` (extend the moved result_summary)
- Modify: `crates/pm-app/src/main.rs` (`--jitter N`, `--jitter-latency-spread-ms`, window labeling), summary output

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn jitter_zero_is_bitwise_baseline() { /* run_with_jitter(cfg, 0) == run(cfg) */ }
#[test]
fn jitter_runs_use_distinct_seeded_latencies() {
    let ls = jitter_latencies(1250, 250, 5, 42);
    assert_eq!(ls.len(), 5);
    assert!(ls.iter().all(|l| (1000..=1500).contains(l)));
    assert_eq!(ls, jitter_latencies(1250, 250, 5, 42)); // deterministic per seed
}
#[test]
fn single_window_result_is_labeled_unvalidated() { /* scorecard(one_window).validation == "UNVALIDATED" */ }
```
Seeded RNG must be an explicit `--jitter-seed` param (Date/entropy-free, reproducible).

- [ ] **Step 2:** Implement: `--jitter N` runs N replays at seeded latencies uniform in [base-spread, base+spread], scorecard reports per-jitter P&L plus p10/p50/p90; headline field is the SPREAD not the point estimate. Scorecard also computes: fractional-sizing report at `--bankroll` (default 2800) with 0.82 realization haircut alongside raw, and the 5-share-floor ruin check (flag if any clip under floor at the given bankroll). Window labeling: `--window-label feb2026` etc.; a summary lacking the full validated set {feb,mar,apr,may,jun} is stamped UNVALIDATED.

- [ ] **Step 3:** Gates (golden unchanged: jitter defaults to 0). Commit `constraint: jittered replay spread, multi-window labeling, sizing-realism scorecard`.

### Task 9: TWAP settlement eras (constraint 9)

**Files:**
- Create: `crates/pm-backtest/src/settlement.rs`
- Modify: `crates/pm-backtest/src/engine.rs` (resolution path), `crates/pm-backtest/src/scorecard.rs` (per-era breakdown), `crates/pm-types` if a SettlementEra enum belongs there

**Interfaces:**
- Produces:
```rust
pub enum SettlementEra { Snapshot, Twap30, Twap60 }
pub fn settlement_era(market_close_utc: DateTime<Utc>, duration: MarketDuration) -> SettlementEra
pub fn venue_taker_delay_ms(at_utc: DateTime<Utc>) -> u64  // 250 before 2026-08-17T11:00Z, 50 after (earlier eras per spec table)
pub fn resolve_outcome(era: SettlementEra, tape: &SpotWindow, strike: f64) -> Outcome
```

- [ ] **Step 1: Failing tests (synthetic tapes)**

```rust
#[test]
fn era_boundaries() {
    assert_eq!(settlement_era(utc("2026-08-06T23:59:00Z"), FiveMin), Snapshot);
    assert_eq!(settlement_era(utc("2026-08-07T00:01:00Z"), FiveMin), Twap30);
    assert_eq!(settlement_era(utc("2026-08-14T00:01:00Z"), FiveMin), Twap60);
    assert_eq!(settlement_era(utc("2026-08-10T00:00:00Z"), FifteenMin), Twap60);
    assert_eq!(settlement_era(utc("2026-08-20T00:00:00Z"), Hourly), Snapshot); // hourly stays candle/print based
}
#[test]
fn late_wick_flips_snapshot_but_not_twap() {
    // tape: price sits 10bps BELOW strike for 290s, spikes above only in the last 2s
    let tape = wick_tape();
    assert_eq!(resolve_outcome(Snapshot, &tape, K), Outcome::Up);
    assert_eq!(resolve_outcome(Twap60, &tape, K), Outcome::Down);
}
```

- [ ] **Step 2:** Implement. Engine behavior: when `--use-outcome-label` supplies a label, the label wins (historical truth) but the engine ALSO computes the era-model outcome and the scorecard reports the disagreement rate per era (a free model-quality diagnostic). When labels are absent, the era model resolves. `--latency-ms` remains the TOTAL modeled chain latency (unchanged semantics; the golden hash must not move). `venue_taker_delay_ms` feeds validation and reporting instead: a run whose total latency is below the era's venue delay is rejected like a sub-750 fantasy run (same `--fantasy` escape + watermark), and the summary reports the era's venue delay alongside the configured total.

- [ ] **Step 3: Strike-basis refusal (constraint 8).** The resolved config carries `spot_source` and `strike_source`; validation rejects a mismatch (e.g. binance spot belief with official/chainlink strikes) with an error naming docs/PROD.md's basis doctrine, unless `--allow-mixed-basis` (research escape hatch, watermarked in the summary like FANTASY). Failing test first:

```rust
#[test]
fn mixed_basis_is_rejected() {
    let err = validate_basis(SpotSource::Binance, StrikeSource::Official, false).unwrap_err();
    assert!(err.to_string().contains("basis"));
}
```

- [ ] **Step 4:** Gates. Golden day is 2026-06-25 (Snapshot era, labels on, same-basis config) so the hash is unchanged. Commit `constraint: TWAP settlement eras, era-aware venue delay, per-era scorecard, mixed-basis refusal`.

### Task 10: TWAP belief primitive (constraint 10)

**Files:**
- Create: `crates/pm-alpha/src/fair_value_twap.rs`
- Modify: `crates/pm-alpha/src/lib.rs` (export)

**Interfaces:**
- Produces:
```rust
/// P(TWAP over the final `w` seconds >= strike), under driftless GBM approx.
/// t_rem: seconds to market close. locked: Some((elapsed_in_window_s, partial_avg)) once t_rem < w.
pub fn twap_digital(spot: f64, strike: f64, sigma_per_sqrt_s: f64, t_rem_s: f64, w_s: f64, locked: Option<(f64, f64)>) -> f64
```
Outside the window (t_rem >= w): effective variance uses (t_rem - w) + w/3 seconds. Inside: the average is (elapsed*partial_avg + remaining*E[future avg])/w with the future-leg variance integral over the remaining sub-window; document the closed form derived in comments minimal enough to verify.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn reduces_to_classic_digital_as_window_shrinks() {
    let classic = digital_p_up(S, K, SIG, 300.0);
    let twap = twap_digital(S, K, SIG, 300.0, 0.001, None);
    assert!((classic - twap).abs() < 1e-6);
}
#[test]
fn narrower_effective_variance_outside_window() {
    // ATM exactly: both 0.5; slightly ITM: twap prob is FURTHER from 0.5 than classic (less time-variance)
    let classic = digital_p_up(S_itm, K, SIG, 300.0);
    let twap = twap_digital(S_itm, K, SIG, 300.0, 60.0, None);
    assert!(twap > classic);
}
#[test]
fn locked_average_dominates_at_the_end() {
    // 55 of 60s elapsed with partial_avg well above K, spot now below K: probability stays near 1
    let p = twap_digital(S_below, K, SIG, 5.0, 60.0, Some((55.0, K * 1.002)));
    assert!(p > 0.95);
}
#[test]
fn monotone_in_spot_and_bounded() { /* p in (0,1), increasing in spot, prop-test over grid */ }
```

- [ ] **Step 2-3:** Red, implement, green; do NOT wire into exo_fade (it dies in Task 12); the primitive is framework capital for the next strategy. Gates (golden untouched: pm-alpha addition only, equivalence PASS must hold). Commit `constraint: TWAP-aware digital belief primitive with locked-average handling`.

### Task 11: CONSTRAINTS.md

**Files:**
- Create: `docs/CONSTRAINTS.md`
- Modify: `README.md` (pointer swap: it currently says CONSTRAINTS.md is a Plan 2 deliverable; now it exists)

One page, ten numbered rules from the spec (section 4, constraints 1-10), each with three lines: the rule, the evidence (one number + source doc), where the enforcement lives (file + test name from Tasks 6-10; rules 7 and 8 cite the existing doctrine docs and the loader behavior). No TBDs. Verify every cited test exists (`cargo test <name> --workspace -- --list` style check or grep). Commit `docs: CONSTRAINTS.md, the ten rules with enforcement pointers`.

### Task 12: The kill

**Files:**
- Delete: `crates/pm-strategy/src/exo_fade.rs`, `crates/pm-strategy/src/regime.rs`, MayJuneFade config alias, `crates/pm-app/src/bin/exo_fade_equivalence.rs` (and its equivalence module in pm-alpha IF exo_fade-specific; keep any generic equivalence machinery), `crates/pm-model/` (after Step 3 trace), `--profile` flag (now fully dead)
- Modify: `crates/pm-strategy/src/lib.rs` (StratId = {Noop} + slim Ctx), `crates/pm-backtest` (model eval made pluggable then default-off), `crates/pm-shadow/src/lib.rs` (generalized over `pm_strategy::Strategy`), `crates/pm-app/src/main.rs` (CLI thinned), golden harness re-anchor
- Create: `crates/pm-backtest/tests/fixture_strategy.rs` (a deterministic tests-only `ThresholdFade` fixture implementing Strategy: enter side X when book price crosses a fixed threshold; ~60 lines)

- [ ] **Step 1: Re-anchor the golden harness FIRST.** Add the fixture strategy; run `golden_replay.sh` variant against the fixture (`--strategies fixture` gated to test/debug builds or a `--allow-fixture` flag), record `tests/golden/day-2026-06-25-fixture.sha256`. From here the gate is: fixture golden IDENTICAL + workspace tests. (The exo_fade golden and the equivalence gate retire with the strategy.)
- [ ] **Step 2: Slim Ctx.** Delete write-only fields (btc/eth_net_exposure_shares confirmed write-only in the phase 1 final review; re-verify by grep before deleting each field). Fix runner writes accordingly.
- [ ] **Step 3: Decouple and delete pm-model.** Make the canonical model eval a pluggable trait on the engine (default: none); re-trace `pm_model` refs (the Task 11/phase-1 OPEN-QUESTIONS list); delete the crate and the OPEN-QUESTIONS deferral entry (mark resolved with date).
- [ ] **Step 4: Delete exo_fade, MayJuneFade, regime.rs, the equivalence bin; thin the CLI** (remove `--profile` entirely, remove strategy-specific flags; `--strategies` accepts `noop` and, behind the fixture gate, `fixture`).
- [ ] **Step 5: Generalize pm-shadow** over the Strategy trait (constructor takes `Box<dyn Strategy>`; the fade-specific decide calls become the trait call; keep the JSONL stream shape stable). Fold in the parked warm_spot_1s fix: propagate non-array Binance warmup responses as errors (the phase-1 parked Important) and add the missing pagination/error-path tests (mock the fetch seam or extract the parse step to a testable fn).
- [ ] **Step 6: Sweep the deferred minors** ledgered in phase 1: parse_strategies unreachable branch + restored test; stale BackToExplore comments in runner/backtest code; autoresearch_ml_loop.sh bonereaper default (point examples at `noop`); archive or rewrite docs/aws-backtest-runbook.md (archive to docs/archive/2026-07/ with a pointer is acceptable).
- [ ] **Step 7: Gates** (build zero warnings, workspace tests, fixture golden IDENTICAL) and README/CONTEXT.md updated to the zero-strategies end state. Commit as a short series with `reset:` prefixes (Ctx slim, pm-model removal, exo_fade kill, pm-shadow generalization, docs), each compiling.

### Task 13: Final whole-branch review

Standard SDD final review over the whole phase 2 range on the most capable model: cross-task consistency, golden-harness integrity (was any hash re-recorded without a ledgered justification?), constraint coverage vs spec section 4 (all ten rules enforced + tested), the kill's completeness (zero strategy code outside Noop/fixture; `grep -rn "exo_fade\|ExoFade\|MayJuneFade\|mayjune" --include='*.rs' crates/` clean), and ledger-minor triage.

## Deliberately NOT in this plan

- Cloud runner, S3 sync-up, data/ deletion, Telonex backfill, TWAP feed ingestion: Plan 3 (blocked on Telonex re-enable for the backfill part; runner script is not).
- Any new tradeable strategy (the TWAP fade revalidation etc. happen ON this framework afterward).
- polymarket-agent changes (tradeIDs fix + fast_live landmine: Plan 3's agent PR).
