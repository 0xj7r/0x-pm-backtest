# Directional Satellite Candidates for Gated Fade (2026-06-19)

## Context
- Fade (exo_fade F1) now protected by per-decision regime gates in SSOT (decide_entry).
- Prod (shadow-final): --skip-calm + --skip-expanded-mixed (from Jun14-19 live sweep; calm + mixed drove bleed).
- Goal: complementary directional strategy(ies) to participate on trend/reversal/clean_directional days where fade bleeds or is gated.
- Research: broad (subagent-driven, not locked to bonereaper_v2 config). Use if it aligns.

From extensive codebase + data + doc research (3 parallel subagents):
- Bleed regimes: calm_low_vol, expanded_mixed (live data).
- Good directional: clean_directional, expanded_reversal_pressure, clean_directional_path (BR2 cluster evidence, but broad).
- Reusable primitives (exogenous, pre-route safe):
  - DirFeatures + DirModel (pm-alpha/directional.rs): flow, oi, basis, liq_proxy, trend_alignment, p_continuation.
  - Regime snapshots (alpha v1 + strategy 8-cluster + WhipsawRiskSnapshot).
  - Ctx pre-route fields (runner: regime_*, prior_ranges, flow, model_attribution).
  - decide extensibility (Aligned path + regime gates).

Past rejections noted: pure aligned as belief (F2 negative), momentum drift.

## 4 Candidates (lightweight satellites)
All activate differentially on bleed/gated regimes. Compatible with current gates for fade core + future pre-route router (weights, risk_mult from decision logs).

1. **Flow-Following Directional Satellite**
   - Thesis: perp/spot taker flow imbal + liq_proxy (forced) + burst predict continuation on directional tape.
   - Signals: dir_features (perp_flow_imbal_60s, spot_flow_imbal, liq_proxy, perp_burst), Ctx.regime_* (eff, reversal, whipsaw), optional DirModel.p_up.
   - Logic: late window; side = sign(flow) if |imbal|>0.3 + alignment + regime clean/reversal; guards on max_whipsaw/reversal/eff.
   - Sizing: micro (0.5-1x fade clip) or regime_mult.
   - Activate: clean_directional / reversal_pressure (fade skip or 0 weight).
   - Validation: regime attribution, flow ablation in hunt matrix, router policy on flow features.

2. **Basis + OI Momentum Directional**
   - Thesis: basis expansion + oi_delta + funding as mechanical continuation signal vs fade disagreement.
   - Signals: basis_bps, oi_delta_5m/30m, funding, trend_sigma (from DirFeatures + PerpState).
   - Logic: enter aligned to basis/oi when consistent + high eff regime; exit on contraction.
   - Sizing: basis_mom scaling (already in DecideConfig).
   - Activate: clean + basis divergence.
   - Validation: extend f3_basis scripts, walk-forward with basis thresh.

3. **Conditional Multi-Horizon Alignment Continuation**
   - Thesis: strong cross-horizon trend_alignment + eff predicts continuation precisely where fade is gated.
   - Signals: trend_alignment, trend_*_sigma (abs), vol_expansion, path_efficiency (Dir + regime).
   - Logic: use DirModel p_up or book-fav only if all horizons agree + eff>=0.35 + regime != calm/mixed; late guards.
   - (Avoids rejected naive full aligned.)
   - Sizing: 1x with regime throttle.
   - Activate: clean_directional_path.
   - Validation: DirModel tests + conditional in alpha harness (already supports --aligned-mode + --dir-model); ablate vs unconditional.

4. **Reversal-Pressure Late/Tail Hybrid**
   - Thesis: on reversal/expanded high flip, selective late fav or cheap tail using reversal + dir signals.
   - Signals: Ctx.regime_reversal_pressure + dir liq/flow + model.
   - Logic: high-cert band (0.70-0.97) if reversal + flow confirm; or tail on fragile.
   - Sizing: lane multipliers + tail frac.
   - Activate: reversal_pressure / high_flip where fade bleeds.
   - Validation: late_break sims + convex in BR2 but exogenous first.

## Integration
- Run as separate strategy (new StratId or via profile) + router policy for allocation.
- Use shared Ctx regime + DirFeatures (no duplication).
- Router: pre-route emit regime + recent same-cluster PnL → weights (fade 0.6/0.0 in directional, satellite 0.4/1.0).
- SSOT: gates stay in decide_entry for fade; satellite can consume decide or own on_event.
- No live until parity + shadow twin for the satellite.

## Validation Commands (per handoff)
```bash
# Regime + gate validation
python3 scripts/score_regime_gate_sweep.py --shadow-dir data/runs/june_gated_daily --since 2026-06-14
python3 scripts/june_regime_compare.py --trades data/runs/june_gated_daily/2026-06-15_trades.jsonl

# Strategy matrix with gates + directional variant
WINDOWS=VERIFY STRATEGIES=exo_fade ./scripts/strategy_hunt_matrix.sh

# Router dataset (after decision-log walk)
python3 scripts/router_decision_log_dataset.py ...
python3 scripts/router_policy_search.py ...  # compare gated-fade vs adaptive vs directional satellite

# Walk-forward hybrid
cargo run -p pm-app -- walk-forward --markets ... --strategies exo_fade,back_to_explore --profile ... --portfolio-mode ...
```

See handoff for full Phase plan (Phase 1 satellite telemetry, Phase 3 pre-route router).

Broad research complete; candidates grounded in Dir + regime + flow primitives. Pick one for prototype implementation next.
