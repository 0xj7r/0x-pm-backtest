# Agent Handoff: Regime Gates + High-Variance Strategy Research

**Date:** 2026-06-19  
**Repo:** `/Users/jackreid/go/polymarket-backtest` (also deployed ad-hoc to Dublin — not a git clone)  
**Branch:** `main` (local changes **uncommitted** at handoff time)  
**Audience:** Next coding agent building regime routing, satellite strategies, and live parity

---

## 1. Executive summary

### What we accomplished this session

1. **Implemented per-decision regime gates in SSOT** (`pm_alpha::decide_entry`) — not calendar-day gates; evaluated on every entry tick using rolling 30m spot path classification.
2. **Wired flags end-to-end:** shadow CLI, alpha harness, `exo_fade` strategy, frozen configs, tests.
3. **Ran regime gate sweep** on Dublin `shadow-final` JSONL (Jun 14–19); selected **`--skip-calm` + `--skip-expanded-mixed`**.
4. **Deployed to Dublin:** rebuilt release `pm-app`, restarted `shadow-final` with new gates. Live execution still blocked by `fade.kill`.
5. **Researched complementary strategies** for high-variance tape (BR2 late_favourite, cluster router, tail sleeve) — documented below as build backlog.

### What the next agent should build

**Primary goal:** A **regime-aware multi-strategy stack** where fade (F1) runs in chop/expanded tape and a **directional satellite** (BR2 `late_favourite`) runs on trend/reversal days — without breaking shadow↔live decision parity.

**Do NOT re-arm live** until ~48h clean paper parity soak with new gates + validated satellite shadow twin.

---

## 2. Production state (Dublin)

| Item | Value |
|------|-------|
| Host | `ubuntu@34.242.101.97` (SSH key: `~/.ssh/whale_pair_dublin_ed25519.pem`) |
| Repo path (server) | `~/pm-backtest` (**not a git repo** — code synced via rsync) |
| Binary | `~/pm-backtest/target/release/pm-app` |
| shadow-final pid | `1269714` (restarted 2026-06-19 ~08:46 UTC) |
| shadow-final out | `~/data/pm-alpha/shadow-final/shadow-*.jsonl` |
| shadow-final log | `~/data/pm-alpha/shadow-final.log` |
| Paper executor | `~/deploy-main/polymarket-agent/target/release/shadow_exec_tail --paper` |
| Live kill switch | `~/fade.kill` **ACTIVE** (touch file exists) |
| Parity (recent) | `matched=188, orphan=0, missed_ref=0` (window 120s) |

### shadow-final command line (frozen prod)

```bash
pm-app shadow \
  --edge-threshold 0.12 \
  --perp-price-weight 0.75 \
  --exit-after-s 0 \
  --rearm-edge 0.08 \
  --max-clips 2 \
  --min-entry-sigma-bps 3.0 \
  --vol-estimator realized \
  --vol-lookback-s 3600 \
  --skip-saturday \
  --out-dir ~/data/pm-alpha/shadow-final \
  --skip-spot-misalign-s 30 \
  --min-entry-ask 0.45 \
  --skip-open-fav-gap \
  --open-fav-p-min 0.88 \
  --open-fav-ask-max 0.62 \
  --open-fav-secs 300 \
  --skip-calm \
  --skip-expanded-mixed
```

SSOT for gated flags array: `scripts/shadow_final_gated_flags.sh`  
Restart script: `scripts/shadow_final_restart_gated.sh`

### Deploy procedure (Dublin)

```bash
# From local machine — rsync changed files to correct crate paths (NOT crates/ root)
RSYNC_SSH="ssh -i ~/.ssh/whale_pair_dublin_ed25519.pem"
rsync -avz -e "$RSYNC_SSH" crates/pm-alpha/src/decide.rs ubuntu@34.242.101.97:~/pm-backtest/crates/pm-alpha/src/
# ... repeat per file or rsync whole src trees

# On Dublin
ssh ubuntu@34.242.101.97 'cd ~/pm-backtest && source ~/.cargo/env && cargo build --release -p pm-app'
bash scripts/shadow_final_restart_gated.sh   # or remote equivalent
```

**Warmup:** After restart, vol3600 buffer needs ~1h before entries resume.

### Monitoring commands

```bash
ssh ubuntu@34.242.101.97 '~/pm-backtest/scripts/paper_parity_status.sh'
ssh ubuntu@34.242.101.97 'grep parity_stats ~/data/pm-alpha/shadow_exec_tail.log | tail -5'
ssh ubuntu@34.242.101.97 'python3 /tmp/score_regime_gate_sweep.py --shadow-dir ~/data/pm-alpha/shadow-final --since 2026-06-14'
```

---

## 3. Architecture: decision SSOT

### Single source of truth

All entry decisions for fade family flow through:

```
AlphaModel::belief → DecisionInputs → decide_entry(DecideConfig) → EntryDecision
```

**File:** `crates/pm-alpha/src/decide.rs`  
**Tests:** `decide::tests::*` including `skips_calm_low_vol_regime`, `skips_expanded_mixed_regime`

### Regime classification (decision-time)

**File:** `crates/pm-alpha/src/regime.rs`

| Label | Rule (30m lookback, 10s steps) |
|-------|----------------------------------|
| `calm_low_vol` | `vol_180s_bps < 4.5` |
| `clean_directional` | vol ≥ 4.5, efficiency ≥ 0.30 |
| `expanded_high_flip` | vol ≥ 4.5, efficiency < 0.30, sign_flip_rate ≥ 0.55 |
| `expanded_mixed` | vol ≥ 4.5, not directional, flip rate < 0.55 |

`regime_gates_pass()` in `decide.rs` runs **before side pick** (line ~422). If `regime_at_decision` is `None`, gates pass (permissive).

### Where `regime_at_decision` is populated

| Path | Populated? |
|------|------------|
| `pm-shadow` live telemetry | ✅ `regime::classify(&spot, now_ns)` at decision |
| `pm-alpha` harness replay | ✅ in `harness/replay.rs` |
| `pm-strategy` `exo_fade` | ✅ wired this session (was `None`) |
| Equivalence tests | Often `None` (gates inert in those tests) |

### DecideConfig regime fields

```rust
pub skip_calm: bool,
pub only_calm: bool,
pub skip_expanded_mixed: bool,
pub skip_expanded_high_flip: bool,
```

### CLI flags (shadow + alpha harness)

```
--skip-calm
--only-calm
--skip-expanded-mixed
--skip-expanded-high-flip
```

Wired in:
- `crates/pm-app/src/main.rs` (Shadow + Alpha subcommands)
- `crates/pm-app/src/alpha.rs` → `HarnessConfig`
- `crates/pm-shadow/src/lib.rs` → `ShadowArgs`, `ShadowConfig`, `sync_decide_cfg()`
- `crates/pm-strategy/src/exo_fade.rs` → `ExoFadeConfig`, `decide_config()`

### Harness change

**Removed** market-open-level `calm_blocked` from `crates/pm-alpha/src/harness/replay.rs`. Regime gating is **only** via `decide_entry` now (per-decision).

### Frozen shadow config helpers

- `pm_alpha::frozen_fade_decide_config()` — `decide.rs`
- `pm_shadow::frozen_shadow_final_args()` — `lib.rs`
- Parity reference: `crates/pm-app/src/bin/exo_fade_equivalence.rs`

---

## 4. Code changes (uncommitted local diff)

```
 configs/mayjune_btc5m.toml                    |  +3 lines (regime flags)
 crates/pm-alpha/src/decide.rs                 |  +78 (regime_gates_pass, tests, DecideConfig fields)
 crates/pm-alpha/src/exo_fade_equivalence.rs   |  +3
 crates/pm-alpha/src/harness/replay.rs         |  -9 (removed calm_blocked)
 crates/pm-alpha/src/harness/types.rs          |  +4 (skip_expanded_mixed)
 crates/pm-app/src/alpha.rs                    |  +2
 crates/pm-app/src/main.rs                     |  +20 (CLI wiring)
 crates/pm-app/src/bin/exo_fade_equivalence.rs |  +3
 crates/pm-shadow/src/lib.rs                   |  +21
 crates/pm-strategy/src/exo_fade.rs            |  +13 (regime classify + config fields)
 scripts/shadow_final_gated_flags.sh          |  +2
 scripts/shadow_final_restart_gated.sh         |  echo update
?? configs/exo_fade_chop_router.toml           (new profile — see §7)
?? scripts/score_regime_gate_sweep.py         (new analysis script)
```

**Tests:** All pass at handoff:
```bash
cargo test -p pm-alpha -p pm-shadow -p pm-strategy -p pm-app
```

**Action for next agent:** Commit with message like `feat(decide): per-decision regime gates + shadow-final deploy flags`.

---

## 5. Empirical evidence

### Regime gate sweep (Jun 14–19, Dublin shadow-final baseline tape)

Script: `scripts/score_regime_gate_sweep.py`

| Combo | Total PnL | Jun 18 | Notes |
|-------|-----------|--------|-------|
| baseline | +$3,059 | −$1,064 | |
| **skip_calm + skip_exp_mixed** | **+$3,682** | **+$74** | **Deployed** |
| skip_calm only | +$3,668 | −$411 | |
| skip_exp_flip | +$3,059 | −$1,064 | No benefit |

Jun 14–16 unchanged under winner combo.

### Regime PnL breakdown (same tape, pre-gates)

| Regime | n | PnL |
|--------|---|-----|
| unlabeled | 1021 | +$3,658 |
| expanded_mixed | 449 | +$10 |
| calm_low_vol | 75 | **−$609** |
| expanded_high_flip | 5 | ~$0 |
| clean_directional | 6 | +$23 |

**Jun 18 loss anatomy:** calm −$653, expanded_mixed −$485 — **not** high_flip.

### Fav loading (deferred)

User explicitly deferred 0.65–0.75 fav sleeve. Touch band showed ~74% hit in anecdotal analysis; **out of scope** for current build.

---

## 6. High-variance strategy research (build backlog)

### 6.1 Problem statement

"High variance" is **not one regime**. The repo has three classifiers:

1. **`pm-alpha::regime`** (4 labels) — used for fade gates
2. **`pm-strategy::regime::MarketRegimeCluster`** (8 fill-time clusters) — used by BTE/BR2
3. **Daily spot labels** (`scripts/june_regime_compare.py`) — `directional_trend`, `chop_whipsaw`, `high_vol`

v1 alpha regime puts ~80% in `expanded_mixed` (`docs/alpha-roadmap.md`) — too coarse for routing.

### 6.2 Validated strategies (strategy hunt)

| ID | Strategy | High-var role | VERIFY / hunt result |
|----|----------|---------------|----------------------|
| **F1** | `exo_fade` BSM disagreement | Core in expanded/chop | **VIABLE** +$11,841 |
| **F2** | aligned continuation | Trend | **REJECT** −$1,025 |
| **F7** | hold-to-resolution | Tail capture | REJECT (worst day −$499) |
| **W1** | `back_to_explore` | Two-sided ladder | REJECT −$85; cluster polarity unstable |
| **W2** | `paired_mm` | Whipsaw MM | REJECT −$277 |
| **W3** | `bonereaper_v2` | Directional taker | WEAK +$327 @ $1K walk-forward |

**Thesis classes** (`docs/research/strategy-hunt/07-strategy-forward-plan.md`):
- **Class A:** BSM disagreement fade (always-on core)
- **Class B:** Directional satellite (NOT aligned — use BR2 lanes)
- **Class C:** Tail convexity sleeve (`B_tail` bucket +$68/trade in thesis decomp)

### 6.3 BR2 cluster evidence (directional satellite candidate)

**Doc:** `docs/global_regime_classifier_router.md`

| Cluster | BR2 PnL | Win rate |
|---------|---------|----------|
| `expanded_reversal_pressure` | +$2,920 | 76.6% |
| `clean_directional_path` | +$1,958 | 88.1% |
| `expanded_high_flip` | +$1,540 | 74.4% |

BR2 `late_favourite` config highlights (`bonereaper_v2.rs`):
- Ask band 0.70–0.97
- `max_whipsaw_score: 0.75`
- Range throttle, fragile high-cert guards

### 6.4 Cluster router (research winner, not deployed)

Adaptive BTE+BR2 router on overlapping markets (Feb–Mar sample):
- Combined test: **+$3,138** vs BR2-only +$2,294
- Best adaptive: +$5,672, max DD 13.55%
- **Caveat:** `expanded_high_flip` BTE polarity flips by period (−$701 early vs +$196 May)

Artifacts referenced in docs (may be missing locally):
- `data/runs/regime_clusters/router_*`
- `configs/back_to_explore_cluster_policy_combined_no_boost.json`

### 6.5 June daily router (Python only, not in Rust SSOT)

**File:** `scripts/june_regime_compare.py`

```
IF trend_eff >= 0.30 AND abs(spot_ret_1d) >= 35bps:
    RUN br2_late_favourite; SKIP fade underdog (ask < 0.65)
ELIF chop_whipsaw:
    risk_off OR gated fade
ELIF low_vol AND fade_pnl > 200:
    fade OK
```

**Next agent:** Port daily/rolling spot router into a **sidecar or pre-route feature layer** — do NOT hack into `decide_entry` without design review (`docs/global_regime_classifier_router.md`).

### 6.6 Explicitly rejected for high-variance (do not rebuild without new evidence)

- F2 aligned mode
- Momentum drift belief replacement
- paired_mm on VERIFY
- BTE standalone global deployment

---

## 7. Config profiles (local, partially new)

### `configs/mayjune_btc5m.toml`

May/June tuned fade profile for walk-forward `mayjune_fade` strategy:
- `skip_expanded_high_flip = true`
- `skip_expanded_mixed = false`
- `skip_calm = false`

**Note:** shadow-final prod uses **opposite** on mixed/calm (`skip_expanded_mixed=true`, `skip_calm=true`) based on **live Jun 14–19 sweep**, not May backtest alone. Reconcile before promoting mayjune profile to prod.

### `configs/exo_fade_chop_router.toml` (new, untracked)

Experimental walk-forward profile:
- `skip_expanded_high_flip = true`
- Comments for future `skip_expanded_mixed` + directional complement

---

## 8. Recommended build plan (prioritized)

### Phase 0 — Hygiene (do first)

- [ ] **Commit** all local changes + `score_regime_gate_sweep.py`
- [ ] **Git-init or clone** on Dublin (currently rsync-only — fragile)
- [ ] Confirm shadow-final post-warmup entries show `regime` on `would_enter` and fewer calm/mixed entries
- [ ] Run 48h parity soak; document in `scripts/paper_parity_status.sh` output

### Phase 1 — Shadow BR2 satellite (highest ROI)

**Goal:** Parallel `shadow-br2` process on same feeds/markets logging `would_enter` for `bonereaper_v2` `late_favourite` lane — **no live execution**.

Tasks:
1. Add `pm-app shadow` lane mode OR separate `shadow-br2` out-dir (pattern: existing `shadow-lane` for late-fav)
2. Wire `BonereaperV2Config` champion/late_favourite profile from TOML
3. Emit JSONL events compatible with `score_shadow_gate_ab.py` / resolution join
4. Parity: harness VERIFY trades vs shadow would_enter for same window

**Key files:**
- `crates/pm-shadow/src/lib.rs` — extend or fork shadow core
- `crates/pm-strategy/src/bonereaper_v2.rs` — lane configs, whipsaw guards
- `crates/pm-app/src/walkforward.rs` — `StratId::BonereaperV2`
- `configs/` — BR2 TOML profiles

### Phase 2 — Regime + thesis telemetry dashboard

Extend `scripts/score_regime_gate_sweep.py`:
- Break PnL by `regime × ask_band × fade_vs_book` (underdog vs favourite)
- Per-day regime attribution
- Compare baseline vs gated vs hypothetical BR2 routing

Optional: log `gate_blocked_reason` on shadow `would_enter` skip events (currently silent skips).

### Phase 3 — Pre-route feature layer (router foundation)

**Design doc:** `docs/global_regime_classifier_router.md`

Emit at market open (or first decision tick):
```json
{
  "regime_label": "expanded_reversal_pressure",
  "regime_scores": {...},
  "strategy_weights": {"exo_fade": 0.6, "bonereaper_v2": 0.4, "risk_off": 0.0},
  "whipsaw_score": 0.42,
  "reasons": ["range_so_far>0.20", "reversal_pressure>0.30"]
}
```

**Files:**
- `crates/pm-strategy/src/regime.rs` — `MarketRegimeCluster`, `WhipsawRiskSnapshot`
- `scripts/router_decision_log_dataset.py`
- `scripts/router_policy_search.py`

**Do NOT** use fill-time-only features for pre-route decisions.

### Phase 4 — Harness validation

```bash
# Regime gate backtest sweep (SSOT gates, not Python replay)
./scripts/whipsaw_backtest.sh
VARIANT=combo WINDOWS="VERIFY HOLDOUT" ./scripts/whipsaw_backtest.sh

# F6 expanded-only
WINDOWS=VERIFY STRATEGIES=F1 ./scripts/strategy_hunt_matrix.sh  # with --skip-calm

# BR2 cluster PnL refresh
python3 scripts/strategy_regime_clusters.py  # needs markets.jsonl artifacts

# June regime report
python3 scripts/june_regime_compare.py
```

### Phase 5 — Tail sleeve (Class C)

- Enable `tail_regime_boost_*` in BR2 for `expanded_reversal_pressure` only
- EC2 configs: `bonereaper_v2_convex_reversal.toml` (see `scripts/launch_ec2_convex_validation.sh`)
- Target: `B_tail` bucket economics (+$68/trade shadow baseline, thin volume)

### Phase 6 — Live re-arm (only after parity + shadow BR2 soak)

1. Remove `~/fade.kill` on Dublin
2. Small clips ($25)
3. Trade-by-trade decision parity vs shadow-final JSONL (postmortem: matched config still lost $700 without parity)

---

## 9. Key files reference

| Purpose | Path |
|---------|------|
| Entry SSOT | `crates/pm-alpha/src/decide.rs` |
| Regime classifier | `crates/pm-alpha/src/regime.rs` |
| Harness replay | `crates/pm-alpha/src/harness/replay.rs` |
| Harness config | `crates/pm-alpha/src/harness/types.rs` |
| Shadow runner | `crates/pm-shadow/src/lib.rs` |
| Live fade strategy | `crates/pm-strategy/src/exo_fade.rs` |
| BR2 strategy | `crates/pm-strategy/src/bonereaper_v2.rs` |
| Fill-time clusters | `crates/pm-strategy/src/regime.rs` |
| CLI | `crates/pm-app/src/main.rs` |
| Alpha harness CLI | `crates/pm-app/src/alpha.rs` |
| Walk-forward | `crates/pm-app/src/walkforward.rs` |
| Parity equiv tests | `crates/pm-app/src/bin/exo_fade_equivalence.rs` |
| Prod gated flags | `scripts/shadow_final_gated_flags.sh` |
| Prod restart | `scripts/shadow_final_restart_gated.sh` |
| Gate sweep analysis | `scripts/score_regime_gate_sweep.py` |
| Whipsaw backtest | `scripts/whipsaw_backtest.sh` |
| June router research | `scripts/june_regime_compare.py` |
| Router design | `docs/global_regime_classifier_router.md` |
| Strategy forward plan | `docs/research/strategy-hunt/07-strategy-forward-plan.md` |
| Thesis buckets | `docs/research/strategy-hunt/06-thesis-decomposition.md` |
| Quant signals / regime | `docs/research/strategy-hunt/05-quant-signals.md` |
| ETH regime confirm | `docs/research/autoloop/eth_complete.md` |

---

## 10. Frozen DecideConfig (shadow/live parity checklist)

| Field | shadow-final value |
|-------|-------------------|
| `edge_threshold` | 0.12 |
| `min_entry_sigma_bps` | 3.0 |
| `min_marginal_edge` | 0.04 |
| `rearm_edge` | 0.08 |
| `max_clips` | 2 |
| `clip_cooldown_ms` | 5000 |
| `exit_after_s` | 0 (hold) |
| `stop_before_close_s` | 90 |
| `skip_saturday` | true |
| `entry_mode` | Fade |
| `notional_usdc` | 50 (shadow telemetry) |
| `skip_spot_misalign_s` | 30 |
| `min_entry_ask` | 0.45 |
| `skip_open_fav_gap` | true |
| `open_fav_p_min` | 0.88 |
| `open_fav_ask_max` | 0.62 |
| `open_fav_secs` | 300 |
| **`skip_calm`** | **true** (new) |
| **`skip_expanded_mixed`** | **true** (new) |
| `skip_expanded_high_flip` | false |
| `only_calm` | false |

AlphaModel: `vol_lookback_s=3600`, `perp_price_weight=0.75`, `vol_estimator=realized`.

---

## 11. Validation windows (no leakage)

| Window | Dates | Use |
|--------|-------|-----|
| TUNE | 2026-02-12 → 2026-04-30 | Selection only |
| VERIFY | 2026-05-07 → 2026-05-18 | Confirm frozen config |
| HOLDOUT | 2026-05-19 → 2026-05-28 | One-shot |
| June+ | sealed | Live shadow only — **never select on June** |

---

## 12. Open questions / risks

1. **mayjune_btc5m vs shadow-final gate mismatch** — TOML skips high_flip not mixed/calm; prod does opposite for mixed/calm. Document which is canonical before merging profiles.

2. **Regime label sparsity early in session** — Many Jun 14–15 entries were `unlabeled` (regime field null). Gates pass when null. Consider logging skip reason when regime unknown vs known-blocked.

3. **Directional trend days** — New gates don't fix Jun 17-style fade-underdog bleed on trend days. BR2 satellite is the identified gap.

4. **Dublin not git-backed** — rsync deploy is error-prone (this session accidentally rsynced to `crates/` root once). Fix infra.

5. **expanded_high_flip sample size** — Only 5 labeled trades Jun 14–19; don't over-fit skip_exp_flip.

6. **ConvexBookStrategy** — In `engine_driver` only, not `StratId` / walk-forward. Tail work may need port.

7. **48h parity soak** — Required before live re-arm; user agreed to keep `fade.kill` until clean.

---

## 13. Suggested first command for next agent

```bash
cd /Users/jackreid/go/polymarket-backtest
git status
cargo test -p pm-alpha -p pm-shadow -p pm-strategy -p pm-app
# Read design before coding router:
cat docs/global_regime_classifier_router.md
cat crates/pm-strategy/src/bonereaper_v2.rs | head -200
# Verify Dublin shadow post-warmup:
ssh -i ~/.ssh/whale_pair_dublin_ed25519.pem ubuntu@34.242.101.97 \
  'tail -20 ~/data/pm-alpha/shadow-final.log; ls -lt ~/data/pm-alpha/shadow-final/shadow-*.jsonl | head -3'
```

**First implementation ticket:** Shadow BR2 `late_favourite` twin on Dublin feeds with JSONL telemetry + harness VERIFY parity check.

---

## 14. User preferences (carry forward)

- **Fav loading sleeve (0.65–0.75 band):** explicitly deferred
- **Focus:** robust entry engine for current use-case (fade + gates), not new belief models
- **Live:** paper only until parity soak; small clips when re-arming
- **SSOT:** all gates in `decide_entry`, not post-hoc Python on shadow JSONL for production decisions

---

*End of handoff. Questions → check transcript at `.grok/sessions/.../updates.jsonl` or git diff on `main`.*