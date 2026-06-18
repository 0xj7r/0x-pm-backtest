# Quant Signals SSOT (2026-06-16)

Canonical definitions for strategy discovery and validation. Implementation lives in
`pm-alpha` (belief, vol, decide); this document is the spec those crates implement.

Every strategy class is expressed as: **signal → decision rule → fee-adjusted EV → risk constraint**.

## 1. State variables

At decision time \(t\), with window \([t_0, T]\), strike \(K\), bar length \(\Delta = T - t_0\):

| Symbol | Definition | Source |
|---|---|---|
| \(S_t\) | Effective spot (blend of CEX spot + basis-adjusted perp) | `ExoState`, `effective_spot()` |
| \(K\) | Strike = spot at \(t_0\) | `MarketMeta.strike` |
| \(\tau\) | Time remaining fraction: \((T - t) / \Delta\) | `FairValueEstimate.time_remaining_s` |
| \(\sigma_{\text{bar}}\) | Realized vol over one bar, return units | `vol_bps_over_bar / 10^4` |
| \(a_s, a_{\neg s}\) | Taker ask on side \(s \in \{\text{YES}, \text{NO}\}\) | book tape (harness only) |
| \(m\) | YES mid: \((\text{bid}_{\text{yes}} + a_{\text{yes}}) / 2\) | book tape |

**Leakage rule:** \(\hat{p}\) (belief) uses only \(\mathcal{F}_t^{\text{exo}} = \{S, \sigma, \text{perp}, \text{ref}\}\). Book enters only at the decision layer.

## 2. Fair value (alpha model)

### 2.1 BSM binary digital (base)

\[
d_t = \frac{\ln(S_t / K)}{\sigma_{\text{bar}} \sqrt{\tau_t}}, \qquad
\hat{p}_{\text{up}} = \Phi(d_t)
\]

where \(\Phi\) is the standard normal CDF (`fair_value::standard_normal_cdf`).

Remaining vol: \(\sigma_{\text{rem}} = \sigma_{\text{bar}} \sqrt{\tau}\).

Degenerate limit (\(\tau \to 0\) or \(\sigma_{\text{rem}} \to 0\)): step function
\(\hat{p}_{\text{up}} = \mathbf{1}[S_t > K]\).

### 2.2 Momentum extension

\[
z_t = \frac{\delta_t + \mu_t \tau_t}{\sigma_{\text{bar}} \sqrt{\tau_t}}, \qquad
\hat{p}_{\text{up}} = \Phi(z_t)
\]

with \(\delta_t = (S_t - K) / K\) and \(\mu_t\) the scaled trailing return
(`momentum_return` in `AlphaModelConfig`).

### 2.3 Perp-led effective spot

\[
S_t^{\text{eff}} = (1 - w)\, S_t^{\text{spot}} + w\, \bigl( F_t - \text{median}(F - S) \bigr)
\]

where \(F_t\) is perp last, \(w =\) `perp_price_weight`.

### 2.4 Realized vol

Sample last price at cadence \(\delta t\) over lookback \(L\):
\(r_i = \ln(P_i / P_{i-1})\). Then

\[
\sigma_{\text{step}} = \text{std}(r_i), \qquad
\sigma_{\text{bar}} = \sigma_{\text{step}} \sqrt{\Delta / \delta t}
\]

Output in bps: \(\sigma_{\text{bar}}^{\text{bps}} = 10^4 \cdot \sigma_{\text{bar}}\).

## 3. Edge and fees

### 3.1 Raw edge (per side)

\[
\varepsilon_{\text{yes}} = \hat{p}_{\text{up}} - a_{\text{yes}}, \qquad
\varepsilon_{\text{no}} = (1 - \hat{p}_{\text{up}}) - a_{\text{no}}
\]

Pick side \(s^* = \arg\max_s \varepsilon_s\). This is `decide_entry` L214–243.

### 3.2 Taker fee (Polymarket crypto)

Per share at fill price \(p\):

\[
f(p) = \rho \cdot p(1-p), \qquad \rho = 0.07
\]

Total fee on \(q\) shares: \(F = f(p) \cdot q\) (`curve_fee`).

### 3.3 Fee-adjusted edge (breakeven)

Minimum edge to break even on a hold-to-resolution taker buy at price \(p\):

\[
\varepsilon^{\text{net}}_s = \hat{p}_s - p - \frac{f(p)}{q} \approx \hat{p}_s - p - \rho\, p(1-p)
\]

For a round-trip (entry + exit taker): subtract \(f(p_{\text{exit}})\) as well.
Passive exit credit (maker rebate): \(+0.2 \cdot f(p_{\text{exit}})\) per share.

### 3.4 Entry gate (C1 fade)

\[
\varepsilon_{s^*} \geq \theta \quad \text{and} \quad \sigma_{\text{bar}}^{\text{bps}} \geq \sigma_{\min}
\]

Default \(\theta = 0.16\) (16pp dislocation). Rearm: both sides \(< \theta_{\text{rearm}}\) before next event.

### 3.5 Kelly sizing (optional)

For side win probability \(\hat{p}\) and entry cost \(c\):

\[
f^* = \frac{\hat{p} - c}{1 - c}, \qquad
\text{clip} = N_0 \cdot \min\!\left(1,\; \frac{f^*}{f_{\text{ref}}}\right) \cdot \frac{c}{\sqrt{\hat{p}(1-\hat{p})}}
\]

(`decide_entry` Kelly branch; \(f_{\text{ref}} = 0.16\)).

## 4. Strategy classes as signal + rule

Each class is a **different functional** of \((\hat{p}, \text{book}, \text{features})\).

| Class | Signal | Decision rule | Hold / exit |
|---|---|---|---|
| **C1 Fade** | \(\varepsilon_{s^*}\) (exo \(\hat{p}\)) | \(\varepsilon_{s^*} \geq \theta\), fade = buy underpriced vs belief | Timed exit or passive mid |
| **C2 Aligned** | \(\varepsilon_{s^*}\) with \(\hat{p} = p_{\text{dir}}\) | Same + \(m_{s^*} \geq m_{\min}\) (favourite filter) | Hold or timed |
| **C3 Late certainty** | \(\hat{p}_{s^*}\) at \(\tau \leq \tau_{\max}\) | \(m_{s^*} \geq 0.85\), \(\varepsilon_{s^*} \geq 0\) | Hold to \(T\) |
| **C4 Tail convexity** | \(\hat{p}_s - a_s\) on cheap leg | \(a_s \leq p_{\max}^{\text{tail}}\) (e.g. 0.10), \(\varepsilon^{\text{net}}_s > 0\) | Hold to \(T\) |
| **C5 Maker** | quoted spread vs \(\hat{p}\) | post bid at \(\hat{p}_s - \delta\); fill = adverse-selection test | Inventory / pair |
| **C6 Flow noise** | \(\Delta m\) without \(\Delta S\) | \(\|\Delta m\| \geq \delta_m\), \(\|\Delta S/S\| < \delta_S\) → fade \(\Delta m\) | Short horizon markout |
| **C7 Cross-horizon** | \(\text{corr}(\Delta m_{15m}, \Delta m_{5m}^{t+\ell})\) | Trade lag if peak \(|\rho| > \rho_{\min}\) | Seconds |
| **C8 Cross-asset** | \(\beta \cdot r_{\text{BTC}}\) vs \(\Delta m_{\text{alt}}\) | Alt book lags BTC impulse | Seconds |
| **C9 Liquidation** | \(\text{liq\_proxy} = \mathbb{1}[\text{burst} > 2 \land \Delta\text{OI} < 0]\) | Direction = sign(perp flow) | Continuation markout |
| **C10 Regime router** | \(\text{regime} \in \{\text{calm}, \text{expanded}, \ldots\}\) | Route to best class per regime | Per-class |

## 5. Exogenous features (perp / flow)

From `directional::dir_features` — all degrade to 0 when input missing:

| Feature | Formula / meaning |
|---|---|
| `basis_bps` | \((F_t / S_t - 1) \times 10^4\) |
| `perp_flow_imbal_60s` | \((V^{\text{buy}} - V^{\text{sell}}) / (V^{\text{buy}} + V^{\text{sell}})\) over 60s |
| `oi_delta_5m` | \(\Delta\text{OI} / \text{OI}\) over 5m |
| `liq_proxy` | burst rate \(\times\, (-\Delta\text{OI})\) when burst > 2 and OI falling |
| `trend_{w}\sigma` | \(r_w / (\sigma_{\text{bar}} \sqrt{w/\Delta})\), clamped |
| `vol_expansion` | \(\sigma_{300s} / \sigma_{1800s} - 1\) |

Continuation model: \(p_{\text{dir}} = P(\text{move continues} \mid \mathbf{x})\) — supervised on
`DirSample`; **not** a substitute for BSM at extreme mids (see alpha-hunt-002).

## 6. Regime labels (exogenous)

From `regime::classify` — fixed thresholds, reporting only (not fitted):

- **calm_low_vol**: low \(\sigma_{180s}\), high path efficiency
- **clean_directional**: trending, low flip rate
- **expanded_high_flip**: high vol + whipsaw
- **expanded_mixed**: high vol, mixed path

## 7. Validation metrics (quant pass gates)

For window \(w\) with daily P&L series \(\{P_d\}\), \(N\) trades, starting equity \(E_0\):

| Metric | Formula | Gate (VERIFY) |
|---|---|---|
| Fee-net P&L | \(\sum_i \pi_i\) after all fees | \(> 0\) |
| Hit rate | \(\#\{\pi_i > 0\} / N\) | \(> 0.50\) (directional) |
| Daily Sharpe | \(\bar{P} / \text{std}(P_d) \times \sqrt{252}\) | \(> 1.0\) |
| Worst day | \(\min_d P_d\) | \(> -0.05 E_0\) |
| Per-trade edge | \(\bar{\pi} / \bar{q}\) | positive |
| Log-loss skill | \(\mathcal{L}_{\text{exo}} - \mathcal{L}_{\text{book}}\) | negative = model beats book |
| Signal t-stat | \(\bar{\varepsilon} / (\text{se}(\varepsilon) / \sqrt{n})\) | \(> 2\) for discovery screens |

**Discovery screen** (Phase A): require signal t-stat \(> 2\) on TUNE **before** running full backtest.
**Validation** (Phase B): full backtest gates above on VERIFY.

## 8. Implementation map

| Signal | Crate / symbol |
|---|---|
| \(\hat{p}_{\text{up}}\) | `pm_alpha::belief` |
| \(\sigma_{\text{bar}}\) | `pm_alpha::vol_bps_over_bar` |
| \(\varepsilon_s\) | `pm_alpha::decide::decide_entry` |
| \(f(p)\) | `pm_alpha::harness::replay::curve_fee` |
| Regime | `pm_alpha::regime::classify` |
| Dir features | `pm_alpha::directional::dir_features` |
| Calibrator | `pm_alpha::calibrator::ExoCalibrator` (post-hoc \(\hat{p}\) correction) |

## 9. What is NOT a signal

- Book mid in belief (leakage)
- Grid-searched thresholds as "alpha" (overfit)
- Hit rate without fee adjustment
- Pre-fee pair-sum \(< 1\) (breakeven is \(\approx 0.965\) at mids after two taker legs)
- Whale P&L without adverse-selection adjustment for taker replication