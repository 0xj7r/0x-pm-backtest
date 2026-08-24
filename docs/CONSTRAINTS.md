# Constraints as code

Ten rules the framework enforces mechanically, not by discipline, because
every one of them was violated during the June 2026 live cycle and cost real
money. Each rule lists what it requires, the evidence that made it a
constraint, and where the enforcement lives today (file plus a verified
test name).

## 1. Truthful latency
- Rule: backtests run at a truthful taker latency (>=750ms). A run below the
  floor requires `--fantasy`; every output (JSON summary, scorecard,
  filename) is then watermarked `FANTASY`.
- Evidence: June P&L fell from +$17,689 at the old 250ms assumption to
  +$2,506 at truthful 1250ms, same decisions (`docs/latency-truth-2026-07.md`).
- Enforcement: `crates/pm-backtest/src/validate.rs::validate_latency`; test
  `latency_below_floor_without_fantasy_is_rejected`.

## 2. Fee-net always
- Rule: fee/rebate accounting applies unconditionally on every fill; no
  `fees_enabled`/skip-fee flag exists in the fill path. The validated taker
  fee curve (`0.07*p*(1-p)` per share, Polymarket's crypto taker fee shape)
  is charged on every taker fill by default; a sub-canonical rate requires
  `--fantasy` and watermarks the run.
- Evidence: at-touch, no-fee accounting on the June executor window
  overstated realized P&L by ~$300 (~1.7% of notional) versus fee-corrected
  accounting (`docs/fill-model-calibration-2026-07.md`).
- Enforcement: `crates/pm-backtest/src/fills.rs` applies `taker_fee_bps` AND
  `taker_fee_curve_rate` (via `curve_fee`) on every taker fill, no bypass
  path (audited, comment at `crates/pm-app/src/main.rs:2458`). Maker fills
  are unaffected (rebate only). Default `taker_fee_curve_rate` is `0.07` in
  both `RunnerConfig` and `WalkForwardConfig`
  (`crates/pm-backtest/src/config.rs`), serialized into the config
  fingerprint. A sub-canonical rate is rejected unless `--fantasy` grants a
  watermarked override:
  `crates/pm-backtest/src/validate.rs::validate_fee_rate`, wired into
  `walk-forward` via `--fee-curve-rate` (default `0.07`) next to the latency
  floor check in `crates/pm-app/src/main.rs`. Closed 2026-08-24 (golden
  hash re-recorded; see `tests/golden/README.md`). Tests:
  `curve_fee_at_half_is_175_cents_per_hundred_shares`,
  `curve_fee_vanishes_at_extremes`,
  `taker_fill_charges_curve_fee_and_maker_does_not`,
  `sub_canonical_fee_rate_requires_fantasy` (all `crates/pm-backtest/src`).

## 3. Observer-noise stress
- Rule: the scorecard headline is the spread over N jittered replays
  (perturbed decision timing/latency), not a single-tape point estimate.
- Evidence: two decision-identical live twins agreed only 57.6% of the time
  on sub-15s entries (`docs/archive/2026-07/decision-stability-2026-07.md`).
- Enforcement: `crates/pm-backtest/src/jitter.rs::jittered_run_latencies`;
  test `jittered_run_latencies_clamps_to_floor_unless_fantasy`.

## 4. Regime-window validation
- Rule: the standard scorecard runs all validated month windows and reports
  per-window; a single-window or non-canonical result is labeled
  `UNVALIDATED`.
- Evidence: regime gates tuned on 6 days of live tape blocked 99% of entries
  out-of-sample over the next 10 days before being pulled from prod
  (`docs/archive/2026-06/handoff/2026-06-19-regime-gates-high-variance-research.md`).
- Enforcement: `crates/pm-backtest/src/scorecard.rs::VALIDATED_WINDOWS` +
  `validation_label`; test `single_run_is_unvalidated`.

## 5. Sizing realism
- Rule: reports run at fractional sizing on the live bankroll (~$2,800),
  show the 0.82 realization haircut alongside raw P&L, and flag the 5-share
  venue-floor/ruin check.
- Evidence: an $850-start equity simulation showed flat $50 clips hit ruin
  within 8 days; fractional sizing was the only policy that survived the
  adversarial ordering (`docs/drawdown-handling-plan-2026-07.md`).
- Enforcement: `crates/pm-backtest/src/scorecard.rs::sizing_realism` +
  `five_share_floor_breached`; test `five_share_floor_flags_small_bankroll`.

## 6. One config fingerprint
- Rule: the engine computes one fingerprint over the full resolved config
  (defaults included) and stamps it into every output and shadow/live log
  line; parity tooling compares fingerprints, not field-by-field prose.
- Evidence: a parity review found the startup fingerprint omitted
  belief-model params, so drift outside `DecideConfig` went undetected
  until coverage was widened (commit `4b40575c`).
- Enforcement: `crates/pm-backtest/src/fingerprint.rs::config_fingerprint`;
  test `canonical_order_does_not_matter`. Stamped as the first JSONL event
  of every stream in `crates/pm-shadow/src/lib.rs`.

## 7. No P&L circuit breakers, no predictive gates
- Rule: sizing is the loss control; any gate a strategy wants is strategy
  code, subject to the same validation as everything else, never a
  framework feature.
- Evidence: every attempt to predict or avoid bad days (regime gates, dwell
  gate, position management) was falsified across Feb/May/June data; a
  regime gate was the proximate cause of the OOS collapse in rule 4's
  evidence, contributing to the June cycle's $2,357 to $850 drawdown.
- Enforcement: doctrine, not code; no framework-level gate exists to
  disable. Governed by `docs/drawdown-handling-plan-2026-07.md` and
  `docs/WHY-LIVE-DIVERGED.md`.

## 8. Strike basis discipline
- Rule: belief and strike must share one price basis; the loader refuses
  mixed-basis configs (`binance_proxy` canonical; official strikes are
  resolution-verification only).
- Evidence: mixing Binance-basis belief with official-basis strikes over
  the same chop week flipped the strategy from +$6,290 to -$6,099, a $12.4k
  swing on identical decisions (`docs/strike-basis-experiment-2026-07.md`).
- Enforcement: `crates/pm-backtest/src/config.rs::validate_basis`
  (`SpotSource`/`StrikeSource`); test `mixed_basis_is_rejected`. Scoping:
  only an `Official` strike source triggers the refusal; no `Official`
  spot price source is wired in yet, so the gate cannot fire end to end on
  a live config today.

## 9. TWAP-era settlement modeling
- Rule: the engine models settlement per market era (pre/post the Chainlink
  TWAP switch) with the correct averaging window by date, never pools P&L
  across eras without a per-era breakdown; the venue taker delay is
  era-aware too.
- Evidence: 5m/15m/4h crypto markets moved from a snapshot close to a
  Chainlink TWAP window (5m: 30s from 2026-08-07, 60s from 2026-08-14),
  alongside a venue-delay schedule that fell 500ms to 50ms over the same
  period (`docs/research/strategy-refresh-2026-08.md`).
- Enforcement: `crates/pm-backtest/src/settlement.rs::settlement_era` +
  `venue_taker_delay_ms`; test `venue_delay_eras`. Latency floor:
  `crates/pm-backtest/src/validate.rs::validate_latency_for_era`; test
  `sub_venue_delay_latency_rejected`.

## 10. TWAP-aware belief primitive
- Rule: pm-alpha exposes a bridge-adjusted digital, P(TWAP_w >= K),
  alongside the classic terminal-price digital; strategies choose the
  primitive matching the market's settlement era and the scorecard flags a
  mismatch.
- Evidence: pricing a digital under a TWAP settlement window is a distinct
  closed form from the terminal-price digital (arXiv:2606.31675); reusing
  the terminal-price formula against a TWAP-settled market misprices the
  near-the-money edge.
- Enforcement: `crates/pm-alpha/src/fair_value_twap.rs::twap_digital`; test
  `locked_average_dominates_near_close`.
