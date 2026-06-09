# Alpha Hunt 001: Exogenous Base Model (BTC-5m, May 2026)

**Date:** 2026-06-09
**Framework:** `pm-alpha` (spec: docs/superpowers/specs/2026-06-09-signal-ssot-design.md)
**Protocol:** tune on May 7-18, frozen test on May 19-28, June untouched. All fills latency-shifted and depth-walked; labels are true market resolutions.

## Model under test

Strict-exogenous belief: `P(up) = Phi(ln(S/K) / (sigma_bar * sqrt(tau)))` from Binance spot, trailing realized vol, and time remaining. The Polymarket book never enters the belief (enforced by construction in `pm-alpha`); it is only the bet cost. Entry rule: one taker entry per market when `p_exo - ask` (or the NO equivalent) exceeds a threshold; held to resolution; $50 notional.

## Multiple-testing disclosure

72 configurations tuned on May 7-18 (vol lookback {900, 1800, 3600} x threshold {0.03, 0.05, 0.08, 0.12} x latency {0, 50, 150, 300, 500, 1000}ms). One config frozen by net EV at 150 ms: **vol=3600s, threshold=0.12**. Additionally evaluated on the test window: one calibrated variant, one clip-ladder execution variant. No June data touched by anything.

## Tune window (May 7-18, 3,456 markets)

Best config: +$22.3k on 3,041 trades (+14.7%/trade, hit 54.6%) at 150 ms; every grid cell positive; P&L decays monotonically with latency.

### Trade autopsy (May 12-13 dump, 516 trades)

- Fill realism: median decision-to-fill 0.16 s; the tape is dense enough for the latency model to bind.
- The edge is the **fade/underdog cohort**: balanced YES/NO entries at book prices 0.2-0.6 where the exogenous belief disagrees with the book, hitting above the price-implied rate (e.g. 59% hit in the 0.4 bucket, 44% in the 0.2 bucket).
- **Stale-tape test passed:** entries mark up on the book itself, +10.4c/share mean at 60 s after fill (68% positive); the suspicious cheap-late cohort marks +25c on the book, i.e. the book genuinely repriced toward the entries; wins are not resolution-label phantoms.
- Book-label alignment sane (book mid log-loss 0.49 at fixed checkpoints).

## Frozen test window (May 19-28, 2,877 markets) — the honest read

| latency | trades | total P&L | per trade | hit |
|---|---|---|---|---|
| 0 ms | 2,439 | +$15,963 | +$6.55 | 54.7% |
| 150 ms | 2,439 | +$14,108 | +$5.78 | 54.7% |
| 500 ms | 2,439 | +$10,152 | +$4.16 | 54.7% |
| 1000 ms | 2,439 | +$6,173 | +$2.53 | 54.7% |

- **Net-of-cost EV: PASSES out of sample**, with a wide latency safety margin (still positive at 1 s; we run ~150 ms).
- **Log-loss vs book: FAILS** (belief 0.5285 vs book 0.4963). The belief as a whole is not better calibrated than the market; the *entry-selected subset* is where the edge lives.
- Healthy tune-to-test degradation (+$7.35 to +$5.78 per trade), not overfit collapse.

Per the spec section 7 bar (both legs required), the base model is **not fully cleared as a belief**, but the trading rule has strong OOS EV in this window.

## Calibrator v1: rejected

Exogenous calibrator (Beta + Isotonic + boosted stumps over 16 exogenous features), trained on 64,227 samples from May 7-18 only, evaluated frozen on May 19-28:

- Log-loss 0.5266 — negligible gain over raw (0.5285), still behind the book (0.4963).
- EV at 150 ms **halved** to +$6.5k (looser entries: 2,814 trades at 52.9% hit).

Greedy protocol verdict: rejected from the entry path. The machinery stays (snapshot saved); future iterations worth trying: per-tau calibration buckets, richer flow features, more training history (Feb-Apr from S3).

## Execution realism: clip ladder

Same frozen config and test window, 150 ms, but entries laddered as 4 x $12.50 clips with a 10 s cooldown (same $50 max per market) instead of one block:

| execution | trades/clips | total P&L | per $ deployed |
|---|---|---|---|
| 1 x $50 block | 2,439 | +$14,108 | +11.6% |
| 4 x $12.50 ladder | 7,827 | +$6,980 | +7.1% |

The first clip captures most of the edge; later clips fill after the book has partly repriced. Laddering halves total P&L at equal max notional but is the realistic deployment shape — +7.1% of deployed notional per market is the honest per-unit number, and it still clears costs with room.

## Per-regime cells (test window, clip run, 150 ms)

| regime (CEX-exogenous v1) | markets | P&L | per clip | hit |
|---|---|---|---|---|
| calm_low_vol | 1,533 | +$1,647 | +$0.36 | 54.6% |
| expanded_mixed | 1,299 | +$4,637 | +$1.51 | 52.4% |
| expanded_high_flip | 24 | +$642 | +$10.88 | 52.5% |
| clean_directional | 21 | +$54 | +$0.77 | 59.2% |

The edge concentrates in expanded/whipsaw regimes and is thin in calm tape — consistent with the fade thesis (the book gets caught wrong when price whips around the strike) and with br2's live experience (idle in calm). This is the seed of the regime router: this strategy wants size in expanded regimes; calm tape belongs to other plays (e.g. the paired-MM analysis) and Feb-Apr-style trends likely belong to the momentum/favourite profile, to be validated separately.

## Daily stability (test window, $50 block, 150 ms)

9 of 10 days positive; mean +$1,411/day, sd $1,033 (daily Sharpe 1.37 on these ten days). Worst day -$775 (May 25, hit 46.7%). At $2,800 bankroll the $50 clip is ~1.8% of equity per market with ~245 trades/day, so per-market sizing, daily loss caps, and exposure overlap limits are the next engineering layer before any live test. The annualized Sharpe implied by ten days is not meaningful; capacity and quote competition will compress it.

## Big caveats, stated plainly

1. **Regime.** May was a violently whipsaw month that structurally favors fading the book; Feb-Apr favored momentum/directional flow (the br2 profile). Nothing here is validated outside May. Feb-Apr S3 validation is mandatory before sizing up (AWS creds currently expired).
2. **Competition for quotes.** The simulator fills our notional against displayed depth at the latency-shifted snapshot. Live, faster takers compete for exactly these stale quotes. The +25c/60s book mark-up suggests the window is real but the capture fraction is unknown; the live fill ratio must be measured small-size first.
3. **Strike proxy.** Strike = first Binance trade at window open, not the true Polymarket open print. Near-pin markets carry proxy error in the belief (resolution labels are unaffected).
4. **One cell.** BTC-5m only. ETH-5m / BTC-15m / ETH-15m manifests are being built; SOL/XRP and 1h/4h windows to follow as part of the ensemble.

## Decision

Champion: **raw exogenous base, vol=3600s, threshold=0.12, taker, BTC-5m** — carried forward as the baseline for family additions (momentum drift, CEX order-flow, regime gating) under the same greedy OOS protocol, plus multi-market and Feb-Apr validation. Not live-deployable on this evidence alone; next gate is regime/multi-market robustness, then small-size live fill-ratio measurement.
