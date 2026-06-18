# First-Principles Strategy Discovery (2026-06-16)

Ignore repo folklore. Start from market mechanics + what the tapes actually contain,
then stress-test mechanistically distinct strategy **classes** before parameter tuning.

**Signal spec:** all formulae live in [`05-quant-signals.md`](05-quant-signals.md). Every class
below is written as signal → rule → fee-adjusted EV.

## 1. Instrument

Binary digital on spot: window \([t_0, T]\), strike \(K = S_{t_0}\), time fraction
\(\tau_t = (T - t) / (T - t_0)\).

Fair value (exo only):

\[
\hat{p}_{\text{up}} = \Phi\!\left(\frac{\ln(S_t / K)}{\sigma_{\text{bar}} \sqrt{\tau_t}}\right)
\]

Fee-adjusted edge on side \(s\) at ask \(a_s\):

\[
\varepsilon^{\text{net}}_s = \hat{p}_s - a_s - 0.07 \cdot a_s(1 - a_s)
\]

Book enters at the **decision** layer only; \(\hat{p}\) is leakage-free (see `ExoState`).
Latency (~150ms) and depth capture are execution-layer stresses, not belief inputs.

## 2. Strategy classes (mechanistically distinct)

Each class has a different **causal story**. Variants within a class (threshold, exit style)
are tuning, not discovery.

| ID | Class | Signal | Decision rule | Kill criterion |
|---|---|---|---|---|
| **C1** | Stale-book fade | \(\varepsilon_{s^*}\) (exo \(\hat{p}\)) | \(\varepsilon_{s^*} \geq \theta\), buy \(s^*\) | t-stat\((\varepsilon^{\text{net}}) < 2\) on TUNE |
| **C2** | Continuation | \(\varepsilon_{s^*}\) with \(p_{\text{dir}}\) | Aligned + \(m_{s^*} \geq m_{\min}\) | NET ≤ 0 or hit ≤ breakeven |
| **C3** | Late certainty | \(\hat{p}_{s^*}\) at low \(\tau\) | \(\tau \leq 120\)s, \(m_{s^*} \geq 0.85\) | worst day < −5% \(E_0\) |
| **C4** | Tail convexity | \(\varepsilon^{\text{net}}_s\) on cheap leg | \(a_s \leq 0.10\), \(\varepsilon^{\text{net}}_s > 0\) | mean edge < fee at entry px |
| **C5** | Maker / MM | \(\hat{p}_s - \delta\) vs bid | post-only fill model | fill anti-selects (would-fill loses) |
| **C6** | PM flow noise | \(\Delta m\) given \(\Delta S \approx 0\) | fade \(\Delta m\) | markout ≤ 0 after fees |
| **C7** | Cross-horizon | \(\text{corr}(\Delta m_{15m}, \Delta m_{5m}^{t+\ell})\) | trade peak lag | peak \(\|\rho\| < 0.05\) |
| **C8** | Cross-asset | BTC impulse → alt \(\Delta m\) | lagged entry | lagged edge < fee |
| **C9** | Forced-flow | `liq_proxy` × perp flow sign | continuation markout | t-stat < 2 on 300s return |
| **C10** | Regime router | `regime` label | route C1/C4/skip | Sharpe ≤ C1 alone |
| **C11** | Informed copy | whale fill direction | lagged replicate | edge < taker fee at px |

**Repo mapping (for reference only — do not stop here):**
- C1 → `exo_fade` / F1
- C2 → F2 aligned / `DirModel`
- C3 → `bonereaper_v2` / late-favourite lane
- C4 → whale tail bands / `tail_max_price` hedge
- C5 → `paired_mm`, `back_to_explore`
- C6 → `f6_flow_fade.py`
- C7 → `f4_cross_horizon.py`
- C9 → `f5_liquidations.py`, `liq_proxy` feature

## 3. Discovery protocol

### Phase A — Signal screen (cheap)

Measure signal mass with proper formulae **before** full backtest. TUNE only.

| Step | Command | Pass gate |
|---|---|---|
| 0. Edge distribution | `python3 scripts/quant_signal_screen.py --date-start … --date-end …` | t-stat\((\varepsilon^{\text{net}}) > 2\) |
| C6 flow noise | `python3 scripts/f6_flow_fade.py` | mean markout > 0 after fees |
| C7 cross-horizon | `python3 scripts/f4_cross_horizon.py` | peak \(\|\rho\| > 0.05\) |
| C9 liquidations | `python3 scripts/f5_liquidations.py` | continuation t-stat > 2 |
| C1–C5 backtest | `scripts/first_principles_scan.sh TUNE` | NET > 0, Sharpe\(_d\) > 1, hit > 50% |
| C11 whale | `scripts/whale_edge_decompose.py <addr>` | \(\bar{\varepsilon}^{\text{net}} > 2 f(p)\) |

Output: `data/runs/first_principles/TUNE_*.json` (summary only; delete `*.trades.jsonl`).

### Phase B — Validate (narrow)

Survivors get **VERIFY** (2026-05-07 → 2026-05-18) with adoption criteria from
`00-protocol.md`. No parameter changes between TUNE and VERIFY.

### Phase C — Implement

Promoted classes become `pm-strategy` types (full pipeline), then `walk-forward` @ $1K.
Alpha harness results alone are not deployable.

## 4. Empirical screens run today (VERIFY, BTC-5m unless noted)

| Class | Screen | Result | Verdict |
|---|---|---:|---|
| **C1** Stale-book fade | F1 alpha | **+$11,841** NET, 63% hit | **PASS** (known) |
| **C2** Continuation | P2 perp-aligned 0.04/0.08 | **−$836 / −$707** | **FAIL** |
| **C3** Late certainty | P1 late fav (120s, mid≥0.85, hold) | **+$392** NET, 93% hit | **MARGINAL** — thin $/day, tail-loss days |
| **C4** Tail convexity | P3 fade + tail hedge (≤0.10, 25% clip) | **+$19,585** NET | **PROMISING** — +65% vs fade alone; validate standalone tail |
| **C5** Maker fade | F3 maker offset 0.01 | (prior) negative vs taker | **FAIL** |
| **C6** PM flow noise | f6_flow_fade | **−2.3c/share** mean fade | **FAIL** |
| **C7** Cross-horizon | f4_cross_horizon | peak corr **< 0.02** | **FAIL** |
| **C9** Liq cascade | f5 (partial) | +3.4bps @ 300s, t=1.7 | **FAIL** — too weak |
| **C5** Paired MM | W2 walk-forward | −$277 @ $1K | **FAIL** |
| **C3** Bonereaper | W3 walk-forward | +$327 @ $1K | **MARGINAL** |

### Interpretation

1. **Only C1 (stale-book fade) clears the bar** among fully validated sleeves.
2. **C4 tail hedge on fade** is the strongest *new* signal: buying the cheap opposite
   tail alongside fade clips nearly doubles gross PnL on VERIFY. Next step: isolate tail
   entries (not hedge-only) and port to `pm-strategy`.
3. **C3 late favourite** is positive but economically thin (~$33/day @ $25 clips) and
   carries binary tail risk (one −$25 loss ≈ 11 wins). Satellite lane only.
4. **C2, C6, C7, C9** are dead on measured data — do not spend more cycles unless the
   data generating process changes (new feed, new market type).
5. **Cross-asset (C8)** and **regime router (C10)** remain untested in this pass.

## 5. Next hunts (ordered by expected information)

1. **C4 standalone tail value** — enter when ask ≤ 0.10 AND belief − ask > fee; hold to resolution.
2. **C8 cross-asset lag** — BTC spot impulse → ETH/SOL PM mid repricing delay distribution.
3. **C10 regime router** — route C1 vs C4 vs skip using `regime_cells` labels; must beat C1 alone on VERIFY.
4. **C11 whale shadow** — archive top whale fills; test whether lagged copy beats fade on same windows.
5. **C5 realistic MM** — `mm_paired_realistic_sim.py` with measured queue position (only if C1 capacity binds).

## 6. What we are explicitly not doing

- More fade threshold / exit sweeps (C1 tuning exhausted).
- Sub-$1 pair arb, dual-surface exits, synthetic split entry (all dead per mechanics review).
- June data for selection (sealed).
- Holding `*.trades.jsonl` after scoring (disk).

## 7. Run commands

```bash
# Full first-principles TUNE screen (BTC-5m)
WINDOWS=TUNE ./scripts/first_principles_scan.sh

# Reproduce today's key screens
python3 scripts/f6_flow_fade.py
python3 scripts/f4_cross_horizon.py
./target/fast/pm-app alpha ... # see first_principles_scan.sh for P1–P4 specs
```