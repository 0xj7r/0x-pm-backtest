# br2 vol-gate ablation: is the realized-vol floor protective? (2026-05-31)

Question (user): how would the LIVE br2 perform if we REMOVED the realized-vol floors,
given the model often likes a direction? Hypothesis: the floor is protective; removing it
lets br2 trade fragile low-vol "favourites" and lose.

## Method
Frozen-snapshot deterministic replay (NO retraining), baseline vs treatment on identical
markets + identical model, only the three vol floors differ.
- Binary: `target/release/pm-app walk-forward`. Snapshot: `data/snap062901.json` (the live
  production meta-calibrator, scp'd from the host) + `--disable-meta-calibration`.
- Markets: `data/runs/volgate/markets-may.jsonl` (all local May 7-20, ~3250 succeeded),
  `--use-outcome-label` (outcome from Binance spot). Builder: `scripts/build_markets_manifest.py`.
- Config: canonical champion command + the LIVE deltas (`--br2-late-favourite-min-ask 0.60`
  = askwide, `--br2-late-favourite-min-model-edge 0.06` = edge06).
- BASELINE: all three `--br2-*-min-realized-vol-180s-bps 1.25`. TREATMENT: all three 0.0.
- Compare: `scripts/volgate_compare.py`. Outputs: `data/runs/volgate/{baseline,treatment}/summary.json`.

## Result (bonereaper_v2, 3250 markets, from $1000)

| metric | BASELINE (vol 1.25) | TREATMENT (vol 0.0) |
|---|---|---|
| markets with orders | 129 (4.0%) | 1204 (37.0%) |
| orders filled | 300 | 3538 |
| total P&L | -$483.29 | -$971.47 |
| compounded return | -48.3% | -97.1% |
| end equity | $516.71 | $28.53 |
| path max drawdown | 48.4% | 97.2% |
| worst market | -$108.46 | -$80.48 |
| hit rate | 1.1% | 12.2% |
| Sharpe | -3.20 | -6.57 |

## Verdict: the vol floor is strongly protective. CONFIRMED.
Removing the floors makes br2 trade 9.3x more markets (129 -> 1204), DOUBLES the loss
(-$483 -> -$971), and NEARLY WIPES THE ACCOUNT (drawdown 48% -> 97%, equity $517 -> $29),
with worse risk-adjusted return (Sharpe -3.2 -> -6.6). The floor does exactly its job:
vetoing the fragile low-vol "favourites" the model likes but that mean-revert.

## Caveats (important, do not over-read the absolute numbers)
1. This is a FROZEN-SNAPSHOT REPLAY over calm May, which is br2's OOS-NEGATIVE regime (br2 is
   a trending-favourite tool; calm is its bad/idle regime). It is NOT the validated champion
   walk-forward (which trained inline + tested OOS). So the absolute -48% baseline is NOT a
   clean live-P&L figure; it reflects br2 trading its WORST regime in-sample-ish.
2. The decision-relevant result is the RELATIVE comparison (identical model + data + config,
   only the floor toggled), which is robust.
3. It reinforces the core thesis: br2 should stay IDLE in calm (as it does live), and the
   calm regime is the MM's job, not br2's. The vol floor is necessary-but-not-sufficient: it
   cuts calm participation 9x and the loss in half, but does not make calm profitable for br2.
