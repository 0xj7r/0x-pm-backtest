# SOL/XRP exogenous-BSM fade diagnosis (W3 2026-05-09..05-14)

## Verdict

- SOL-5m fade: STRUCTURAL (genuinely no edge). NOT a data bug.
- XRP-5m fade: STRUCTURAL (genuinely marginal edge). NOT a data bug.

The inverted-looking SOL hit rate (0.292) is real and explained: the strategy fades a
well-calibrated model against an even-better-calibrated market. There is no flag/config fix
that recovers it because there is no recoverable edge.

## How the "data bug" hypothesis was tested and rejected

The ETH precedent (a 0.334 hit was a data bug) framed an inverted hit as a bug until proven
otherwise. Four independent checks all clear the pipeline:

1. SCALE/DECIMALS. SOL strikes 90.08..98.08, XRP strikes 1.413..1.539. These drift
   realistically intraday and day-to-day (SOL 91.9->94.0 on 05-09 alone, XRP 1.415->1.504
   on 05-10), i.e. a live spot series, not a constant or off-by-10^N artefact. The prompt's
   "expected SOL ~$130-180 / XRP ~$2-3" is a stale assumption: the May 9-14 2026 archive
   simply has SOLUSDT ~$92 and XRPUSDT ~$1.43. The BSM uses log(spot/strike) and sigma in
   bps, both scale-invariant, so absolute price level cannot invert the belief regardless.

2. SPOT SERIES MAPPING. `infer_spot_symbol_from_slug` (crates/pm-app/src/discovery.rs:48)
   maps sol-updown -> SOLUSDT and xrp-updown -> XRPUSDT correctly. SOL markets load SOL spot,
   not BTC/ETH. Strike and the belief-state spot come from the SAME `SpotHistory` object
   (crates/pm-app/src/alpha.rs:572-587), so strike and state are basis/scale consistent by
   construction. Test #4 (strike-vs-state scale mismatch) is therefore impossible here.

3. SIGN/BASIS. The headline corr(p_exo, won) = -0.029 (SOL) is a RED HERRING: `won` is the
   entered-side outcome, scrambled by the fade's entry/exit logic. Measured against the
   actual YES outcome, p_exo is strongly and correctly POSITIVE:

   SOL fade reliability (p_exo bucket -> realized P(YES)):
     0.2 -> 0.141 | 0.4 -> 0.340 | 0.6 -> 0.662 | 0.8 -> 0.756
     corr(p_exo, YES_outcome) = +0.438

   XRP fade: 0.2 -> 0.213 | 0.4 -> 0.415 | 0.6 -> 0.631 | 0.8 -> 0.800
     corr(p_exo, YES_outcome) = +0.360

   A sign-flip bug would give strongly NEGATIVE correlation. The model is well-calibrated and
   monotone. The belief is correct. There is nothing to invert.

## Why the fade still loses (the actual mechanism)

The fade buys the side the model deems underpriced; on these markets that is the cheap
long-shot 76% of the time (SOL: side agrees with model majority only 186/767 = 0.24, mean
entry 0.31). The model edge looks positive at entry (mean +0.103) but does not convert,
because the market is the better forecaster:

  Brier (lower = better), SOL: model p_exo = 0.2062 vs market mid = 0.1930
  Brier, XRP:                  model p_exo = 0.2182 vs market mid = 0.2231

On SOL the Polymarket mid beats the model, so fading the model loses (gross -$942, fees
-$668, net -$1,610). On XRP the model edges the market slightly, so the XRP fade is weakly
positive (gross +$786, fees -$352, net +$434). The Brier ordering exactly predicts the sign
of each asset's P&L. This is the all-weather-book test: SOL has negative correlation to the
fade edge and belongs out of the book.

## Coherent lane, inverted fade: resolved

The lane reads the book mid for direction and does not rely on p_exo to pick the underdog, so
it is unaffected by whether the model beats the market: SOL lane hit 0.951. The divergence is
not evidence of a belief bug; it is the expected signature of "model fine, but market wins on
SOL," which only the model-heavy fade is exposed to.

## Fix

None. No flag/config change recovers SOL: the loss is structural (market out-forecasts the
model on SOL-5m in this window). Do not run a corrective harness backtest. Drop SOL-5m fade
from the book; keep XRP-5m fade only as a marginal, low-conviction lane pending more sample.

## Caveats

- Tape coverage ~70% SOL / ~68% XRP: hit rates and Brier are over covered windows only.
- 6-day single-regime window (expanded_mixed). XRP's +$434 is a tiny, thin-sample result and
  should not be promoted without a wider out-of-sample check (and never on the sealed dates
  2026-05-19..06-30).
