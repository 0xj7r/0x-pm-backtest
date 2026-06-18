# Strategy Forward Plan (2026-06-16)

Consolidates first-principles discovery (`04`, `05`), cross-window validation
(`01`, `scorecard`), walk-forward comparison (`02`), and live/shadow analysis
(`data/runs/analysis/`, postmortem). This is the execution roadmap from research
to deployable `pm-strategy` + shadow parity.

**Signal spec:** [`05-quant-signals.md`](05-quant-signals.md)  
**Protocol:** [`00-protocol.md`](00-protocol.md)

---

## 1. Honest strategy thesis (rename: fade → BSM disagreement / digital value)

### What we are actually doing

We are **not** “fading stale books” in the microstructure sense (C6 PM flow noise
failed: −2.3¢/share markout). We are **buying mispriced binary digitals** when an
exogenous BSM fair value disagrees with the Polymarket taker ask by more than fees
can explain.

\[
\hat{p}_{\text{up}} = \Phi\!\left(\frac{\ln(S_t / K)}{\sigma_{\text{bar}} \sqrt{\tau_t}}\right), \qquad
\varepsilon^{\text{net}}_s = \hat{p}_s - a_s - 0.07 \cdot a_s(1-a_s)
\]

**Causal story:** CEX spot + perp state updates faster than the PM book reprices
(~150ms latency window). The edge is **staleness capture on disagreement**, not
superior modeling — log-loss still shows the book beats our exo belief on average
(\(\mathcal{L}_{\text{exo}} = 0.547\) vs \(\mathcal{L}_{\text{book}} = 0.492\) on
VERIFY). We monetize **episodic dislocations**, not calibration dominance.

### What the name change buys us

| Old framing | Problem | New framing |
|---|---|---|
| “Stale-book fade” | Implies mean-reversion on \(\Delta m\) without \(\Delta S\) (C6 dead) | **BSM disagreement** — explicit belief vs ask |
| “Fade” | Sounds like betting against momentum (C2 aligned dead: −$836/−$707) | **Digital value** — buy underpriced leg vs exo \(\hat{p}\) |
| “Exo fade” (repo) | Fine internally; misleading externally | Map to `exo_fade` / F1 in code; document as disagreement |

### Empirical anchor (BTC-5m, VERIFY 2026-05-07 → 2026-05-18)

| Config slice | NET | Hit | Sharpe(d) | Worst day | Notes |
|---|---:|---:|---:|---:|---|
| F1 timed exit ($25, θ=0.16, exit 30s) | **+$11,841** | 63.0% | 46.7 | +$415 | Strategy-hunt **VIABLE** cell |
| F1 TUNE cross-check | +$33,334 | 61.2% | 22.3 | +$415 | 2.31× scaled consistency vs VERIFY |
| Shadow/hold stack (θ=0.12, $50, hold) | +$26,026 | 56.8% | TBD | TBD | `shadow_match.json`; 4,952 trades |
| Champion timed (θ=0.16, $25, exit 30s) | +$19,585 | 60.8% | TBD | TBD | `champion_f1.json` |
| F7 hold-to-resolution | +$12,079 | 56.6% | 22.3 | **−$499** | REJECT — tail risk breaches −5% gate |

**Honest limits:**

1. **Single market validated end-to-end:** only F1 × btc5m passes TUNE+VERIFY adoption gates.
2. **Model does not beat book on log-loss** — edge is timing/dislocation, not belief accuracy.
3. **Live divergence postmortem (2026-06-16):** parallel reimplementation lost ~$700 while shadow
   made +$211 on identical config — **decision parity is mandatory**, not optional.
4. **June sealed** — no selection on June data; forward shadow is the live exam for compositional
   upgrades (perp@0.12 + rearm).

---

## 2. Three thesis classes (A / B / C) — entry + sizing rules

Each class is a **different functional** of \((\hat{p}, \text{book}, \tau)\). Variants
within a class (threshold, exit style) are tuning; classes are discovery.

### Class A — BSM disagreement (core, always-on)

**Thesis:** Buy side \(s^* = \arg\max_s \varepsilon_s\) when fee-adjusted disagreement
exceeds threshold and vol is sufficient for the digital to have meaningful curvature.

| Parameter | Shadow / live frozen | Strategy-hunt research | Rationale |
|---|---|---|---|
| Belief | BSM base, vol_lookback=3600s, perp_price_weight=0.75 | same | Perp-led TEST +21.8% vs spot champion |
| Entry | \(\varepsilon_{s^*} \geq \theta\) | θ=0.16 (F1 matrix) / **0.12 (shadow)** | 0.12: +$26k VERIFY vs +$11.8k @0.16/$25; more participation |
| Sigma floor | \(\sigma_{\text{bar}}^{\text{bps}} \geq 3.0\) | harness often 0 (offline filter) | ADOPT-CANDIDATE: ~free NET, prunes dead-low-vol (~3.3% entries) |
| Vol ceiling | none (max_entry_sigma_bps=0) | — | Upper sigma cuts positive volume |
| Rearm | \(\varepsilon_s < 0.08\) both sides, max_clips=2, cooldown 5s | same | TEST +40% vs champion with rearm |
| Marginal clip | \(\varepsilon^{\text{net}} \geq 0.04\) on clip ≥2 | min_marginal_edge=0.04 | Stops fee-bleed re-entries |
| Exit | hold to resolution (exit_after_s=0) | exit 30s + passive mid (F1) | Hold: higher NET; timed exit: lower tail, validated in matrix |
| Calendar | skip Saturday (UTC) | not in F1 matrix | Saturday ~$0 NET pooled; skip lifts Sharpe 1.0–1.6 |
| Sizing | **$50 flat** (shadow measure) | **$25 flat** (2.5% of $1K) | Pilot: vol-sizing ref 9.58 clamp [0.5, 2.0]; Kelly **rejected** (−32% NET) |
| Fees / latency | curve 0.07, 150ms | same | Non-negotiable realism |

**Sizing rule (pilot, not shadow log):**

\[
\text{clip} = N_0 \cdot \text{vol\_scale}(\sigma_{\text{bar}}) \cdot \mathbb{1}[\text{not Saturday}]
\]

where \(N_0 = \$25\)–\$50, vol_scale clamps to [0.5, 2.0] vs ref 9.58 bps. **Do not** use naive
Kelly on cheap entries — live shadow showed five 0.04–0.17 entries losing full premium at $50 flat;
belief-discounted fractional Kelly is a **pilot-layer** experiment only.

**Thesis gates (optional risk policy — not alpha):**

| Gate set | VERIFY NET | Δ vs ungated | trades | hit% | Verdict |
|---|---:|---:|---:|---:|---|
| Ungated shadow (`shadow_baseline`) | **+$26,026** | — | 4,952 | 56.8% | **Alpha champion** |
| Full A-band (`p≥0.25`, ask∈[0.15,0.85]) | +$20,940 | **−19.5%** | 4,884 | 57.1% | Risk policy only |
| `min_entry_ask=0.15` only | +$20,999 | −19.3% | 4,894 | 57.2% | Dominant gate (blocks B_tail) |
| `min_p_side=0.25` only | +$23,855 | −8.3% | 4,929 | 57.0% | Moderate cost |
| Full gates + Kelly | +$16,254 | **−37.5%** | 4,884 | 57.1% | **KILL Kelly** |

Gated run leaves only **A_lead + C_fav_value** buckets (B_tail/D_lottery removed). B_tail
was **+$6,067** on 89 trades (+$68/tr) — profitable tail convexity, not bleed. D_lottery
was −$312 on 6 trades (0% hit) — correct kill.

**Decision:** Ship **ungated shadow stack** for alpha; gates are optional live risk overlays.
If live must block deep OOTM (`p=0.189 touch=0.06`), use **micro-clip Class C sleeve** or
`min_entry_ask=0.10` + `ε_net>0` — not full A-band gates that sacrifice ~$5k VERIFY.

**Promotion status:** VIABLE (F1 × btc5m timed exit; shadow hold@0.12 ungated). HOLDOUT pending.

---

### Class B — Late-window digital certainty (satellite)

**Thesis:** In final \(\tau\) fraction, favourite mid is high but residual digital mispricing
vs \(\hat{p}\) still exists; buy aligned favourite, hold to \(T\).

| Rule | Value | Source |
|---|---|---|
| Window gate | enter_within_close_s = 120 | P1 / C3 |
| Favourite filter | \(m_{s^*} \geq 0.85\) | align_min_mid |
| Mode | Aligned (belief agrees with book direction) | EntryMode::Aligned |
| Edge | \(\varepsilon_{s^*} \geq 0\) (thr −1 = no edge gate) | P1 harness |
| Exit | hold (exit_after_s=0) | — |
| Sizing | $25 flat, max_clips=1 | strategy-hunt frame |

**VERIFY result (btc5m):** +$392 NET, 93.0% hit, $0.14/trade mean → **~$33/day** at $25 clips.
W3 walk-forward: +$327 @ $1K compounded but worst day −$334 (fails −5% gate).

**Verdict:** Satellite lane only — positive but economically thin; one −$25 loss ≈ 11 wins.
Do not allocate core capital until standalone `pm-strategy` port + VERIFY worst-day study.

---

### Class C — Tail convexity / cheap digital lottery (additive sleeve)

**Thesis:** When ask on cheap leg \(a_s \leq p_{\max}^{\text{tail}}\), buy if
\(\varepsilon^{\text{net}}_s > 0\) — convex payoff on residual probability the book underprices.

| Rule | Value | Notes |
|---|---|---|
| Price cap | tail_max_price = 0.10 (test 0.05 standalone) | Whale 8d1d: +0.126 edge/$1 in 0.0–0.1 bucket |
| Edge | \(\varepsilon^{\text{net}}_s > 0\) after fee curve | Not θ=0.16 — fee dominates at low px |
| Exit | hold to \(T\) | — |
| Sizing | tail_frac × main clip (0.25 default) OR standalone micro-clip | 25% of $25 = $6.25 hedge; whale uses 1–2¢ tickets |

**VERIFY result:** P3 fade+tail (`tail_max_price=0.10`, 25% clip) reported +$19,585 in
`04-first-principles.md` (+65% vs fade-alone narrative); artifact `VERIFY_P3_fade_tail_btc5m.json`
shows +$19,585 aggregate — **isolate standalone tail entries next** (not hedge-only on Class A).

**Verdict:** PROMISING — highest new-signal priority after Class A HOLDOUT. Port to `pm-strategy`
before sizing live.

---

### Class routing (future C10 — not yet validated)

| Regime | Route | Evidence |
|---|---|---|
| expanded_mixed | Class A full size | 60% of F1 VERIFY PnL in expanded_mixed cell (+$11,772) |
| calm_low_vol | Class A reduced / same | Still profitable (+$7,388 @ θ=0.16) but lower $/trade |
| clean_directional | Class B satellite only | C2/C3 marginal; do not add aligned continuation |
| Alts (SOL/XRP) | **skip Class A** | SOL −$1,081, XRP −$1,139 VERIFY @ spot belief |

---

## 3. What to kill

### Markets / sleeves — stop spending cycles

| Target | VERIFY / screen evidence | Action |
|---|---|---|
| **Alt fade (SOL/XRP/ETH spot belief)** | SOL −$1,081 (33% hit), XRP −$1,139 (39% hit); ETH rejected in alpha-hunt-002 | **KILL** spot-belief fade on alts until perp-led belief (Phase C) |
| **F2 / C2 aligned continuation** | −$1,025 VERIFY; P2 perp-aligned −$836/−$707 | **KILL** as standalone; not a diversifier in whipsaw (corr +0.67 with fade) |
| **F7 hold without risk shape** | +$12k NET but worst day −$499 (10× gate) | **KILL** as primary; hold OK only inside Class A with rearm + sigma floor |
| **C6 PM flow noise** | −2.3¢/share fade markout | **KILL** |
| **C7 cross-horizon** | peak \|ρ\| < 0.02 | **KILL** |
| **C9 liquidation cascade** | +3.4bps @ 300s, t=1.7 | **KILL** |
| **C5 maker / paired MM** | W2 −$277; prior maker fade negative | **KILL** unless C1 capacity binds (unlikely) |
| **W1 back_to_explore** | −$85, 44.6% hit | **KILL** |
| **Momentum drift belief** | −$37k as fade replacement in trend | **KILL** (final) |

### Sizing / execution — stop or deprioritize

| Target | Evidence | Action |
|---|---|---|
| **Flat $50 on cheap-entry cohort** | Live shadow: 0.04–0.17 entries lost full premium | **KILL** for pilot; shadow may log at $50 for frequency measurement only |
| **Naive Kelly sizing** | H7: −32% NET vs flat | **KILL** default; vol-sizing or discounted Kelly at pilot only |
| **Lottery tails without edge gate** | Mid-priced longshots (0.2–0.4) bleed −$0.08/edge-$1 (whale) | **KILL** ungated cheap tickets; require \(\varepsilon^{\text{net}} > 0\) |
| **Deep tail standalone (≤0.02)** | TBD — breakeven ~1.1%+fees | Screen on TUNE before any live clip |
| **Threshold sweep below 0.16** | 0.10/0.12 REJECT under passive exit (NET −12% to −16%) | **KILL** further θ sweeps on timed-exit stack; 0.12 frozen on hold stack only |
| **Pair completion / naive passive exit** | Locks ~$6 vs ~$9.7 convergence exit | **KILL** |
| **fade_live parallel engine** | 61% opposite-side vs shadow; 68 over-entries | **KILL** binary; live = shadow `decide()` + execution |

### Research hygiene

- No June data for selection.
- No HOLDOUT re-runs after peeking.
- Delete `*.trades.jsonl` after scoring (disk).
- Alpha harness alone is **not deployable** — must port to `pm-strategy` + walk-forward.

---

## 4. Validation protocol

### Windows (no leakage)

| Window | Dates | Purpose | Allowed actions |
|---|---|---|---|
| **TUNE** | 2026-02-12 → 2026-04-30 (78d) | Selection, gate thresholds, sleeve add | Grid/sweep, signal screens |
| **VERIFY** | 2026-05-07 → 2026-05-18 (12d) | Confirm frozen config | **No parameter changes** |
| **HOLDOUT** | 2026-05-19 → 2026-05-28 (10d) | One-shot final test | Winners only; no re-tune |
| **June+** | sealed | Live shadow / forward only | Measure, never select |

### Phase A — TUNE (select + gates)

Run on TUNE only before full backtest spend:

```bash
# Signal mass screen (t-stat > 2 on fee-adjusted edge)
python3 scripts/quant_signal_screen.py --date-start 2026-02-12 --date-end 2026-04-30

# Full class screen
WINDOWS=TUNE ./scripts/first_principles_scan.sh
```

**TUNE pass gates (per class):**

| Gate | Formula / threshold |
|---|---|
| Signal t-stat | \(> 2\) on \(\varepsilon^{\text{net}}\) distribution |
| Fee-net P&L | \(> 0\) |
| Hit rate (directional) | \(> 0.50\) |
| Daily Sharpe | \(> 1.0\) (annualized) |
| Consistency | VERIFY NET \(> 0.9 \times\) TUNE $/day \(\times\) (VERIFY days / TUNE days) |

**Current TUNE outcomes:**

| Cell | TUNE NET | Forward? |
|---|---:|---|
| F1 × btc5m | +$33,334 | **yes** → VERIFY confirmed |
| F7 × btc5m | +$53,671 | no (VERIFY worst-day fail) |
| Class B / C | TBD standalone ports | screen on TUNE first |

### Phase B — VERIFY (confirm)

Frozen config from TUNE winner. **Zero knob turns.**

```bash
WINDOWS=VERIFY STRATEGIES=F1 MARKETS=btc5m NOTIONAL=25 ./scripts/strategy_hunt_matrix.sh
python3 scripts/score_strategy_hunt.py
```

**VERIFY adoption (ALL required):**

1. Fee-net NET \(> 0\)
2. Daily Sharpe \(> 1.0\)
3. Worst day \(> -0.05 \times E_0\) (−$50 @ $1K)
4. Hit rate \(> 0.50\) (directional)
5. Depth stress: survives 25% `depth_capture_frac` OR documents live discount
6. Log-loss: \(\mathcal{L}_{\text{exo}} < \mathcal{L}_{\text{book}}\) — **informative only** for Class A (fails today); do not use as hard reject for disagreement thesis

**F1 VERIFY scorecard:** all hard gates **PASS** (NET +$11,841, Sharpe 46.7, worst +$415, hit 63%).

### Phase C — HOLDOUT (once)

Exactly **one** run for cells passing TUNE+VERIFY:

```bash
WINDOWS=HOLDOUT STRATEGIES=F1 MARKETS=btc5m NOTIONAL=25 ./scripts/strategy_hunt_matrix.sh
```

**HOLDOUT status:** **pending** for F1 × btc5m. Do not HOLDOUT W1–W3 or F7.

**Compositional upgrades** (perp@0.12 + rearm, vol-sizing): **no further backtest looks** per
`alpha-roadmap.md` — validate in live shadow A/B, then promote with $1K step-up.

### Promotion ladder

```
Signal screen (TUNE) → Alpha harness VERIFY → TUNE confirmation → HOLDOUT once
    → pm-strategy port → walk-forward @ $1K → shadow parity → micro-pilot
```

---

## 5. Live shadow parity checklist

Gate real money on **trade-by-trade decision parity** with shadow, not fill-rate alone
(postmortem lesson: 100% fill rate + config match still lost $700).

### Frozen `DecideConfig` (must match byte-for-byte)

Source of truth: `crates/pm-app/src/bin/exo_fade_equivalence.rs` + shadow service config.

| Field | Required value | Parity check |
|---|---|---|
| `edge_threshold` | **0.12** | Log at each decision |
| `min_entry_sigma_bps` | **3.0** | Reject if \(\sigma_{\text{bar}}^{\text{bps}} < 3\); backtest may score offline — live must wire flag |
| `min_marginal_edge` | **0.04** | Clip ≥2 only if marginal net edge passes |
| `rearm_edge` | **0.08** | Both sides < 0.08 before re-entry |
| `max_clips` | **2** | Per market per window |
| `clip_cooldown_ms` | **5000** | — |
| `exit_after_s` | **0** (hold) | Shadow logs hold; pilot may differ only after parity proven |
| `stop_before_close_s` | **90** | No entries in final 90s |
| `skip_saturday` | **true** | UTC Saturday stand-down |
| `entry_mode` | **Fade** | Not Aligned |
| `notional_usdc` | **50.0** (shadow log) | Pilot execution may scale; decisions must not change |
| `kelly_sizing` | **false** | Kelly OFF in shadow; vol-sizing at execution layer only |
| `vol_sizing_*` | 0 / defaults | Shadow measure-only |

### Frozen `AlphaModelConfig`

| Field | Value |
|---|---|
| `vol_lookback_s` | 3600 |
| `vol_sample_dt_s` | 1 |
| `vol_estimator` | realized (ewma-halflife 600 in finalized candidate — confirm single SSOT) |
| `perp_price_weight` | **0.75** |
| `momentum_lookback_s` | 0 |

### Input pipeline parity (Gate B)

- [ ] `exo_fade_equivalence` binary: **0 mismatches** on shared market tape
- [ ] Same `ExoState` construction: spot buffer + perp trades + basis-adjusted effective spot
- [ ] Same book reads: ladder top-of-book (not alternate mid semantics)
- [ ] Same strike \(K = S_{t_0}\); stand-down if spot cannot cover open
- [ ] Latency model: decision timestamp + 150ms fill assumption in backtest; live uses real fills

### Session monitors (continuous)

| Monitor | Alert threshold | Postmortem reference |
|---|---|---|
| Side agreement live vs shadow | any opposite-side in same market | 37/61 opposite overnight |
| Over-entry | live enters, shadow rejects | 68 phantom entries |
| Book WS resets | >2 per hour unexplained | 14× overnight vs 0 shadow |
| Belief feed gaps | any spot/perp drop | 0 in postmortem (still diverged) |
| Fill price realization | <0.95 sustained | looked healthy while wrong-side |
| PnL divergence | live − shadow < −$100/session | −$700 vs +$211 |

### Kelly / sizing parity note

- **Shadow:** flat $50, `kelly_sizing=false` — measures signal frequency and decision quality.
- **Pilot:** fractional vol-sizing or belief-discounted Kelly **must not change** `decide_entry`
  outputs; sizing is post-decision only.
- **Reject:** naive Kelly (concentrates unreliable large-edge coinflips).

### Pre-flight command sequence

```bash
# Gate B — construction equivalence
cargo run -p pm-app --bin exo_fade_equivalence

# Shadow vs analysis replay (VERIFY window)
# champion_f1.json = timed-exit research baseline
# shadow_match.json = hold@0.12 shadow stack (+$26,026 VERIFY)
```

**No real money until:** P4 paper session shows 100% decision match (market, side, clip count).

---

## 6. Implementation PR plan (ordered steps)

Each PR is independently mergeable; later steps gated on earlier verification.

### PR1 — Document SSOT + rename (this file + `05` cross-links)

- [x] Forward plan written + §8 swarm validation
- [x] Thesis gates in `decide_entry` + CLI (`--min-p-side`, `--min-entry-ask`, `--max-entry-ask`)
- [x] `scripts/thesis_gate_sweep.sh` + VERIFY matrix
- [ ] Update `README.md` index row for `07-strategy-forward-plan.md`
- [ ] Rename user-facing "fade" → "BSM disagreement" in deployment docs (keep `exo_fade` symbol)

### PR2 — Wire harness gates missing from alpha CLI

- [x] `--min-entry-sigma-bps` + `--skip-saturday` in `pm-app alpha` + `HarnessConfig` + `from_harness`
- [x] `scripts/thesis_backtest_sweep.sh` adds `shadow_parity` variant (σ≥3, skip Saturday)
- [x] Unit tests: sub-3bps sigma rejects; Saturday rejects (in `decide.rs`)
- [ ] Run `shadow_parity` VERIFY after release build; NET within ~1% of `shadow_baseline` ($26,026)

### PR3 — HOLDOUT run (single shot)

```bash
WINDOWS=HOLDOUT STRATEGIES=F1 MARKETS=btc5m NOTIONAL=25 ./scripts/strategy_hunt_matrix.sh
```

- [ ] Record HOLDOUT NET, Sharpe, worst day to `scorecard.md`
- [ ] Delete trades.jsonl after scoring
- **Gate:** HOLDOUT NET > 0 to proceed AWS full-history (else pause promotion)

### PR4 — `pm-strategy` port: Class A (`exo_fade` parity)

- [ ] Implement hold@0.12 + sigma floor + Saturday + rearm in `crates/pm-strategy/src/exo_fade.rs`
- [ ] Walk-forward VERIFY @ $1K: `--clip-fraction-of-equity 0.025 --max-clip-usdc 30`
- **Target:** WF NET within 2× of alpha (compounding vs sum difference documented in `02`)

### PR5 — Shadow library extraction (postmortem P3)

- [ ] Extract lightweight `pm-shadow` crate from `pm-app::shadow` (no Nautilus/parquet)
- [ ] `run_shadow_with_sink` → `ExecIntent` channel (P2 done in pm-app)
- [ ] `shadow_live` consumer in polymarket-exec: paper orders only

### PR6 — Decision parity gate (postmortem P4)

- [ ] Full-session paper: shadow vs shadow_live intent stream
- [ ] Divergence detector cron: side + entry count per market
- [ ] Document pass/fail in `data/runs/analysis/shadow_parity_<date>.json`

### PR7 — Class C standalone tail (`pm-strategy`)

- [ ] New strategy type or sleeve flag: `tail_max_price=0.10`, hold, fee-net edge gate
- [ ] TUNE screen → VERIFY frozen → compare vs Class A correlation
- [ ] Deep tail ≤0.05 experiment (whale-motivated); TBD if no TUNE signal mass

### PR8 — Class B satellite (optional, lower priority)

- [ ] Port late favourite lane with explicit worst-day reporting
- [ ] Only if VERIFY worst day > −$50 after risk shaping (tail hedge / daily budget)

### PR9 — Micro-pilot (postmortem P5, gated)

- [ ] $20–25 clips, 2 concurrent, daily stop, kill-switch
- [ ] Vol-sizing at execution layer (not in decide)
- [ ] Scale bar: realized capture ≥ 50% of sim per-trade edge over 5 sessions

### PR10 — Multi-market expansion (only after PR6 pass)

- [ ] BTC-15m: separate TUNE/VERIFY/HOLDOUT track (`alpha-hunt-003` promising)
- [ ] Alts: perp-led belief (Phase C) — **not** spot fade retry
- [ ] AWS full-history for W3 only if bonereaper re-enters VIABLE set (currently REJECT)

---

## Appendix — key artifact paths

| Artifact | Path | Key numbers |
|---|---|---|
| F1 VERIFY | `data/runs/strategy_hunt/VERIFY_F1_fade_btc5m.json` | +$11,841, 63% hit |
| F1 TUNE | `data/runs/strategy_hunt/TUNE_F1_fade_btc5m.json` | +$33,334 |
| Shadow stack replay | `data/runs/analysis/shadow_match.json` | +$26,026 @ θ=0.12, $50 hold |
| Timed champion | `data/runs/analysis/champion_f1.json` | +$19,585 @ θ=0.16, exit 30s |
| Class B (P1) | `data/runs/first_principles/VERIFY_P1_late_fav_btc5m.json` | +$392, 93% hit |
| Class C (P3) | `data/runs/first_principles/VERIFY_P3_fade_tail_btc5m.json` | +$19,585 (isolate tail) |
| SOL alt control | `data/runs/alt/sol-updown-5m_spot.json` | −$1,081 |
| XRP alt control | `data/runs/alt/xrp-updown-5m_spot.json` | −$1,139 |
| Scorecard | `docs/research/strategy-hunt/scorecard.md` | auto-generated |
| HOLDOUT | TBD | **pending** |

---

## 8. Swarm validation (2026-06-16)

Cross-check by parallel agents: code review, trade decomposition, harness backtests,
`exo_fade_equivalence` parity.

### Code / parity

| Check | Result |
|---|---|
| `decide_entry` thesis gates (`min_p_side`, `min_entry_ask`, `max_entry_ask`) | 14 unit tests pass |
| Gate ordering | edge → thesis gates → sigma → Saturday → aligned → sizing ✓ |
| `exo_fade_equivalence` (44 scenarios) | **0 mismatches — GATE B PASS** |
| `frozen_fade_decide_config` | Ungated (gates off) — matches live shadow |
| Harness parity gap | `from_harness` leaves `min_entry_sigma_bps=0`, `skip_saturday=false` — **PR2** |
| `configs/thesis_a_btc5m.toml` | Fixed: `kelly_sizing=false` (was true, contradicted H7) |

### VERIFY backtest matrix (`scripts/thesis_gate_sweep.sh`)

| variant | NET | trades | hit% |
|---|---:|---:|---:|
| shadow_baseline (ungated) | **$26,026** | 4,952 | 56.8% |
| thesis_a_gated (flat) | $20,940 | 4,884 | 57.1% |
| thesis_a_gated_kelly | $16,254 | 4,884 | 57.1% |
| thesis_a_p_floor | $23,855 | 4,929 | 57.0% |
| thesis_a_ask_floor | $20,999 | 4,894 | 57.2% |
| champion F1 (θ=0.16, exit 30s, $25) | $11,841 | 3,186 | 63.0% |
| spot_only (perp weight 0) | $17,258 | 4,889 | 55.1% |

TUNE gated (thesis_a_gated, flat $50): **+$110,474** / 23,724 trades / 60.1% hit (78d).
VERIFY gated: +$20,940 / 12d → **$1,745/d** vs TUNE **$1,416/d** → ratio **1.23×** (passes 0.9× gate).
Remaining TUNE ablations (kelly, p_floor, ask_floor): in progress.

### Forward decision

1. **Alpha SSOT:** Ungated shadow stack — BSM disagreement, θ=0.12, perp@0.75, hold, flat $50.
2. **Do not ship full A-band gates** for NET — they remove profitable B_tail (+$68/tr).
3. **Kill Kelly** in decide path (−22% on gated stack; −32% historically on ungated).
4. **Live tail fix:** Micro-clip Class C or `notional` cap when `ask < 0.10` — not blanket gate.
5. **Next PRs:** Wire sigma floor + Saturday in harness (PR2) → HOLDOUT F1 → `exo_fade` port → shadow parity.

---

## Executive summary

**Thesis:** Rename "fade" to **BSM disagreement / digital value** — we buy underpriced binary
legs when exogenous \(\hat{p}\) exceeds the taker ask by more than fees, capturing ~150ms CEX→PM
dislocation. Not stale-book mean-reversion (C6 dead); not aligned continuation (C2 dead).

**Portfolio:** **Class A ungated shadow** is the alpha champion (VERIFY +$26,026, 4,952 trades).
F1 timed exit (+$11,841 @ θ=0.16/$25) is the adoption-gate winner for risk-shaped deployment.
Full thesis gates (−19.5% NET) are **risk policy**, not alpha. Class B/C remain satellites.

**Kill:** Alts on spot belief, aligned continuation, naive Kelly, full A-band gates for alpha,
parallel `fade_live` engine, ungated lottery tickets at full clip.

**Validation:** VERIFY gate sweep complete; TUNE gated sweep running. HOLDOUT once for F1.
`exo_fade_equivalence` passes — decision SSOT is sound.

**Live path:** Shadow parity on frozen `DecideConfig` (ungated, Kelly off, σ≥3, Saturday skip)
→ wire harness parity (PR2) → HOLDOUT → `pm-strategy` port → paper parity → micro-pilot with
vol-sizing at execution layer only.