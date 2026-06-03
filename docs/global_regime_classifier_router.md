# Global Regime Classifier Router

This is the working design for moving away from global strategy knobs toward
regime-aware strategy routing.

## Goal

Before a market is handed to a strategy, classify the current tape regime using
live-safe signals, then route capital to the strategy/model family that has
historically performed best in that regime.

The classifier should not be a BackToExplore gate or a BR2 gate. It should be a
shared market-state object consumed by every active strategy.

## Contract

For each market and fill opportunity, produce:

- `regime_label`: coarse cluster name, e.g. `clean_directional_path`,
  `expanded_high_flip`, `expanded_reversal_pressure`, `calm_low_vol`.
- `regime_scores`: calibrated probabilities or scores for each regime.
- `strategy_weights`: allocation weights for `back_to_explore`,
  `bonereaper_v2` / BR2-directional, `paired_mm`, and `risk_off`.
- `risk_multiplier`: global size multiplier before strategy-local sizing.
- `reasons`: top contributing live-safe signals for audit/debug.

## Live-Safe Inputs

Current artifacts expose these useful fill-time fields:

- `market_yes_range_so_far`
- `regime_path_efficiency`
- `regime_sign_flip_rate`
- `regime_reversal_pressure`
- `regime_whipsaw_score`
- `regime_realized_vol_180s_bps`
- `binance_adverse_vol_*`
- `binance_flow_imbal_*`
- `spot_ret_*` and `spot_accel_*`
- `seconds_to_close`
- model confidence/risk/edge fields

Do not use final resolved range or outcome labels as classifier inputs. They are
only labels for training/evaluation.

## Current Evidence

BackToExplore has regime-dependent polarity:

- Early/full-history sample:
  - `expanded_high_flip`: `-$701.69`
  - `low_efficiency_nonreversal`: `+$710.14`
- May sample:
  - `expanded_high_flip`: `+$195.99`
  - `low_efficiency_nonreversal`: `-$218.82`

BR2 selected full-history is directionally strong across the current live-safe
clusters:

- `clean_directional_path`: `+$1,957.65`, `88.1%` traded-market win rate
- `expanded_reversal_pressure`: `+$2,919.76`, `76.6%`
- `expanded_high_flip`: `+$1,540.28`, `74.4%`

The BR2 recent-regime logistic report now includes AUC:

- Test log loss: `0.5475`
- Existing `side_model_p` test log loss: `0.5669`
- Test AUC: `0.6502`
- Existing `side_model_p` test AUC: `0.6965`

Interpretation: BR2's existing side model ranks well, while the recent-regime
model improves calibration. The router should combine both rather than replace
one with the other.

## Overlap Router Evaluation

The first same-market diagnostic joins BackToExplore and BR2 artifacts on
`6,500` overlapping BTC 5m markets from `2026-02-27` to `2026-03-22`.

Artifacts:

- `data/runs/regime_clusters/router_overlap_bte_vs_br2.md`
- `data/runs/regime_clusters/router_overlap_bte_vs_br2_bte_features.md`
- `data/runs/regime_clusters/router_overlap_bte_vs_br2_br2_features.md`
- `data/runs/regime_clusters/router_overlap_bte_vs_br2_train50.md`
- `data/runs/regime_clusters/router_market_dataset_bte_vs_br2.md`
- `data/runs/regime_clusters/router_market_dataset_bte_vs_br2.jsonl`
- `data/runs/regime_clusters/router_market_dataset_may_bte_vs_br2.md`
- `data/runs/regime_clusters/router_market_dataset_may_bte_vs_br2.jsonl`
- `data/runs/regime_clusters/router_market_dataset_combined_bte_vs_br2.jsonl`
- `data/runs/regime_clusters/router_policy_search_combined_bte_vs_br2.md`
- `data/runs/regime_clusters/router_policy_search_combined_bte_vs_br2_riskheavy.md`
- `data/runs/regime_clusters/router_policy_search_combined_bte_vs_br2_riskmax.md`

Train/test split `60/40`, feature source `union`:

- BR2-only test PnL: `+$2,294.25`, max DD `17.69%`
- BackToExplore-only test PnL: `+$900.28`, max DD `13.30%`
- Cluster router test PnL: `+$3,138.08`, max DD `13.23%`

Train/test split `60/40`, feature source `back_to_explore`:

- Cluster router test PnL: `+$2,761.92`, max DD `12.86%`

Train/test split `60/40`, feature source `br2`:

- Cluster router collapses to BR2-only: `+$2,294.25`, max DD `17.69%`

Interpretation: the router effect is real enough to pursue, but strategy-local
BR2 fills do not expose enough feature coverage by themselves. Production needs
a shared market-state feature layer available before route selection, not a
router that depends on whichever strategy happened to fill.

## Market-Level Router Dataset

`scripts/router_market_dataset.py` now emits one row per overlapping market with
per-strategy labels and explicit feature provenance:

- `labels.candidate_pnl_usdc`: realized PnL label for each candidate strategy.
- `labels.best_candidate`: best candidate on that market.
- `diagnostic_fill_summary_features`: current-market fill-derived summaries.
  These are discovery features only until the runner emits the same signals from
  a shared pre-route market-state layer.
- `no_lookahead_features`: prior rolling PnL, win-rate, loss-rate, and trade-rate
  features computed only from earlier markets.

Current BTE-vs-BR2 dataset:

- Rows: `6,500`
- Train rows: `3,900`
- Test rows: `2,600`
- Range: `2026-02-27` to `2026-03-22`
- Output: `data/runs/regime_clusters/router_market_dataset_bte_vs_br2.jsonl`

Held-out policy results:

- BR2-only: `+$2,294.25`, max DD `17.69%`
- BackToExplore-only: `+$900.28`, max DD `13.30%`
- Diagnostic current-cluster router: `+$3,138.08`, max DD `13.23%`
- Prior-288-market rolling mean selector: `+$2,407.36`, max DD `10.63%`
- Prior-288 rolling selector with risk-off when all prior means are negative:
  `+$2,161.35`, max DD `11.37%`
- Prior same-cluster 288-market selector with risk-off: `+$2,978.48`, max DD
  `13.36%`

May overlap (`5,752` markets, `2026-05-01` to `2026-05-20`) is different:

- BR2-only one-shot 60/40 test: `-$18.81`, max DD `18.63%`
- BackToExplore-only one-shot 60/40 test: `+$39.15`, max DD `21.74%`
- Diagnostic current-cluster router one-shot 60/40 test: `-$54.71`, max DD
  `21.13%`
- Chronological fold search best: mostly BackToExplore / long rolling selector,
  `+$817.94`, max DD `17.22%`

Combined overlap (`12,252` markets, `2026-02-27` to `2026-05-20`) policy
search:

- Fixed BR2 over the same held-out fold windows: `+$2,808.68`, max DD `23.11%`
- Fixed BackToExplore: `+$2,722.89`, max DD `12.25%`
- Best adaptive policy: `hybrid_w288_min20_risk-1_cw0.1`
  - PnL: `+$5,671.72`
  - Max DD: `13.55%`
  - CVaR 5%: `-$40.12`
  - Worst market: `-$220.00`
  - Routes: `5,894` BR2, `3,206` BackToExplore
  - Fold stability: `12/14` positive folds; losing folds are `2026-05-07` to
    `2026-05-10` (`-$58.32`) and `2026-05-16` to `2026-05-19` (`-$103.80`)
- The same policy remains top under heavier risk objectives:
  - `pnl - 80*max_dd_pct + 20*cvar05`
  - `pnl - 150*max_dd_pct + 30*cvar05`

Interpretation: the adaptive route materially improves PnL versus either fixed
strategy while keeping drawdown much closer to BackToExplore than BR2. It is not
yet deployable as-is because its current `diagnostic_cluster` is fill-derived.
The deployable shape is therefore:

1. Use shared current-market state for regime classification.
2. Use recent no-lookahead strategy/regime performance for adaptive routing.
3. Route to `risk_off` when both current regime confidence and recent strategy
   edge are weak.

## Pre-Route Feature Export

`crates/pm-app/src/runner.rs` decision logs now include the shared live regime
fields needed for a deployable router dataset:

- `market_yes_range_so_far`
- `seconds_since_open`
- `seconds_to_close`
- `regime_whipsaw_score`
- `regime_path_efficiency`
- `regime_reversal_pressure`
- `regime_sign_flip_rate`
- `regime_realized_vol_180s_bps`
- `prior_market_range_1d`
- `prior_market_range_3d`
- `prior_market_range_7d`

These fields are produced before strategy order submission and are written to
both JSONL and Parquet decision logs. The next targeted backtests should enable
`--decision-log` on a compact BTE/BR2 overlap slice and rebuild the router
dataset from these rows rather than from `fills_detail`.

`scripts/router_decision_log_dataset.py` is the dataset builder for that path.
It joins:

- candidate `markets.jsonl` files for realized per-strategy PnL labels
- one updated runner `decision_log.jsonl` for pre-route market-state features

Old decision logs from before `4d8fe2fe` do not contain the new regime fields.
They can be used only with `--allow-legacy-decision-log` for smoke testing; they
are not deploy-validation evidence.

Example validation command after rerunning a paired overlap with updated
decision logs:

```bash
python3 scripts/router_decision_log_dataset.py \
  --candidate bte=back_to_explore:data/runs/<run>/markets.jsonl \
  --candidate br2=bonereaper_v2:data/runs/<run>/markets.jsonl \
  --feature-candidate bte \
  --decision-log data/runs/<run>/decision_log.jsonl \
  --decision-strategy back_to_explore \
  --out-jsonl data/runs/regime_clusters/router_decision_log_dataset_bte_vs_br2.jsonl \
  --out-md data/runs/regime_clusters/router_decision_log_dataset_bte_vs_br2.md

python3 scripts/router_policy_search.py \
  data/runs/regime_clusters/router_decision_log_dataset_bte_vs_br2.jsonl \
  --train-size 2600 \
  --test-size 650 \
  --step-size 650 \
  --out-md data/runs/regime_clusters/router_policy_search_decision_log_bte_vs_br2.md \
  --out-json data/runs/regime_clusters/router_policy_search_decision_log_bte_vs_br2.json
```

## Routing Hypothesis

Initial routing should be conservative:

- `clean_directional_path`: prefer BR2-directional; allow BackToExplore only if
  its model edge is strong and recent regime agrees.
- `expanded_reversal_pressure`: prefer BR2-directional with reduced late-fav
  size; BackToExplore only in May-like sub-regime.
- `expanded_high_flip`: do not use one global rule. This bucket flips sign by
  period, so route via higher-level regime probability or recent-window model.
- `low_efficiency_nonreversal`: also polarity-flips; use as a classifier feature,
  not as a global gate.
- `calm_low_vol`: collect more evidence. Current BackToExplore exact sample is
  negative, May is positive, and BR2 artifact has no direct calm bucket.
- `risk_off`: activate after clustered strategy losses or when router confidence
  is low and drawdown is elevated.

The symbolic clusters are a thin coordination layer on top of the continuous
regime signals, not a replacement for specialist strategy gates. BR2 and
BackToExplore should continue to consume `range_so_far`, path efficiency, flip
rate, reversal pressure, realized vol, adverse flow, and model scores directly
for sizing/gating. The router uses the cluster label plus no-lookahead rolling
same-cluster PnL/win-rate/trade-rate to decide which specialist should receive
capital.

Fresh decision logs now carry a pre-route `regime_cluster` field. Dataset
builders should prefer that field when present and fall back to recomputing the
label from continuous features only for legacy logs.

## Analysis Artifacts

Current local reports:

- `data/runs/regime_clusters/back_to_explore_exact_vs_may.md`
- `data/runs/regime_clusters/br2_selected_clusters.md`
- `data/runs/regime_clusters/strategy_cluster_comparison.md`
- `data/runs/regime_clusters/br2_recent_regime_logistic.md`

Scripts:

- `scripts/strategy_regime_clusters.py`
- `scripts/recent_regime_model.py`
- `scripts/router_overlap_eval.py`
- `scripts/router_market_dataset.py`
- `scripts/router_decision_log_dataset.py`
- `scripts/router_policy_search.py`

## Next Work

1. Add a shared pre-route market-state emitter in walk-forward so the router can
   consume current-market `range_so_far`, path efficiency, flip rate, reversal
   pressure, realized vol, and flow features without depending on strategy fills.
   The first decision-log export fields are in place; flow fields still need to
   be added to the pre-route row or derived from existing model attribution.
2. Run paired BTE/BR2 targeted backtests with `--decision-log` enabled and build
   the same router dataset from decision rows instead of `fills_detail`.
3. Train a global classifier to predict coarse tape regime and per-strategy
   expected value by regime.
4. Validate the router offline by replaying a synthetic allocation:
   `strategy_weights × strategy_pnl`, with no lookahead.
5. Only after the router passes offline tests, wire it into walk-forward as a
   shared object consumed by active strategies.
