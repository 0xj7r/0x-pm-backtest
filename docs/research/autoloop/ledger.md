# Autoresearch ledger

Scoring convention: fee-net NET = sum(pnl); gross = sum(pnl+fee); daily Sharpe = mean/std of daily NET (grouped by decision day) annualized x sqrt(365); window_secs filter 300 (5m) / 900 (15m). Reference baseline = passive-exit combo (thr 0.16, timeout 60, exit 30s, clips 2, pw 0.5), recomputed from data/runs/alpha/feemin: W3 NET $19,835 / Sharpe 39.7 / 254 e/day; W2 $28,000 / 21.8 / 181 e/day; W1 $31,380 / 19.8 / 160 e/day. Batch outputs: data/runs/alpha/batch0612.

## 2026-06-12 H0: threshold refit under passive exit (0.10/0.12/0.14 vs 0.16)

| run | window | NET | vs base | Sharpe | hit | n | e/day |
|---|---|---|---|---|---|---|---|
| thr 0.10 | W3 | 13,632 | -31% | 20.4 | 0.566 | 5,660 | 472 |
| thr 0.12 | W3 | 16,722 | -16% | 26.1 | 0.576 | 4,750 | 396 |
| thr 0.14 | W3 | 20,365 | +2.7% | 32.5 | 0.604 | 3,863 | 322 |
| thr 0.16 (base) | W3 | 19,835 | - | 39.7 | 0.626 | 3,046 | 254 |
| thr 0.10 | W2 | 20,079 | -28% | 11.6 | 0.550 | 11,960 | 399 |
| thr 0.12 | W2 | 24,567 | -12% | 14.8 | 0.565 | 9,312 | 310 |
| thr 0.14 | W2 | 26,331 | -6.0% | 18.3 | 0.581 | 7,089 | 236 |
| thr 0.16 (base) | W2 | 28,000 | - | 21.8 | 0.601 | 5,418 | 181 |

The fee-net frontier did NOT move back toward 0.12 with cheaper exits: NET and Sharpe are monotonically better toward 0.16 in both windows (0.14's +2.7% W3 NET comes with a 7-point Sharpe give-up and loses W2 by 6%). The extra participation at lower thresholds (up to 472 e/day) is all fee-and-noise bleed.

VERDICT: 0.10 and 0.12 REJECT (consistent W2+W3 loss on NET and Sharpe). 0.14 INCONCLUSIVE-pending-W1, leaning REJECT (Sharpe worse in both scored windows). Keep 0.16.

## 2026-06-12 H1: passive-exit timeout 30/90/120 vs 60

| run | window | NET | vs base | Sharpe | hit |
|---|---|---|---|---|---|
| to 30 | W3 | 19,772 | -0.3% | 38.4 | 0.617 |
| to 90 | W3 | 18,818 | -5.1% | 36.5 | 0.632 |
| to 120 | W3 | 18,825 | -5.1% | 37.7 | 0.637 |
| to 30 | W2 | 27,981 | -0.1% | 21.8 | 0.589 |
| to 90 | W2 | 27,874 | -0.5% | 21.8 | 0.607 |
| to 120 | W2 | 27,820 | -0.6% | 22.0 | 0.612 |

Identical entries (n matches base exactly). Longer waits do raise hit rate (0.617 to 0.637 W3) but give the gain back in adverse late fills; NET is flat to -5% everywhere. No timeout beats 60 in either window.

VERDICT: REJECT (keep timeout 60; the parameter is insensitive in the 30-120 range, which is itself useful robustness evidence). Trades files deleted for these runs.

## 2026-06-12 H2: exit horizon 45/60/90 vs 30 under passive exit (W3 only, W2 in flight)

| run | window | NET | vs base | Sharpe | hit |
|---|---|---|---|---|---|
| ex 45 | W3 | 17,782 | -10% | 32.5 | 0.600 |
| ex 60 | W3 | 19,068 | -3.9% | 33.8 | 0.597 |
| ex 90 | W3 | 17,684 | -11% | 30.7 | 0.573 |

Fee-free exits do not favor longer convergence capture on W3: every longer horizon loses NET and Sharpe; 30s remains optimal. Non-monotone (60 beats 45 and 90), consistent with 30s sitting at the convergence sweet spot.

VERDICT: INCONCLUSIVE-pending-W1W2, leaning REJECT (W3 uniformly worse, not catastrophic).

## 2026-06-12 H4: rearm off (max-clips 1) vs clip-2 (W3 only, W2 in flight)

| run | window | NET | vs base | Sharpe | hit | n | net/trade |
|---|---|---|---|---|---|---|---|
| clips 1 | W3 | 15,752 | -21% | 41.8 | 0.640 | 2,109 | 7.47 |
| clips 2 (base) | W3 | 19,835 | - | 39.7 | 0.626 | 3,046 | 6.51 |

The backtest disagrees with the live clip-2-bleeds read: the second clip adds +$4,083 W3 NET (clip-2 trades are dilutive per trade, 6.51 vs 7.47, but positive in aggregate) at a 2-point Sharpe cost. Worth re-examining the live read against the 0.45 deflator rather than the backtest.

VERDICT: INCONCLUSIVE-pending-W1W2, leaning keep clip-2.

## 2026-06-12 H5: perp price weight 0.25/0.75/1.0 vs 0.5 (W3 only, W2 in flight)

| run | window | NET | vs base | Sharpe | hit | n |
|---|---|---|---|---|---|---|
| pw 0.25 | W3 | 17,442 | -12% | 33.2 | 0.605 | 2,973 |
| pw 0.5 (base) | W3 | 19,835 | - | 39.7 | 0.626 | 3,046 |
| pw 0.75 | W3 | 21,465 | +8.2% | 42.9 | 0.624 | 3,186 |
| pw 1.0 | W3 | 22,767 | +15% | 44.0 | 0.618 | 3,452 |

Monotone in weight on both NET and Sharpe: leaning fully on the perp price beats the 50/50 spot blend in whipsaw, with more entries too. Caveat: W3 is exactly where perp-led repricing should matter most; W1 trend is the real test before any adoption.

VERDICT: INCONCLUSIVE-pending-W1W2; strongest ADOPT-CANDIDATE direction in this batch (pw 0.75-1.0).

## 2026-06-12 H6: 15m horizon with passive exit (W3 only)

| run | window | NET | Sharpe | hit | n | e/day | net/trade |
|---|---|---|---|---|---|---|---|
| btc-15m | W3 | 4,669 | 25.9 | 0.648 | 645 | 54 | 7.24 |

Standalone positive with better per-trade economics than 5m (7.24 vs 6.51) at ~1/5 the participation. This is an additive book, not a replacement; W3 says the fade transfers to 15m.

VERDICT: INCONCLUSIVE-pending-W1W2, promising as incremental volume.

## 2026-06-12 H7: kelly / momentum overlay / ewma vol (W3 only)

| run | window | NET | vs base | Sharpe | hit | n |
|---|---|---|---|---|---|---|
| kelly | W3 | 13,569 | -32% | 35.1 | 0.629 | 3,046 |
| mom 0.1 | W3 | 17,385 | -12% | 39.4 | 0.602 | 3,543 |
| mom 0.2 | W3 | 11,552 | -42% | 30.8 | 0.562 | 4,115 |
| ewma 600 | W3 | 18,957 | -4.4% | 46.5 | 0.619 | 3,031 |
| ewma 1800 | W3 | 19,603 | -1.2% | 42.4 | 0.622 | 3,028 |

Kelly sizing loses on both NET and Sharpe at touch (and the 0.45 live deflator only makes concentration worse). Momentum overlay bleeds in whipsaw exactly as expected; W1 trend is its only chance. EWMA vol is the interesting one: NET-flat with a Sharpe lift (46.5 at halflife 600 vs 39.7), i.e. same money, smoother.

VERDICT: kelly INCONCLUSIVE-pending leaning REJECT; momentum INCONCLUSIVE-pending-W1 (whipsaw loss expected, trend window decides); ewma INCONCLUSIVE-pending-W1W2, ADOPT-CANDIDATE direction on Sharpe if W1/W2 confirm.

## 2026-06-12 F1: window-open aligned momentum as second engine (W3 only)

| run | window | NET | gross | Sharpe | hit | n | e/day |
|---|---|---|---|---|---|---|---|
| aligned e30 | W3 | -4,401 | -107 | -22.1 | 0.527 | 3,099 | 258 |
| aligned hold | W3 | 3,063 | 6,937 | 7.8 | 0.670 | 3,099 | 258 |

Day-correlation with the fade base on the 12 shared W3 days: e30 +0.58, hold +0.62. The 30s-exit expression is gross-negative before fees (pure noise) and is rejected. The hold expression is genuinely positive (hit 0.670) but weak (Sharpe 7.8 vs 39.7) and NOT complementary: its P&L days overlap the fade's, so the second-engine diversification thesis fails on W3.

VERDICT: aligned e30 REJECT (gross-negative, trades deleted). aligned hold INCONCLUSIVE-pending-W1W2 with the complementarity claim already weakened; only worth adopting if W1 shows trend-regime alpha the fade lacks.

## 2026-06-12 ETH-5m recheck under passive exit (H10)

| run | window | NET | gross | Sharpe | hit | n |
|---|---|---|---|---|---|---|
| eth5 passive | W3 | -31,332 | -24,464 | -121.7 | 0.334 | 4,332 |

Catastrophic: negative even before fees, hit 0.334. The passive exit's fee savings are irrelevant; the ETH fade signal itself is wrong-way. Per backlog rule, closed permanently.

VERDICT: REJECT (final, ETH fade family closed). Trades deleted.

## 2026-06-12 F2: session seasonality gating (background-agent report)

Report: docs/research/autoloop/f2_seasonality.md. Hour-only gate is empty (every UTC hour positive in W3). Hour x dow gate fit on W3 (+$165-171) reverses sign in all four OOS cells (-$188 to -$620) with near-zero cell overlap between windows. Real finding: weekends are stably weaker (but still positive) across all three windows; a sizing tilt, not a gate, is the only credible follow-up.

VERDICT: REJECT.

## 2026-06-12 H3: sigma-conditioned exit style (background-agent report)

Report: docs/research/autoloop/h3_sigma_exit.md. Passive-vs-taker delta is sign-unstable across W1/W2/W3 in every regime cell and sigma bucket; the W3-fit switching rule loses both OOS comparisons; oracle thresholds flip direction between W1 and W2. Key structural finding: the whole question reduces to predicting the 8-13% of passive exits that time out (-$21/trade vs +$3 when filled); sigma does not predict timeouts, book depth or quote stability at exit might.

VERDICT: REJECT (do not condition exit style on sigma or regime cell). Follow-up queued: timeout predictor from book features.

## 2026-06-12 Markout attribution (background-agent report, live shadow study)

Report: docs/research/autoloop/markout_attribution.md. Mark-basis capture 0.663 this window (vs cited 0.45, part of which is backtest size fiction). Gap decomposition: 69% entry quote decay from losing the 150ms race (lost on 45% of signals; still-quoted capture 0.886 vs raced 0.472), 26% walking the book for size, 5% exit slippage. All slip-cap and IOC counterfactuals lose money; the fix is Binance feed latency (101ms receipt lag vs 9ms for the Polymarket book), not order placement.

VERDICT: study complete; action item is live infra (latency cut), not a backtest hypothesis.

## Pending from batch0612 (not yet scored)

W2: h2_ex45/60/90, h4_noclip2, h5_pw025/075/10, h6_btc15, h7 (kelly/mom/ewma), f1, eth (will still run per script; verdict already final). W1: all. Score on arrival; H2/H4/H5/H6/H7/F1-hold verdicts above are provisional until then.

## 2026-06-12 15:11 iteration: W2 arrivals scored (H2, H4, H5 partial)

| run | window | NET | vs base | Sharpe | hit | n |
|---|---|---|---|---|---|---|
| ex 45 | W2 | 27,314 | -2.5% | 20.5 | 0.589 | 5,418 |
| ex 60 | W2 | 28,533 | +1.9% | 21.3 | 0.586 | 5,418 |
| ex 90 | W2 | 28,606 | +2.2% | 19.6 | 0.574 | 5,418 |
| clips 1 | W2 | 23,397 | -16.4% | 23.3 | 0.614 | 3,961 |
| pw 0.25 | W2 | 27,285 | -2.6% | 20.5 | 0.601 | 5,366 |
| pw 0.75 | W2 | 27,701 | -1.1% | 21.4 | 0.598 | 5,660 |

H2: W2 mildly favors longer exits on NET but gives up Sharpe; combined with W3 (all longer horizons lose), 30s stays. VERDICT firming REJECT pending W1.
H4: clip-2 adds in BOTH scored windows now (+21% W3, +19.6% W2 equivalent); the live clip-2-bleeds read was small-n noise. VERDICT leaning keep clip-2, pending W1.
H5: pw 0.75 is flat on W2 (-1.1%, within consistency tolerance) after +8.2% on W3; the perp-weight edge is regime-concentrated in whipsaw, April basis is quiet. Still the top ADOPT-CANDIDATE direction; W1 trend window decides. pw 1.0 W2 still running.

## 2026-06-12 18:11 iteration: W2 remainder + first W1 arrivals scored

| run | window | NET | vs base | Sharpe | hit | n |
|---|---|---|---|---|---|---|
| pw 1.0 | W2 | 27,542 | -1.6% | 19.4 | 0.595 | 6,128 |
| kelly | W2 | 18,811 | -32.8% | 21.9 | 0.603 | 5,418 |
| mom 0.1 | W2 | 26,794 | -4.3% | 21.2 | 0.584 | 6,425 |
| mom 0.2 | W2 | 18,581 | -33.6% | 15.0 | 0.542 | 7,995 |
| ewma 600 | W2 | 29,271 | +4.5% | 23.1 | 0.598 | 5,364 |
| ewma 1800 | W2 | 30,754 | +9.8% | 24.2 | 0.610 | 5,360 |
| f1 aligned e30 | W2 | -14,957 | - | -33.7 | 0.492 | 7,492 |
| f1 aligned hold | W2 | 4,019 | - | 3.7 | 0.663 | 7,492 |
| btc15 | W2 | n=0 | - | - | - | EMPTY RUN, investigate cache/manifest coverage for April 15m |
| eth5 passive | W2 | n=0 | - | - | - | empty (ETH April books not cached); ETH already closed on W3 |
| thr 0.10 | W1 | 20,271 | -35.4% | 10.7 | 0.548 | 17,845 |
| thr 0.12 | W1 | 32,022 | +2.0% | 17.3 | 0.573 | 13,709 |
| thr 0.14 | W1 | 33,240 | +5.9% | 19.3 | 0.593 | 10,328 |
| to 30 | W1 | 31,292 | -0.3% | 19.6 | 0.599 | 7,678 |
| to 90 | W1 | 31,575 | +0.6% | 20.0 | 0.616 | 7,678 |
| to 120 | W1 | 31,280 | -0.3% | 19.4 | 0.618 | 7,678 |

VERDICTS finalized this iteration:
- H1 timeout: REJECT FINAL, keep 60 (flat in all three windows; robust plateau).
- H0 thresholds: REJECT FINAL, keep 0.16. W1 gives 0.12/0.14 small NET gains but Sharpe is worse in every window that pays them; fails the consistency bar.
- H7 kelly: REJECT FINAL (-32% W3, -33% W2).
- H7 momentum overlay: REJECT FINAL (negative in W3 and W2; no trend-window rescue plausible at -4% on mixed tape).
- F1 aligned e30: REJECT FINAL (catastrophic W2 -$14,957). aligned hold: REJECT (weak Sharpe 3.7-7.8, day-corr +0.6 to fade; fails complementarity).
- H7 ewma: ADOPT-CANDIDATE strengthening (600: Sharpe up everywhere scored; 1800: +9.8% NET W2). W1 pending decides 600 vs 1800.
- H5 perp weight: W2 flat for all weights; W1 (running) is the decider for pw 0.75/1.0.
- DATA GAP: W2 btc15 and eth5 runs produced zero trades; check tick-cache coverage for April 15m/ETH before re-running. W3 15m result stands.

## 2026-06-12 — F9 lane cross-token, ETH W3

Late-favourite lane (aligned-mode, align-min-mid 0.85, enter within 120s of close, hold to expiry,
one taker entry leg, fee-curve 0.07) run on W3 (2026-05-07..05-18) with identical flags on BTC-5m
and ETH-5m. Sigma floor applied OFFLINE (sigma_bar_bps >= 4.0). window_secs==300 trades only.
Runs: data/runs/alpha/lane_xtoken/{btc_w3,eth_w3}.{json,trades.jsonl}.

| run | floor | n | NET | $/day | hit | avg entry | breakeven hit | days green |
|---|---|---|---|---|---|---|---|---|
| BTC W3 | none | 2138 | +$1,281.10 | +$106.76 | 92.75% | 0.9113 | 91.69% | - |
| BTC W3 | sigma>=4 | 1655 | +$1,513.56 | +$126.13 | 93.47% | 0.9129 | 91.85% | 9/12 |
| ETH W3 | none | 1985 | -$1,082.16 | -$90.18 | 91.44% | 0.9193 | 92.44% | 5/12 |
| ETH W3 | sigma>=4 | 1774 | -$441.30 | -$36.78 | 92.00% | 0.9194 | 92.46% | 5/12 |

Breakeven hit = p + 0.07*p*(1-p) at the observed avg entry price.

VERDICT: DOES-NOT-TRANSFER (on W3). On identical tape and flags, BTC clears its fee-adjusted
breakeven by +1.1pp (no floor) / +1.6pp (sigma floor) and reproduces the lane baseline, while
ETH lands 1.0pp / 0.5pp BELOW breakeven despite entering at slightly richer favourites (0.919 vs
0.911). The ETH favourite premium at 120s-to-close is not large enough to cover one taker fee leg
plus resolution risk; the sigma floor halves the bleed but does not flip the sign, and only 5/12
days are green. This is not the latency artefact that killed the ETH FADE; the hold-to-expiry lane
loses on calibration, not signal direction.

Caveat: W3 is whipsaw tape. ETH favourite reliability may differ on calm/trend regimes, so the
real calm-regime read needs the Feb-Apr backfill (ETH tick coverage for W1/W2 is currently a known
data gap). Treat this as a W3-only DOES-NOT-TRANSFER, not a permanent kill.

## 2026-06-12 20:11 iteration: W1 complete, hypotheses finalized

| run | W1 NET | vs base | W1 Sharpe | hit | n |
|---|---|---|---|---|---|
| ex 45 | 35,106 | +11.9% | 19.6 | 0.604 | 7,678 |
| ex 60 | 38,523 | +22.8% | 18.5 | 0.603 | 7,678 |
| ex 90 | 41,247 | +31.4% | 18.6 | 0.594 | 7,678 |
| clips 1 | 25,569 | -18.5% | 22.1 | 0.614 | 5,660 |
| pw 0.25 | 29,297 | -6.6% | 19.1 | 0.603 | 7,605 |
| pw 0.75 | 32,152 | +2.5% | 19.8 | 0.610 | 7,952 |
| pw 1.0 | 31,698 | +1.0% | 19.1 | 0.604 | 8,423 |
| kelly | 23,402 | -25.4% | 19.5 | 0.613 | 7,678 |
| mom 0.1 | 28,304 | -9.8% | 17.7 | 0.589 | 9,401 |
| mom 0.2 | 16,928 | -46.1% | 11.4 | 0.545 | 12,025 |
| ewma 600 | 31,430 | +0.2% | 20.7 | 0.608 | 7,599 |
| ewma 1800 | 31,045 | -1.1% | 19.9 | 0.611 | 7,508 |
| f1 aligned e30 | -18,600 | - | -24.6 | 0.490 | 11,983 |
| f1 aligned hold | 13,350 | - | 9.8 | 0.661 | 11,983 |
| btc15 / eth5 | n=0 | - | - | - | W1 15m/ETH tapes not cached; backfill agent covers |

FINAL VERDICTS:
- H2 exit horizon: SURPRISE. Trend tape strongly rewards longer holds (ex90 +31.4% W1) but ex90 fails W3 (-11%). ex60 passes the consistency bar: W1 +22.8%, W2 +1.9%, W3 -3.9% (within tolerance), Sharpe slightly lower everywhere. ADOPT-CANDIDATE exit-after-s 60 under passive exit; flag possible regime-conditioned horizon as future work, resist further splitting (overfit risk).
- H4 rearm: keep clip-2, FINAL (adds 16-21% in every window).
- H5 perp weight: ADOPT-CANDIDATE pw 0.75 (W3 +8.2%, W1 +2.5%, W2 -1.1%; consistent, modest). pw 1.0 REJECT (whipsaw-only).
- H7 kelly: REJECT FINAL. momentum overlay: REJECT FINAL (loses even on trend tape, -9.8% W1).
- H7 ewma 600: ADOPT-CANDIDATE (weak): Sharpe up in all three windows (46.5/23.1/20.7 vs 39.7/21.8/19.8), NET flat. ewma 1800 REJECT (W1 flat-negative).
- F1 aligned hold: REJECT stands despite +$13.4k W1 (Sharpe 9.8 less than half the fade, day-corr +0.6; not a second engine).
- NEW H12 queued: joint candidate config validation. The adopted candidates interact (passive exit + ex60 + pw0.75 + ewma600 + rearm at thr 0.16); run the JOINT config across W1/W2/W3 vs the current combo before any live config change. One run, three windows; this is the config that challenges for the live A/B slot.

## 2026-06-12 21:11 iteration: F10 (b)(c) lane loss budgets + burn clustering (offline, W3 lane trades, sigma>=4)

| rule | total | $/day | green days | days stopped |
|---|---|---|---|---|
| no stop | $1,514 | 126.1 | 9/12 | 0 |
| stop@2 burns | -$232 | -19.3 | 4/12 | 12/12 |
| stop@3 burns | $269 | 22.4 | 4/12 | 12/12 |
| stop@4 burns | $554 | 46.1 | 7/12 | 12/12 |

Burn clustering: P(burn within 2h | just burned) = 0.59 vs 0.54 unconditional. Barely above base rate.

VERDICT: burn-count daily stops REJECT. At lane cadence (~138 entries/day) burns are routine (8-12% of trades), every day trips any K<=4 stop, and the amputated win stream costs far more than the avoided burns. Clustering is too weak to time. Implications: (1) the live "4-burn kill rule" should be retired in favor of a bankroll-fraction circuit breaker only (protect against black-swan strings, not normal variance); (2) fractional sizing IS the variance tool that works here, since weak clustering means time-diversification is real; (3) F10(a) convex hedge grid still pending harness availability; (4) lane W1/W2 runs still needed for multi-window confirmation (only W3 lane trades exist; Feb-Apr lane backtest queued behind H12).

## 2026-06-12 16:05 UTC: H12 joint candidate config, all windows

Config: thr 0.16, perp-weight 0.75, ewma-600 vol, exit-after 60s, passive exit (timeout 60), rearm 0.08 / clips 2, min-marginal-edge 0.08, fee curve 0.07.

| window | NET | vs combo-passive base | Sharpe | hit | n |
|---|---|---|---|---|---|
| W3 | $21,507 | +8.4% | 41.5 (39.7) | 0.600 | 3,221 |
| W2 | $30,800 | +10.0% | 23.8 (21.8) | 0.586 | 5,671 |
| W1 | $37,236 | +18.7% | 18.7 (19.8) | 0.597 | 7,893 |

VERDICT: ADOPT-CANDIDATE confirmed jointly; the adopted pieces compose (+8 to +19% NET over the prior best in every window, Sharpe up in two of three). This becomes the live challenger config (A/B vs canonical). Pilot remains the frozen champion. Pending stacks: basis-mom tilt (harness feature), ewma vol in shadow (code change; live challenger runs rolling-3600 until built).

## 2026-06-12 H6 follow-up: 15m on sampled Feb-Apr tapes (backfill agent)

Data gap closed: April/Feb-Mar 15m books were never in the local tick cache (raw S3 archive has them
back to 2025-10-11; only May 7+ was mirrored locally). Backfilled 16 sampled days from
s3://pm-research-data-prod (192 up+down assets/day, 8.3GB total) and re-ran the H6 cell with
identical flags (thr 0.16, pw 0.5, vol3600, exit 30s, passive exit timeout 60, clips 2, fee 0.07).
Runs: data/runs/alpha/batch0612/{W1S,W2S}_h6_btc15.{json,trades.jsonl}. window_secs==900.

Sampled days, NOT full windows: W1S = 8 days spread across 2026-02-15..03-28
(02-15, 02-21, 02-27, 03-05, 03-11, 03-17, 03-23, 03-28); W2S = 8 days across 2026-04-02..04-28
(04-02, 04-06, 04-10, 04-14, 04-18, 04-22, 04-25, 04-28). Sharpe over 8 day-samples is noisy;
treat NET/day and $/trade as the primary reads.

| run | window | NET | NET/day | Sharpe | hit | n | e/day | $/trade | green |
|---|---|---|---|---|---|---|---|---|---|
| btc-15m | W3 (full, ref) | 4,669 | 389 | 25.9 | 0.648 | 645 | 54 | 7.24 | 11/12 |
| btc-15m | W1S (8d sample) | 1,095 | 137 | 9.0 | 0.532 | 389 | 49 | 2.81 | 5/8 |
| btc-15m | W2S (8d sample) | 1,529 | 191 | 15.0 | 0.559 | 390 | 49 | 3.92 | 6/8 |

The W3 "better per-trade economics than 5m" read does NOT generalize: outside whipsaw the 15m fade
degrades harder than 5m. $/trade falls 7.24 -> 2.81/3.92 (15m, -61%/-46%) vs 6.51 -> 4.09/5.17
(5m, -37%/-21%); hit falls to 0.53-0.56. Participation is stable (~49 e/day) so the decay is
signal quality, not opportunity count. Still: positive NET in all three regimes, majority-green
days, and roughly +$140-190/day incremental volume on top of the 5m book in calm/trend tape.

VERDICT: KEEP as additive volume (consistency bar passed: green in W1S/W2S/W3), but the 15m book
is a whipsaw-concentrated edge like the perp weight, not a uniform second engine. Size it as
incremental (~1/5 of 5m participation), do not extrapolate W3 economics into calm regimes.
Caveat: W1S/W2S are 8-day samples, not the full windows the 5m numbers use.

## 2026-06-12 16:40 UTC iteration: H13 extended exit horizons on the joint config (W2 pending)

| exit | W1 NET | W1 Sharpe | W1 hit | W3 NET | W3 Sharpe | W3 hit |
|---|---|---|---|---|---|---|
| 60s (H12 base) | $37,236 | 18.7 | 0.597 | $21,507 | 41.5 | 0.600 |
| 120s | $42,231 | 18.9 | 0.599 | $19,605 | 28.0 | 0.574 |
| 180s | $47,250 | 18.9 | 0.607 | $19,870 | 28.5 | 0.568 |
| hold to expiry | $54,579 | 21.9 | 0.626 | $20,645 | 22.6 | 0.577 |

Monotone to expiry on trend tape (+47% vs ex60, Sharpe UP); whipsaw flat on NET (-4%) with Sharpe halved. The signal predicts window outcome, not just short-horizon convergence. Hold-to-expiry also deletes the exit leg entirely (no exit fee, no exit slippage, no exit deflator; redemption free), so LIVE capture improves beyond what backtest dollars show. Composes with the whale-style 99c early recycle (strictly dominates pure hold: same payoff, faster capital turnover).

VERDICT: pending W2. If W2 is within tolerance, ADOPT-CANDIDATE hold-to-expiry (or hold-with-99c-recycle) as the exit policy of the joint config, with the Sharpe tradeoff stated honestly: more NET, lumpier path on whipsaw days. Follow-up if adopted: re-test rearm/max-clips under hold (capital ties up per window), and spec the 99c-recycle exit rule for the shadow.

## 2026-06-12 H6 follow-up part 2: 15m longer history (Nov-Jan probe) + cross-token tape costs

Probe of the 15m fade before the Feb regime, on tapes from the S3 archive (goes back to 2025-10-11).
Download budget (10GB) was mostly consumed by the Feb-Apr backfill above, so this is a 4-day probe,
not the planned 6+6: W0A = 2025-11-26 + 2025-12-14 (run span 11-26..12-14), W0B = 2026-01-09 +
2026-01-23 (run span 01-09..01-23). Same flags as W1S/W2S. Binance spot+perp for these days fetched
from data.binance.vision via the existing scripts. Runs: data/runs/alpha/batch0612/{W0A,W0B}_h6_btc15.*

FEE CAVEAT, prominent: we could NOT verify when taker fees began applying to 15m crypto markets
(5m launched 2026-02-12 with fees; Gamma API does not resolve these slugs). The harness charges
fee-curve 0.07 regardless, so if pre-Feb 15m was fee-free, add back the fee column. The vol/regime
read is the point here, not exact fee-net. Second caveat: the archive only has tapes for ~2/3 of
pre-Feb 15m markets (W0A 133/192, W0B 124/188 markets), and 2-day samples are anecdotal n.

| run | days | NET | fees | n | hit | e/day | $/trade | green |
|---|---|---|---|---|---|---|---|---|
| W0A (Nov 26, Dec 14) | 2 | -10 | 53 | 34 | 0.588 | 17 | -0.30 | 1/2 |
| W0B (Jan 9, Jan 23) | 2 | +292 | 85 | 51 | 0.627 | 26 | 5.72 | 2/2 |

Read: participation collapses in the early tape (17-26 e/day vs 49 Feb-Apr, partly missing-tape
coverage, partly thinner books), Nov-Dec is fee-marginal (gross +$43 before fees), Jan looks like
Feb-Apr economics. Nothing here changes the P1 verdict; the 15m edge exists at least back to Jan,
is fee-line-sensitive before that, and the sample is too small for more than that.

Cross-token tape costs (S3 listing only, nothing downloaded; per-day download for a sampled day,
April unless noted): ETH-15m ~285MB (192/192 assets in archive), SOL-15m ~139MB (123/192),
XRP-15m ~120MB (127/192), SOL-5m ~193MB (389/576), XRP-5m ~139MB (382/576). A 4-6 day ETH-15m
April sample = 1.1-1.7GB; SOL/XRP-15m 6-day samples ~0.7-0.8GB each. ETH-5m fade stays CLOSED
(wrong-way signal); ETH-15m sample is the cheapest next look once a fresh download budget exists.
Manifests for sol/xrp/eth/bnb/doge/hype at 5m/15m/4h all exist in data/manifests/canonical/.

Budget accounting: S3 9.5GB (16 Feb-Apr days 8.3GB + 4 Nov-Jan days 1.2GB) + ~0.5GB binance.vision
= ~10GB, at cap. Raw parquets were deleted after .btc tick-cache conversion was verified (the tick
caches are the durable artifact; re-runs need no re-download). Holdout 2026-05-19..06-30 untouched.

## 2026-06-12 17:50 UTC: H13 COMPLETE, hold-to-expiry ADOPT-CANDIDATE

| exit | W1 NET/Sharpe | W2 NET/Sharpe | W3 NET/Sharpe | total NET |
|---|---|---|---|---|
| 60s (H12) | $37,236 / 18.7 | $30,800 / 23.8 | $21,507 / 41.5 | $89,543 |
| 120s | $42,231 / 18.9 | $37,324 / 24.0 | $19,605 / 28.0 | $99,160 |
| 180s | $47,250 / 18.9 | $40,321 / 23.1 | $19,870 / 28.5 | $107,441 |
| hold | $54,579 / 21.9 | $43,442 / 22.5 | $20,645 / 22.6 | $118,666 |

VERDICT: ADOPT-CANDIDATE hold-to-expiry on the joint config (+47%/+41%/-4%, passes consistency; +33% total). The fade signal predicts window outcomes, not 30s convergence. Convergent with wallet 0xce25 (752k fills, zero sells, everything to redemption). Live capture improves structurally (entry-only execution; redemption free and exact). Honest tradeoff: whipsaw-day Sharpe drops (41.5 to 22.6); book gets lumpier on chop.
Follow-ups: H14 running (threshold sweep under hold = the participation question); H15 to spec: 99c early recycle (sell at >=0.99 pre-close, strictly dominates pure hold on capital turnover); shadow exit-after-s 0 semantics need a code check before any live config flip (harness 0 = hold; shadow may exit immediately).

## 2026-06-12 22:15 UTC: H14 threshold sweep under hold-to-expiry (participation question)

Joint config with exit-after-s 0, min-marginal-edge 0.04. Baseline = hold @ thr 0.16 (W1 $54,579/21.9, W2 $43,442/22.5, W3 $20,645/22.6).

| thr | W1 NET (vs base) | W1 e/day | W2 NET | W2 e/day | W3 NET | W3 e/day |
|---|---|---|---|---|---|---|
| 0.06 | $45,617 (-16%) | 536 | $26,974 (-38%) | 552 | $2,222 (-89%) | 564 |
| 0.08 | $71,699 (+31%) | 473 | $42,931 (-1%) | 501 | $15,965 (-23%) | 539 |
| 0.10 | $80,644 (+48%) | 389 | $50,370 (+16%) | 424 | $16,467 (-20%) | 488 |
| 0.12 | $74,041 (+36%) | 299 | $49,522 (+14%) | 333 | $19,611 (-5%) | 416 |

The frontier moves DOWN under hold, exactly as the 0xce25 architecture predicted: one fee leg + full convergence capture makes thinner edges profitable. thr 0.12 passes the consistency bar (+36%/+14%/-5%) with 2.6x the participation of thr 0.16 (299-416 e/day vs 116-254). thr 0.10 is the W1/W2 NET maximum but fails W3 (-20%). thr 0.06 confirms the floor exists (fee+noise bleed at 5m cadence).

VERDICT: ADOPT-CANDIDATE thr 0.12 + hold-to-expiry on the joint config. Combined effect vs original champion: roughly 3.5-4x backtest NET with ~3x participation. Whipsaw remains the weak regime (consider sigma-conditioned threshold 0.12/0.16 as future work, resist for now). Follow-ups: H15 99c-early-recycle spec; re-test rearm clips under hold+0.12 (capital per window triples); shadow exit-after-s 0 semantics check before live deploy; sizing review (at 300-400 e/day x $50 clips, capital and depth bind well before backtest dollars do).

## 2026-06-12 22:45 UTC: ONE-SHOT TEST LOOK (May 19-28, weekly budget, user-approved) PASSED

Frozen config: thr 0.12, hold-to-expiry, pw 0.75, ewma 600, rearm 0.08/clips 2, min-marginal-edge 0.04, fee 0.07.
NET $15,904 over 10 sealed days ($1,590/day, identical to the $1,543-1,651/day exploration range). Sharpe 21.3, hit 0.603, 363 e/day, 9/10 days green, worst day -$22. Zero OOS degradation. June stays sealed for the final pre-deploy config. Live: deployed as 4th shadow stream (shadow-hold) 2026-06-12 23:00 UTC.

## 2026-06-12 23:11 UTC iteration: F10(a) lane convex tail hedge grid LAUNCHED

Machine free post-H14; detached runs: lane config (aligned 0.85, thr 0.02, enter-within-close 120, stop-before-close 5, hold) x tail-max-price {0.01, 0.02, 0.03} x tail-frac {0.2, 0.3} x windows {W3, W2, W1}, sigma>=4 floor applied at scoring. Also scores the 1-2c tail as standalone (flip rate vs ~1.1% breakeven). Results next iteration.

## 2026-06-12 23:35 UTC iteration: F10(a) W3 scored, structural null

| variant | NET | $/day | worst day | hit |
|---|---|---|---|---|
| no hedge | $1,514 | 126.1 | -$213 | 0.928 |
| tail 1c x 0.2/0.3 | $1,514 | 126.1 | -$213 | identical, ZERO tail fills |
| tail 2c x 0.2/0.3 | $1,482-1,492 | ~124 | -$213 | tiny drag |
| tail 3c x 0.2/0.3 | $1,584-1,591 | ~132 | -$234 to -$245 | worst-day WORSE |

Finding: hedge-at-entry cannot buy cheap tails. At lane entry the favourite is 0.85-0.92, so the opposite side asks 8-15c, never 1-2c; the 1-2c grid simply never fills. At 3c the rare fills add a little NET but worsen tail days. The wallet's 1c tickets are a DIFFERENT trade: bought in near-decided windows (price ~0.99/0.01) late in the window, standalone.

VERDICT: F10(a) REJECT for lane hedging (W2/W1 runs will complete and are expected to confirm the structural null). NEW F11 queued: standalone late-window cheap-tail strategy: measure from tick caches the frequency that a side offered at <=2c inside the final 60-120s subsequently WINS (flip rate vs ~1.1% fee-inclusive breakeven), by window type and regime. Offline script, no harness change. Lane risk shaping conclusion stands from F10(b)(c): fractional sizing + bankroll circuit breaker, no burn-count stops, no entry hedge.

## 2026-06-13 04:20 UTC iteration: F10(a) COMPLETE all windows, REJECT FINAL

W2/W1 confirm the W3 structural null. Across every window: 1c tail never fills (byte-identical to no-hedge); 2c adds tiny drag; 3c adds marginal NET on W2/W3 but LOSES on W1 and ALWAYS worsens the worst day (the thing a hedge is supposed to fix). A hedge that makes the worst day worse is not a hedge.

| window | no-hedge $/day | best hedge $/day | worst-day no-hedge | worst-day best-hedge |
|---|---|---|---|---|
| W3 | 126.1 | 132.6 (3c) | -$213 | -$245 (WORSE) |
| W2 | ~77 | 90.6 (3c) | -$261 | -$309 (WORSE) |
| W1 | ~68 | 67.9 (1c=noop) | -$285 | -$285 (no change) |

VERDICT: F10(a) entry-hedge REJECT FINAL. Lane tail risk is handled by fractional sizing + bankroll circuit breaker (F10 b/c), not by buying the opposite side at entry (structurally impossible at 1-2c since the loser asks 8-15c when the favourite is 0.85-0.92). The cheap-tail trade is real but SEPARATE (F11, queued): standalone late-window 1-2c tickets bought near-decided, not as a lane hedge. Lane risk policy FINAL: fractional clips, bankroll % circuit breaker, no burn-count stops, no entry hedge. F10 closed entirely.

## 2026-06-13 ToD drawdown / structural vol floor (background agent)

Report: docs/research/autoloop/tod_drawdown.md. No UTC hour is negative across all 3 windows (weakest 06 UTC +0.37/trade); overnight 02-09 is weak-positive (+2.85 vs +5.59 daytime), a low-VOL effect, not a sign flip. ADOPT sigma_bar_bps >= 3.0 floor: frozen OOS NET-positive (W1 +$129, W2 +$64), removes ~3.3% of entries, trims drawdown at ~zero at-touch NET cost. 4-5 bps over-prunes (reject). Bad hours are a vol proxy; toxic loss sits below ~3 bps in daytime too, so the floor captures it with no calendar rule.

LIVE NUANCE: at-touch overnight is weak-positive but LIVE overnight is negative (deflator + fees on thin gross); the 3 bps floor prunes exactly those sub-survival trades, so expected LIVE effect is NET-POSITIVE, not zero-cost. This is the mechanism behind the nightly -$70 to -$110 shadow bleed.

VERDICT: ADOPT-CANDIDATE min_entry_sigma_bps 3.0 for the fade (already a lane flag at 4.0; needs wiring for the fade path + shadow). Stacks with the joint/hold config. Queue: H17 wire fade sigma floor, re-validate joint+hold+sigma3 jointly across windows; the live decay dashboard should also track net-pnl-per-entry-price-decile over time (0.6-0.8 band is already efficient; erosion = 0.2-0.6 starting to resemble it).

## 2026-06-13 05:35 UTC iteration: MULTIBOOK + LANEDECON scored (candidate across assets/horizons, decontaminated)

Candidate (hold@0.12, perp 0.75 BTC / 0 ETH, ewma600, rearm2, sigma>=3 floor at scoring):
| cell | W3 NET | hit | Sharpe | e/day | W2 NET | W1 NET |
|---|---|---|---|---|---|---|
| BTC-5m | $19,828 | 0.57 | 17.4 | 382 | $47,522 | $74,154 |
| BTC-15m | $8,428 | 0.61 | 13.7 | 113 | $2,730 | $4,424 |
| ETH-5m | $4,487 | 0.58 | 12.3 | 222 | (Feb-Apr ETH 5m not cached) | - |
| ETH-15m | empty (ETH 15m not cached) | - | - | - | - | - |

VERDICTS: BTC-5m candidate stands (sigma>=3 floor barely changes W3 $19,611->$19,828, slightly POSITIVE, confirms the floor is ~free at-touch and prunes drawdown). BTC-15m is a real ADDITIVE book (W3 $8,428 = ~doubles the old 60s-exit baseline; hold helps 15m most) but whipsaw-concentrated (W1/W2 thin $2.7-4.4k on sampled days). ETH-5m VIABLE post-decontamination (W3 +$4,487, hit 0.58) - the inversion was 100% the BTC-perp bug. ETH-15m / ETH-Feb-Apr need tape backfill.
LANEDECON: lane spot-only re-run corrects the contaminated ETH verdict (see chat). Backfill needed for ETH 15m and ETH/SOL/XRP Feb-Apr to complete the multi-book table. SOL/XRP run still in progress.

## 2026-06-13 05:40 UTC iteration: SOL/XRP scored (spot-only, W3 6-day core), BUG-SUSPECTED

| cell | n | hit | NET | $/day |
|---|---|---|---|---|
| SOL-5m fade | 767 | 0.292 | -$1,610 | -$268 |
| XRP-5m fade | 440 | 0.427 | +$434 | +$72 |
| SOL-5m lane | 143 | 0.951 | +$28 | +$4.7 |
| XRP-5m lane | 353 | 0.935 | -$204 | -$34 |

VERDICT: INCONCLUSIVE-BUG-SUSPECTED, do NOT trust. SOL-5m fade hit 0.292 is an INVERSION (same signature as the ETH-perp bug at 0.334), and the same diagnostic divergence holds: SOL LANE is coherent (0.951 hit) while SOL FADE is inverted -> points at SOL signal/spot construction, not the asset. This run was already spot-only (perp off), so it is a DIFFERENT bug than ETH: likely SOL spot-series scale/basis (SOL ~$150) or strike mismatch. XRP fade weakly positive but tiny 6-day/68%-coverage sample, untrustworthy. Lanes coherent but marginal/negative (SOL ~flat, XRP sub-breakeven like ETH) -> consistent with lane-does-not-transfer-beyond-BTC.

ACTION: do not conclude SOL/XRP viability. Queue F12 = SOL/XRP fade data diagnosis (mirror the ETH diagnosis: verify the per-asset spot series is the correct asset AND correct scale/decimals, and the strike basis matches state; the lesson from ETH is that an inverted hit rate is a data bug until proven otherwise). Only after a clean diagnosis re-run can SOL/XRP fade get a real verdict. Backlog: F12 queued; SOL/XRP marked bug-suspected not dead.

## 2026-06-13 06:00 UTC: F12 SOL/XRP fade diagnosis -> STRUCTURAL (not a bug), SOL fade CLOSED

Diagnosis (docs/research/autoloop/solxrp_fade_diagnosis.md): SOL-5m fade is STRUCTURAL loss, NOT a data bug. The belief is correctly calibrated (corr(p_exo, actual YES)=+0.438). It loses because the Polymarket SOL mid OUT-FORECASTS our model (Brier mid 0.193 < model 0.206) - you cannot fade a book sharper than your model. The -0.029 corr(p_exo, won) was a red herring (won = entered-side outcome, scrambled by fade direction). All bug checks cleared: SOL->SOLUSDT correct, strikes real (~$92 SOL, ~$1.43 XRP), strike/state share one SpotHistory (no scale mismatch), BSM scale-invariant. XRP-5m fade: STRUCTURAL marginal-positive (model Brier 0.218 < mid 0.223, +$434).

VERDICT: SOL-5m fade DROP (structural, no fix). XRP-5m fade marginal-keep-as-datapoint only. Distinguishes cleanly from ETH (ETH was a perp-contamination BUG, recovered to +$4.6k; SOL is structural). Corroborated by wallet 0x8d1d abandoning SOL/XRP by June.

KEY INSIGHT (load-bearing): our edge = (our model + fresh Binance) beating a SLOW book, NOT model quality per se. Where the book is already well-calibrated (SOL), no edge exists. NEW DIAGNOSTIC queued: per-asset Brier(model) vs Brier(book-mid) as the canonical edge-health / erosion metric - when book Brier drops below model Brier, the edge is gone. Better early-warning than still_quoted. This is the single best erosion tripwire and should go on the decay dashboard.

## 2026-06-13 06:15 UTC: Lane ToD/liquidity -> overnight is a LIQUIDITY effect (not vol), ADOPT-CANDIDATE overnight align-min-mid 0.90

Report docs/research/autoloop/lane_tod_liquidity.md. Lane overnight (00-09 UTC) hit 0.927 / +$0.49/trade vs daytime 0.941 / +$1.12/trade. The ~1.4pt deficit flips the lane negative (breakeven margin only ~0.9pt) = the live overnight burns. Mechanism: VOL-CEILING REJECTED (burn% flat 6-7% across sigma 4-12; overnight is lower-vol). LIQUIDITY CONFIRMED: sub-0.90 favourite mids burn ~2x, overnight skews to those thin-book marginal favourites. The fade's ToD was low-vol (sigma floor covers it); the lane's is liquidity (sigma floor does NOT cover it, lane already runs sigma>=4).

VERDICT: ADOPT-CANDIDATE overnight (00-09 UTC) align-min-mid 0.85->0.90. Hit -> 0.946, burns 106->73, worst-day halves (BTC -$218->-$115, XTOK -$213->-$61), NET flat = pure drawdown control. CAVEAT: 12 days only, re-confirm on sealed archive before real-money lane. Deploy as a small time-gated shadow.rs change (align_min_mid steps to 0.90 when UTC hour in 0..9). STRATEGIC NOTE: the lane is marginal/BTC-only and may be made redundant by hold@0.12 covering calm tape; this fix keeps it viable as a data-collector pending the deep-tail maker sleeve (F11) and the hold-redundancy question.

## 2026-06-13 06:25 UTC: Within-window entry-timing exploration -> REAL effect, NOT gateable at-touch

Per-trade edge rises monotonically with seconds-since-window-open (sigma>=3 already applied, so SEPARATE from vol): first-60s entries $1-4.8/trade vs $5-12/trade later, robust across W1/W2/W3 (strong on mixed/whipsaw, mild on trend), and first-60s is 41-50% of ALL volume.

Entry-timing gate sweep (skip first N seconds), candidate BTC-5m:
| gate | W1 NET/Sh | W2 NET/Sh | W3 NET/Sh |
|---|---|---|---|
| none | $74,154/21.9 | $47,522/18.8 | $19,828/17.4 |
| skip<60s | $45,244/16.6 | $34,808/19.9 | $12,131/14.8 |
| skip<120s | $22,788/9.8 | $23,802/18.1 | $10,559/19.1 |

VERDICT: do NOT add an entry-timing gate. Same lesson as thresholds/seasonality/burn-stops/basis: cutting positive-but-low-edge volume loses NET (-39% at 60s) without lifting Sharpe (mostly drops). The early-window trades are dilutive per-trade but additive in aggregate. The ONLY scenario the gate wins is if the LIVE deflator pushes thin early entries negative ($1.07/trade hold entries are prime suspects) - this is a LIVE-MEASUREMENT question, not a backtest gate. ACTION: add within-window-timing P&L breakdown to the live decay dashboard (per-trade live capture by seconds-since-open); gate only if live early-entry capture is negative. Timing exploration CLOSED: fade ToD = sigma floor (covered); lane ToD = liquidity (overnight 0.90 fix); within-window = real but not gateable. Recommendation stands.

## 2026-06-13 06:40 UTC: Calendar exploration (hour-of-day + day-of-week) on candidate -> SATURDAY is the one gateable effect

Candidate hold@0.12, sigma>=3 already applied, pooled W1/W2/W3.
HOUR-OF-DAY: gradient, NO hour robustly negative across 3 windows. Golden: 12 UTC $12.1/trade (EU-US overlap), 11-15 & 22-23 strong; weak 02-06 UTC ($2.2-2.6) but positive. The weak hours are low-vol = already pruned by the sigma floor. NOT separately gateable (cutting weak-but-positive hours loses NET, same lesson as thresholds/within-window).
DAY-OF-WEEK: Mon-Fri +$5.1-7.0/trade, Sunday +$4.4, SATURDAY -$0.2 pooled (W1 -$0.5, W2 +$0.9, W3 -$1.5) - flat-to-negative in ALL THREE windows EVEN AFTER the sigma floor = a residual liquidity/participation effect (thinnest crypto day), NOT vol. Sunday is fine; specifically Saturday.

VERDICT: ADOPT-CANDIDATE Saturday stake downweight (0.25-0.5x, toward skip). Unlike the within-window/threshold gates that cut clearly-positive volume, Saturday is ~ZERO at-touch so it is NEGATIVE live (after deflator) - cutting it loses ~nothing in backtest NET (~+$850 pooled) and removes live-losing trades + a dead day (drawdown control). Survives F2 overfit critique: single robust marginal across 3 windows, not a fitted multi-cell gate. Implement as downweight (per F2 weekend-tilt lesson) not hard skip, keeps optionality. CALENDAR EXPLORATION CLOSED: hour=sigma-floor-covered; day=Saturday-downweight. Recommendation set: sigma floor (live), lane overnight-0.90 (deploy), Saturday downweight (candidate), within-window=live-instrument-only.

## 2026-06-13 06:50 UTC iteration: scored-everything + disk hygiene (two ingestion agents in flight)

All available outputs scored and ledgered (multibook/h12-16/f10a/ethfull/lanedecon/solxrp/F11/calendar/lane-ToD/SOL-diag). No new harness run: ETH-complete and BTC-long+XRP ingestion agents are running and would contend for disk. Iteration action = disk hygiene per backlog rule: deleted 309 scored *.trades.jsonl (0.9GB, kept .json summaries). Disk 22->23GB free; raw cache (56GB) is read-source for agents, left intact. Agents have 20GB guards and ingest cell-by-cell (1h/4h cells small), headroom adequate. Next iterations: score eth_complete / btclong_xrp as they land.

## 2026-06-13 07:20 UTC: Saturday downweight CONFIRMED via harness -> SKIP Saturday, strong ADOPT

Candidate BTC-5m, sigma>=3, full vs Sat 0.5x vs Sat skip:
| window | full NET/Sh/maxDD | Sat skip NET/Sh/maxDD |
|---|---|---|
| W1 | $74,154/21.9/-$1,325 | $75,287/23.0/-$440 |
| W2 | $47,522/18.8/-$3,599 | $46,393/20.0/-$744 |
| W3 | $19,828/17.4/-$938 | $20,778/19.4/-$499 |

VERDICT: ADOPT skip-Saturday (or 0.25x). NET-neutral (W1/W3 up, W2 -$1k, wash), Sharpe +1.0-1.6 every window, maxDD cut 2-5x (W2 -$3,599->-$744). Skip DOMINATES 0.5x on all metrics (Saturday contributes ~0 NET so nothing to keep). Cleanest risk-adjusted improvement of the session, robust all 3 regimes. Implement as a UTC-Saturday entry gate (or 0.25x stake) in the harness + shadow. Confirms the calendar exploration: hour=sigma-floor; day=skip-Saturday.

## 2026-06-13 07:25 UTC: BTC long horizons + XRP full (agent) -> longer horizons DEAD, cheap-tail CLOSED, XRP marginal

FACT: Polymarket horizons are 5m/15m/4h ONLY (no 1h up/down market). Report docs/research/autoloop/btclong_xrp.md.
- BTC-15m fade +$8,437 (+$6.20/trade, t=4.10, full coverage) = the real longer-horizon win; edge extends ONE horizon up.
- BTC-4h fade DEAD -$649 (t=-2.38): latency edge gone (an hour+ lets the book price Binance correctly = SOL lesson via duration). Edge needs a SLOW book; 4h book is sharp.
- Deep cheap-tail CLOSED: no <=0.10/<=0.05 level at ANY horizon (5m/15m/4h). BTC-4h min ask 0.46, XRP-4h 0.45; losing side bottoms ~0.42-0.49 in final 10%, winner bid only ~0.55-0.58 (4h uncertain to the wire). Whale sub-0.05 fills = recycle artifact / sub-snapshot, not a resting level. Cheap-tail-maker question DONE.
- XRP: marginal not dead. XRP-5m fade +$1,684 (+$2.05/trade, t=1.89, full 12d W3, high-flip-concentrated). XRP-15m/4h negative, XRP lane dead. Small high-flip diversifier at most.

MULTIBOOK FINAL (BTC-5m core + BTC-15m additive confirmed; ETH-5m probationary pending Feb-Apr; XRP-5m marginal sprinkle; SOL + all-4h + all-lanes-ex-BTC DEAD). Edge = fast-ref + SLOW book; dies where book is sharp (SOL asset, 4h duration). Ceiling: BTC 5m+15m + ETH-5m.

## 2026-06-13 07:35 UTC iteration: ETH-complete W3 cells scored (agent still backfilling Feb-Apr)

| cell | n | hit | NET | Sharpe |
|---|---|---|---|---|
| eth-5m W3 | 2670 | 0.578 | $4,487 | 12.3 |
| eth-15m W3 | 966 | 0.614 | $4,306 | 14.6 |
| eth-4h W3 | 64 | 0.797 | $1,076 | 6.7 |

ETH mirrors BTC's horizon shape on W3: 5m AND 15m both viable (~$4.3-4.5k each), long horizon weak. ETH-4h marginally positive but n=64/Sh6.7 = too thin to trust + diverges from dead BTC-4h = likely noise, not actionable. VERDICT: ETH-5m and ETH-15m VIABLE on W3 (pending Feb-Apr W1/W2 backfill for cross-regime confirmation, agent running). ETH-4h INCONCLUSIVE-thin (do not pursue). Updates multibook: ETH adds 5m+15m (probationary) paralleling BTC; ceiling now BTC-5m/15m + ETH-5m/15m if ETH confirms across regimes.

## 2026-06-13 08:35 UTC iteration: research convergence note + basis-tilt queued

No new scoreable cells (ETH W2 backfill mid-write, agent in ingest phase, CPU idle). Everything runnable is scored; candidate config FINALIZED ($142,457/90d, Sharpe 24-29, maxDD 2-5x lower). Rather than a redundant cached run, recording the open frontier:
1. ETH cross-regime confirmation (agent running W1/W2 ETH backfill) -> would promote ETH-5m/15m from probationary.
2. TOP UNBUILT EDGE: basis-momentum tilt (F3). Validated OFFLINE +9-15% NET all 3 windows (stake 1.25x when 60s perp-basis-momentum agrees with entry side, 0.75x disagree). Needs a HARNESS FEATURE: basis_mom_60s_bps input + --basis-mom-tilt flag (spec in f3_basis_momentum.md). This is the single highest-value unfinished alpha; it is a build not a run, hence not auto-runnable by this loop. PROMOTE to a feature-build task.
3. Live: 5-stream shadow A/B running (hold@0.12 winning decisively; EWMA stream just armed; lane on probation, 9 burns today).
VERDICT: research has CONVERGED on the finalized candidate. Remaining work is (a) ETH agent completion, (b) basis-tilt feature build, (c) live validation -> pilot. No new backtest hypotheses warrant a cached run.

## 2026-06-13 09:05 UTC: ETH W3 confirmed all horizons + ETH-4h DEEP TAIL reopens cheap-tail (BTC-dead, ETH-alive)

ETH agent still running (W1/W2 Feb-Apr backfill in progress, sourcing tapes from S3 one window at a time). W3 results (candidate, spot-only, sigma>=3):
| cell | NET | hit | Sharpe(daily) | trades |
|---|---|---|---|---|
| ETH-5m W3 | $4,487 | 57.8% | 0.67 | 2670 |
| ETH-15m W3 | $4,306 | 61.4% | 0.80 | 966 |
| ETH-4h W3 | $1,076 | 79.7% | 0.37 | 64 (thin) |

ETH confirms the fade on W3 across 5m/15m/4h; 15m is the standout. ETH-1h DATA-MISSING (no eth-updown-1h market series exists). Cross-regime (W1/W2) PENDING.

MAJOR: ETH-4h has a DEEP CHEAP-UNDERDOG TAIL absent on BTC. Scanned 57 ETH-4h tapes: best ask reaches 0.0010, 180k ticks at ask<=0.05 persisting to final 300s, fade traded entries as cheap as 0.03. CONTRADICTS BTC-4h (min ask 0.46, no tail). Mechanism: ETH-4h thinner/less-arbed -> losing side decays to ~0; BTC-4h efficiently uncertain to wire. REOPENS the cheap-tail-maker question (F11 closed it on BTC + all 5m/15m, but NOT ETH-4h / less-liquid-asset 4h). Capacity tiny (~6 up-markets/day) = small sleeve not franchise. QUEUE: dedicated 4h deep-tail study across ETH + other less-liquid assets (does the maker tail clear at 4h? adverse selection?). Hold until ETH backfill done + user go-ahead (let-it-run mode).

## 2026-06-13 09:35 UTC iteration: CLIP-SIZE sweep ($150/$200 real book-walk) scored

Candidate, sigma>=3 + Sat-skip, --notional-usdc 150/200 vs $50 baseline (satgate). Harness walks real top-5 book ladder.
| window | $50 NET | $150 NET / mult | $200 NET / mult |
|---|---|---|---|
| W1 trend | $75,287 | $187,233 / 2.49x | (running) |
| W2 mixed | $46,393 | $107,500 / 2.32x | $129,801 / 2.80x |
| W3 whip | $20,778 | $39,363 / 1.89x | $43,240 / 2.08x |

Ideal-linear: $150=3.0x, $200=4.0x. ACTUAL: $150 ~2.2x avg (73% eff), $200 ~2.4x (60% eff). Real book-walk is WORSE than the top-1 depth model ($150=2.62x est) - thin BTC-5m books eat more than the optimistic 0.45 walk-capture assumed. Regime-dependent: W1 trend best (2.49x), W3 whipsaw worst (1.89x, thinnest books on choppy tape). VERDICT: $150 is the sweet spot for $2K (~2.2x = ~$2,300/day live); $200 adds only ~0.2x for more bankroll = not worth it. Diminishing returns are steep past $150. Size conservatively on whipsaw days. Updates [[polymarket-account-sizing]]: real $150 mult is 2.2x not 3x.

## 2026-06-13 09:40 UTC iteration: ETH-5m CROSS-REGIME CONFIRMED (W2 April) -> promoted

| ETH-5m cell | n | hit | NET | $/day | Sharpe |
|---|---|---|---|---|---|
| W3 whip | 2670 | 0.578 | $4,487 | $374 | 12.3 |
| W2 Apr (NEW, 19d) | 2940 | 0.552 | $9,729 | $512 | 11.4 |

ETH-5m now confirmed POSITIVE across 2 regime windows (whipsaw + mixed), consistent Sharpe ~12, April per-day stronger than May. ~30% of BTC-5m daily P&L. VERDICT: ETH-5m promoted from probationary to CONFIRMED ADDITIVE BOOK (W1 trend pending for full triple but 2 windows = strong). MULTIBOOK now solidly BTC + ETH x 5m + 15m. Caveat: W2 was 19d (partial/sampled backfill a+b), not full 30d. Pending: ETH-5m W1, ETH-15m W1/W2, clipsweep W1_n200. Frontier converging: the asset/horizon map is BTC-5m(core) + BTC-15m + ETH-5m(confirmed) + ETH-15m(W3); everything else dead (SOL, 4h-except-ETH-tail, follower lanes).

## 2026-06-13 09:45 UTC: ETH agent FINAL -> confirmed whipsaw+mixed, TREND inconclusive (correction)

| ETH cell | NET | $/day | hit | Sharpe(d) | verdict |
|---|---|---|---|---|---|
| ETH-5m W3 whip | $4,487 | $374 | 57.8% | 0.67 | VIABLE |
| ETH-5m W2 Apr mixed (19d) | $9,729 | $512 | 55.2% | 0.61 | VIABLE |
| ETH-5m W1 Mar trend (4d ONLY) | -$360 | -$90 | 53.6% | -0.14 | MARGINAL/inconclusive |
| ETH-15m W3 | $4,306 | $359 | 61.4% | 0.80 | VIABLE (best) |
| ETH-4h W3 | $1,076 | $98 | 79.7% | 0.37 | thin + DEEP TAIL (ask->0.001) |
| ETH-1h | - | - | - | - | DATA-MISSING (no manifest) |

CORRECTION to the 09:40 "ETH promoted/confirmed" entry: ETH-5m is confirmed on whipsaw(W3)+mixed(W2) but TREND(W1) went slightly negative (loss in clean-directional trades = fade fighting a persistent trend). CONTRAST: BTC-5m W1 trend was its STRONGEST (+$75k) -> BTC fades trends fine, ETH-5m (this sample) does not. Caveats: W1 ETH only 4d (AWS throttling, DATA-PARTIAL), hit 53.6% marginal not decisive. VERDICT: ETH-5m = confirmed additive on 2/3 regimes, TREND-REGIME OPEN (needs full W1 data). NOT unconditionally all-weather like BTC. ETH-4h deep-tail confirmed (only novel cheap-tail surface found). Multibook: BTC-5m(core,all-weather) + BTC-15m + ETH-5m(2/3 regimes) + ETH-15m(W3). Requeue: full ETH-5m W1 backfill when AWS throttling clears; ETH-4h deep-tail study.

## 2026-06-13 10:05 UTC: Can lagging indicators time the hold config's weak (choppy) stretches? NO

Tested 3 forward-predictive indicators on 28,361 candidate trades (rolling-30, corr of trailing vs forward 30-trade pnl):
- recent performance -> forward pnl: corr +0.073 (bad stretches do NOT persist)
- recent chop (near-50/50 entry rate) -> forward: +0.052 (wrong sign, ~0)
- recent sigma -> forward: +0.099
ALL ~zero (|corr|<0.1). The hold config's choppy-tape weakness is IRREDUCIBLE BINARY VARIANCE, not a detectable/persistent regime. A "sit out when losing" filter would whipsaw (cut after loss, miss recovery) and lose NET like every prior gate (F2 seasonality, within-window, H3 sigma-exit).

KEY DISTINCTION: STRUCTURAL always-bad conditions ARE filterable (already done: sigma-3 floor = dead-low-vol, Saturday-skip = thin day). DYNAMIC regime-timing is NOT (no autocorrelation in performance/chop/vol). VERDICT: do not build a regime/chop filter; the tool for the weak environment is SIZING (fractional clips / Kelly), not timing - ride the variance, the backtest Sharpe 24-29 already prices it. Generalizes the lane lesson (burn-stops failed, fractional sizing worked). Caveat: only rolling-30 tested; an external long-horizon trend signal is unexplored but near-zero autocorr across 28k trades argues against.

## 2026-06-13 10:30 UTC iteration: CLIP SWEEP COMPLETE (W1_n200 in) - corrects earlier $200 call

| window | $50 | $150 (mult) | $200 (mult) |
|---|---|---|---|
| W1 | $75,287 | $187,233 (2.49x) | $227,497 (3.02x) |
| W2 | $46,393 | $107,500 (2.32x) | $129,801 (2.80x) |
| W3 | $20,778 | $39,363 (1.89x) | $43,240 (2.08x) |
| TOTAL | $142,457 | $334,097 (2.35x) | $400,538 (2.81x) |

CORRECTION to 09:35 entry: with W1_n200 included, $200 = 2.81x (NOT the ~2.4x estimated from partial W2/W3 data). $200 adds ~20% NET over $150 (2.81x vs 2.35x) - meaningful, not negligible. Efficiency: $150=78% of linear, $200=70%. VERDICT: $150 is the SAFE pick for $2K (peak concurrent ~$750-1,500); $200 buys ~20% more NET but borderline on $2K bankroll (bursts approach $2K). Depth penalty grows with size but absolute P&L keeps rising through $200. Recommendation: pilot ramps $10-25 -> $100 -> $150, and $200 is a viable stretch if peak-concurrent capital holds. Past $200 untested (would need $5-10k tier). Clipsweep trades deleted post-scoring.

## 2026-06-13 12:35 UTC iteration: bonereaper favourite play - BTC cell (maker uplift confirmed, ETH/SOL pending)

BTC-5m favourite (aligned, align-min-mid 0.85, hold, W3, spot-only, sigma>=4): n=1636, hit 93.5%, TAKER NET $1,460, MAKER NET $2,052 (+40%). Maker model = taker_net + fees_saved + 20% rebate. Confirms maker execution lifts the thin favourite edge ~40% (the lever our BTC-only TAKER lane missed). PENDING (still running): ETH-5m, SOL-5m at align 0.85 AND 0.90. KEY OPEN QUESTIONS: does the favourite play clear on ETH/SOL where the latency FADE is structurally dead (would prove favourite-buying is a DISTINCT edge needing longshot-bias not book-speed)? does align 0.90 (his nearer-certain sweet spot) beat 0.85? Multi-asset pooled Sharpe vs single. Verdict deferred to next iteration when ETH/SOL cells complete.

## 2026-06-13 13:05 UTC: BONEREAPER favourite play COMPLETE -> BTC taker-viable (=lane), ETH maker-only, SOL dead

| favourite (W3, sigma>=4) | hit% | avg_px | taker | maker |
|---|---|---|---|---|
| BTC 0.85 | 93.5% | 0.91 | +$1,460 | +$2,052 |
| BTC 0.90 | 95.6% | 0.94 | +$1,076 | +$1,453 |
| ETH 0.85 | 92.0% | 0.92 | -$441 | +$158 |
| ETH 0.90 | 94.4% | 0.94 | -$238 | +$137 |
| SOL 0.85 | 93.5% | 0.93 | -$111 | -$3 |
| SOL 0.90 | 94.6% | 0.95 | -$186 | -$130 |

VERDICT (closes bonereaper question): his multi-asset favourite play = BTC favourites (TAKER-viable, +$1,460 = exactly our LANE, already deprioritized for fragility) + ETH favourites (MAKER-ONLY: taker loses -$441, maker flips +$158, premium too thin for fee) + SOL (DEAD even as maker, efficient book). So his edge is substantially MAKER EXECUTION on follower assets + rebate-tier-at-scale. We CAN do the BTC leg as taker today (it is the lane); the multi-asset expansion is MAKER-DEPENDENT and the follower legs are tiny even then (ETH +$150, SOL ~0). Does NOT rescue the lane into a new franchise. CONFIRMS: favourite-buying is a distinct edge from the fade (it is favourite-underpricing/longshot-bias), real on BTC, maker-thin on ETH, gone on SOL. Followable fully only with maker infra (the maker-future build, not $2K Bronze today). 0.90 vs 0.85: similar, 0.85 slightly better NET. Multi-asset pooled is ~90% BTC. Bonereaper CLOSED. bonefav trades deleted.

## 2026-06-13 14:05 UTC: "Avoid bad days" - conviction floor REFUTED, day-level movement signal FOUND

CONVICTION FLOOR REFUTED: bucketing candidate by |p_exo-0.5|, near-50/50 entries are the BEST per-trade (+$5.47-6.32, 22-28% vol) not the worst; favourites are LOWEST ($3.21-4.00). Mechanism: fade edge = book MISPRICING not winner-picking; near-50/50 windows with big book discounts pay most even at 44% hit (bought deep below win rate). A conviction floor would cut the best trades + half the volume = disaster. So bad days are NOT about entry selection - same entries are REAL edges on moving days, PHANTOM edges on pinned days (book is right when nothing moves = temporal SOL condition).

DAY-LEVEL MOVEMENT SIGNAL (first positive result): corr(day mean-sigma, day NET) = +0.284. Low-movement days avg +$969 (23/30 green), high-movement +$1,905 (27/30 green) ~2x. BUT: (a) low-movement days still profitable on average (do NOT skip); (b) not predictable in morning (intra-day autocorr +0.099); so cannot AVOID bad days. VERDICT: supports VOL-RESPONSIVE SIZING (clip ~ recent realized movement: bigger on moving tape = real edges, smaller on flat tape = phantom edges) - a continuous refinement of "sizing is the answer", grounded in +0.284. Would have shrunk today's loss without sacrificing profitable quiet days. QUEUE: backtest vol-responsive sizing (clip scaled by recent sigma) vs flat clips across W1/W2/W3. Ruled out as avoid-signals: conviction(refuted), recent-vol/chop/performance(~0.1 intra-day), calendar(Saturday only).

## 2026-06-13 14:15 UTC: VOL-RESPONSIVE SIZING backtest -> ADOPT-CANDIDATE (+21% NET, -40% drawdown, same capital)

Exposure-matched (mean multiplier=1, linear-pnl approx), candidate all 3 windows, clip scaled by entry sigma_bar_bps:
| rule | NET | Sharpe | maxDD |
|---|---|---|---|
| flat | $141,503 | 20.2 | -$3,599 |
| linear sigma clamp[0.5,2] | $171,758 | 19.7 | -$2,184 (+21% NET, -40% DD) |
| linear sigma clamp[0.33,3] | $175,078 | 19.1 | -$2,052 (+24% NET) |
| binary 1.5x/0.5x med | $168,920 | 19.1 | -$2,364 |
| sqrt sigma clamp[0.5,2] | $158,066 | 20.3 | -$2,524 (+12%, Sharpe-preserving) |

VERDICT: ADOPT-CANDIDATE linear-by-sigma clamp[0.5,2]. +21% NET AND -40% maxDD at SAME average exposure. Sizes UP on high-movement tape (real edges), DOWN on pinned tape (phantom edges, e.g. today). CAUSAL (entry sigma_bar_bps, no look-ahead; already logged live). Sharpe ~flat (win is NET+drawdown not ratio - sizing up big days adds variance). Stacks with clip-size work (vary $150 clip 0.5-2x by sigma). Deployable refinement; needs harness/shadow per-trade sigma-scaled sizing flag (small build, like basis-mom-tilt). Caveat: linear-pnl approx (re-confirm depth interaction at deploy). This is the constructive answer to "avoid bad days": cannot gate them (conviction refuted, timing ~0 autocorr) but vol-sizing makes them hurt 40% less + good days bigger. QUEUE: build --vol-sizing flag, re-validate with depth.

## 2026-06-13 14:35 UTC: DAILY-LOSS CIRCUIT BREAKER backtest -> REJECT (eats 40-69% NET)

Flat $50 candidate, offline daily stop:
| stop | NET | vs off | Sharpe | worst day | days triggered |
|---|---|---|---|---|---|
| off | $141,503 | - | 20.2 | -$2,855 | 0 |
| -$300 | $85,384 | -40% | 11.4 | -$351 | 48/90 |
| -$200 | $69,378 | -51% | 10.0 | -$251 | 55/90 |
| -$100 | $44,132 | -69% | 7.3 | -$149 | 68/90 |

VERDICT: REJECT tight daily circuit breaker. Protects worst-day (-$2,855->-$351) but destroys 40-69% NET + halves Sharpe. Mechanism: intraday dips are followed by RECOVERIES (live today: hold -$336->-$107); a stop locks the dip, misses the bounce. -$300 stop fires on 48/90 days = amputates normal variance not rare disasters. Same lesson as lane burn-stops / every gate. Bankroll protection = SIZING (survivable worst-day clips, vol-responsive sizing) NOT stopping. A circuit breaker only viable as a VERY WIDE black-swan backstop (rare trigger, large-% of bankroll), not a daily limit. Confirms user recollection. Vol-sizing FULL backtest (real book-walk) running separately.

## 2026-06-13 14:40 UTC iteration: VOL-SIZING FULL BACKTEST (real book-walk) -> ADOPT-CANDIDATE +13%/capital

Built --vol-sizing-ref-bps/lo/hi flag (replay.rs sizing branch: clip = notional*clamp(sigma_bar_bps/ref, lo, hi); causal, uses entry sigma). Ran ref=9.58 (mean sigma), clamp[0.5,2.0], vs flat $50, sigma>=3+Sat-skip, exposure-matched (NET per $1k capital deployed):
| window | flat $/k-dep | vsize $/k-dep | per-$ uplift |
|---|---|---|---|
| W1 trend | 127.8 | 138.2 | +8% |
| W2 mixed | 113.6 | 135.9 | +20% |
| W3 whip | 107.1 | 114.5 | +7% |
| TOTAL | 119.6 | 134.8 | +13% |

VERDICT: ADOPT-CANDIDATE confirmed via FULL book-walk backtest. Real uplift +13%/capital (vs +21% offline reweighting approx - the offline overstated because sizing UP on high-vol walks deeper into the book = depth penalty eats ~8pts). Still a genuine deployable improvement at constant capital, best on mixed tape (W2 +20%). Plus the drawdown reduction (offline showed -40%, real likely less). Causal + deployable to shadow (needs the same flag wired into shadow, small build). The --vol-sizing flag is now in the harness. Stacks with clip-size. volsize trades deleted post-scoring.

## 2026-06-13 14:50 UTC: vol-sizing clamp tuning -> [0.5,2.0] CONFIRMED, BAKED INTO CANDIDATE

W3 real book-walk, NET per $1k deployed: [0.5,2.0] $114.5 (best) > [0.33,3.0] $114.1 > [0.4,2.5] $113.6. flat $107.1. Tighter clamp wins (aggressive up-sizing depth penalty cancels benefit, as predicted; offline linear-pnl wrongly favoured [0.33,3]). FINAL vol-sizing params: --vol-sizing-ref-bps 9.58 --vol-sizing-lo 0.5 --vol-sizing-hi 2.0.

VOL-SIZING NOW BAKED INTO FINALIZED CANDIDATE. Full frozen config: edge-thresholds 0.12, exit-after-s 0 (hold), perp-price-weight 0.75, vol-estimator ewma + ewma-halflife-s 600, rearm-edge 0.08, max-clips 2, min-marginal-edge 0.04, sigma>=3 floor (--min-entry-sigma-bps 3.0), Saturday-skip, VOL-SIZING ref 9.58 clamp[0.5,2.0]. Backtest: +13%/capital over flat (90d), strongest mixed tape. Harness flag built+tested. Shadow is measure-only at $50 (vol-sizing applies at the PILOT EXECUTION layer, not the measurement shadow). Why vol not Kelly: Kelly (H7) REJECTED -32% (sizes by per-trade edge = concentrates unreliable big-edge coinflips); vol-sizing sizes by MOVEMENT = bets bigger when the strategy has real edge (speed-on-movement). vclamp trades deleted.

## 2026-06-13 15:30 UTC: Saturday MEAN-REVERSION hypothesis -> REJECTED (loses all 13 Saturdays)

Motivated by today's live Saturday 30% fade-hit (looked mean-reverting). Tested inverting the fade (bet toward strike / against displacement) on the pinned/Saturday regime.
- By sigma: inverting only wins at sigma<2bps (+$7.71/t, n=78=0.26% of trades, already pruned by sigma-3 floor). Above 2bps the fade dominates, inverting loses -$4 to -$13/t.
- Saturday-only (13 backtest Saturdays): fade NET -$472 (breakeven, the dead day), INVERTED NET -$22,347. Inverting loses on ALL 13 Saturdays (-$274 to -$4,025 each).
VERDICT: REJECT. Saturdays do NOT mean-revert - they are COIN-FLIPS (fade hits 50-64%, slightly right, loses only on fees). Today's 30% hit was a ~3-sigma BAD DRAW, not a regime. Inverting loses on direction AND fees = -$22k. No exploitable structure in Saturday tape in EITHER direction. The eye saw "reversion" in one vivid day; 13 Saturdays say coin-flips. CONFIRMS: skip Saturday (cannot trade it either way), do not build a reversion sleeve. Classic one-vivid-day trap, killed by the multi-Saturday backtest.

## 2026-06-13 16:25 UTC iteration (E): H5 perp-price-weight -> ALL-THREE-WINDOWS COMPLETE, pw0.75 ADOPT (validates baked config)

Folded the now-complete W1 (trend) leg into the H5 verdict (was W3-only + partial W2). Base = pw0.5 (W1 base proxy ~31,400 = H1 timeout-60 cluster 31,292-31,575; W2 base from prior; W3 base 19,835). Sharpe not recomputed for W1 (summary jsons carry no per-trade series; trades purged per disk rule) - NET+hit decisive and track Sharpe in the W2/W3 rows.

| pw | W1 trend NET (vs base) | W2 mixed (vs base) | W3 whip NET (vs base) | hit W1/W3 |
|---|---|---|---|---|
| 0.25 | 29,297 (-6.7%) | 27,285 (-2.6%) | 17,442 (-12%) | 0.603/0.605 |
| 0.5 base | ~31,400 (-) | base | 19,835 (-) | -/0.626 |
| 0.75 | 32,152 (+2.4%) | 27,701 (-1.1%) | 21,465 (+8.2%) | 0.610/0.624 |
| 1.0 | 31,698 (+1.0%) | 27,542 (-1.6%) | 22,767 (+15%) | 0.604/0.618 |

VERDICT: ADOPT pw0.75 (CONFIRMS the finalized config). pw0.75 is the regime-robust optimum: best variant in TREND (W1 +2.4%), flat in MIXED (W2 -1.1%, within 10% tolerance), strong in WHIPSAW (W3 +8.2%); never violates the consistency rule. pw1.0 over-leans on the perp - wins only whipsaw big (+15%) but trails 0.75 in trend/mixed (the regime where the static spot blend matters); rejected as less robust. pw0.25 loses in all three. This closes H5 with full cross-regime support for the perp-price-weight 0.75 already baked into [[leading-config-hold012]] - previously only W3-validated. No new run; scored from existing batch0612 outputs.

## 2026-06-13 17:10 UTC iteration (F): F3 basis-momentum stake tilt -> ADOPT-CANDIDATE (weak, whipsaw-concentrated); offline +9-15% was exposure-inflated

Built the feature: `--basis-mom-agree <m> --basis-mom-disagree <m>` (replay.rs sizing branch). basis_mom_60s_bps = [(perp/spot-1)@t - @(t-60s)]*1e4, computed in belief_pass from the already-loaded PerpState+spot, stored on Decision; stake multiplied by agree-mult when the 60s basis change agrees with the entry side (Yes wants basis rising), disagree-mult otherwise. NOT an entry gate (same trades). Tested 1.25/0.75 (the f3-offline values) vs control 1.0/1.0 on the base COMBO (perp 0.5, exit30, thr0.16, passive-60), all three windows. EXPOSURE-MATCHED (NET per $1k deployed) because the tilt deploys ~5% more capital:

| window | control NET/$1k | tilt NET/$1k | real uplift | raw NET (ctrl->tilt) | hit |
|---|---|---|---|---|---|
| W1 trend | 82.66 | 83.82 | +1.4% | 31,380->33,152 (+5.6%) | 0.611 (same) |
| W2 mixed | 105.13 | 105.85 | +0.7% | 28,000->29,390 (+5.0%) | 0.601 (same) |
| W3 whip | 132.92 | 140.84 | +6.0% | 19,835->22,252 (+12.2%) | 0.626 (same) |

Same trade count + same hit in every window confirms a pure SIZING tilt (reweights stakes, takes no new trades). VERDICT: ADOPT-CANDIDATE (weak). The signal is real and consistent (positive all three windows, never loses, passes the consistency rule) but the genuine per-capital edge is +6% in whipsaw and only +0.7-1.4% in trend/mixed; the raw 5-12% NET gain is ~half just extra exposure. The f3 offline +9-15% OVERSTATED (same exposure-inflation trap as vol-sizing offline +21% -> real +13%). DO NOT bake yet: decisive follow-up = retest on the FINALIZED hold config (perp-price-weight 0.75), where much of the basis information may already be captured by the heavier perp blend (overlap/double-count risk) - the +6% whipsaw value could shrink. If it survives on perp-0.75-hold, adopt as a free low-conviction whipsaw overlay (two params, no new trades). Feature flag built + committed-to-tree (not git). f3live trades deleted post-scoring; .json kept.

## 2026-06-13 17:30 UTC: SATURDAY on the FINALIZED HOLD config (not fade) -> KEEP SKIP (hold nets -$954, amplifies variance)

Re-tested the Saturday question on the ACTUAL finalized config (hold-to-redemption: edge 0.12, perp 0.75, exit-after-s 0, ewma-600, rearm clip-2, min-marginal 0.04; sigma>=3 applied offline since alpha lacks the floor flag), motivated by today's live shadow hold-ewma +$974 Saturday recovery. Ran W1+W2+W3, filtered to the 13 Saturdays, fee-net per-Saturday:

| date | n | hit% | NET |
|---|---|---|---|
| 2026-02-14 | 337 | 59% | -927 |
| 2026-02-21 | 256 | 56% | +354 |
| 2026-02-28 | 237 | 53% | -377 |
| 2026-03-07 | 397 | 57% | -452 |
| 2026-03-14 | 396 | 57% | -510 |
| 2026-03-21 | 403 | 57% | +73 |
| 2026-03-28 | 386 | 59% | +707 |
| 2026-04-04 | 249 | 43% | -2,855 |
| 2026-04-11 | 363 | 58% | +886 |
| 2026-04-18 | 350 | 64% | +2,528 |
| 2026-04-25 | 248 | 54% | +570 |
| 2026-05-09 | 267 | 56% | -12 |
| 2026-05-16 | 375 | 49% | -938 |
| TOTAL | | | -954 (6/13 green, mean -$73/Sat) |

VERDICT: KEEP SATURDAY-SKIP. Hold-on-Saturday nets -$954 vs skip $0, and is SLIGHTLY WORSE than the fade's -$472. Mechanism (the insight): hold-to-redemption AMPLIFIES Saturday variance both ways - it never exits, so it rides the full binary outcome; the recovery days are bigger (+$2,528) but the blowups are bigger too (-$2,855 on 04-04 = worst of sample, exceeds the fade's -$2,363 same day). On coin-flip Saturday tape, riding the binary nets negative. Today's live shadow hold-ewma +$974 is one of the ~6/13 green Saturdays = fully consistent with the -$954 aggregate, NOT evidence to un-skip (4th one-vivid-day trap dodged today). Even reduced-size is negative EV (mean -$73/Sat). Saturday-skip stays in [[leading-config-hold012]]. satbt trades deleted post-scoring.

## 2026-06-13 18:40 UTC: DAY-OF-WEEK on hold config -> SATURDAY UNIQUELY BAD, Sunday fine (skip stays Sat-only)

Follow-up to the Saturday verdict: is the negative/coin-flip pattern Saturday-specific or weekend-wide? Hold config (finalized), all 3 windows, sigma>=3, fee-net, by weekday:
| day | NET | $/trade | hit% | green-days |
|---|---|---|---|---|
| Mon | +23,019 | +5.97 | 61% | 13/13 |
| Tue | +23,204 | +6.52 | 62% | 11/12 |
| Wed | +25,409 | +6.67 | 62% | 11/12 |
| Thu | +30,141 | +6.95 | 63% | 12/14 |
| Fri | +21,093 | +5.13 | 61% | 11/13 |
| Sat | -954 | -0.22 | 56% | 6/13 |
| Sun | +19,590 | +4.43 | 59% | 12/13 |

VERDICT: Saturday is the ONLY negative day and ONLY coin-flip green-rate (6/13). Sunday is solidly positive (+$19.6k, 12/13 green) - so this is NOT a generic weekend effect; skip stays SATURDAY-ONLY, do NOT extend to Sunday. Mild weekend-liquidity drag visible (Sun $4.43/trade vs weekday $5-7) but Sunday clears it; only Saturday's drag tips the regime negative. Weekday total +$122,867, weekend +$18,636 (all Sunday). Confirms [[leading-config-hold012]] Saturday-skip is correctly scoped. satbt trades deleted.

## 2026-06-13 21:00 UTC: Saturday CORE-AFTERNOON window (12-17 UTC) on hold config -> PROMISING but thin (do not deploy, shadow-test)

Tested restricting Saturday to a fixed a-priori afternoon window (12:00-17:00 UTC, the US-morning/UK-afternoon liquidity overlap) on the hold config, 13 Saturdays, sigma>=3, fee-net.
RESULT: 12-17 window = +$1,998 (8/13 green) vs full-Saturday -$954 (6/13) vs skip $0. Mechanism: dodges the toxic US-evening cluster 18:00-21:00 UTC (18:00 -1659, 19:00 -1802, 21:00 -1149; all 47-48% hit = the most consistent bad feature in the hour profile).
CAVEATS (why NOT deploy): (1) 8/13 green still ~coin-flip; 5 Saturdays the window LOST. (2) tail-carried - one Saturday (04-18 +$1,205) = 60% of the +$1,998; ex-that +$793/12. (3) in-sample on 13 days, no clean holdout (June/May19-28 sealed); the earlier finding that WHICH part of Saturday is good varies day-to-day still applies.
VERDICT: plausible real effect (evening toxicity is the robust signal) but evidence too thin to deploy. Conservative = keep skip-all-Saturday. Defensible-aggressive = trade 12-17 UTC at REDUCED size. CLEAN PATH = stand up a Saturday-afternoon-only SHADOW stream to collect live OOS Saturdays before any real-money commit (don't fit harder on the 13). Queued: F-SAT-PM shadow stream. satbt trades deleted.

## 2026-06-14 ~10:00 UTC: EWMA-600 vs REALIZED-3600 on the HOLD config (all 3 windows, deterministic) -> REJECT EWMA, use REALIZED

Motivated by the shadow EWMA-underperformance question (which the shadow couldn't settle: ~$100/day independent-process noise floor swamped a ~1% effect; AND my shadow scorer had a clip-2 double-count bug, now fixed via ladder_settle_pnl_usd direct field). Ran the deterministic backtest on the finalized hold config (edge 0.12, perp 0.75, exit-after-s 0, rearm clip-2, min-marginal 0.04), realized-3600 vs ewma-600, W1/W2/W3, fee-net. Accounting self-checked: total_pnl == Σ per-trade pnl (exact), gross-fee==pnl (0 bad/run), partial fills present (marginal-edge floor).

| window | realized | ewma | diff |
|---|---|---|---|
| W1 trend | 73,198 | 74,041 | +1.2% (ewma) |
| W2 mixed | 49,026 | 49,522 | +1.0% (ewma) |
| W3 whipsaw | 26,026 | 19,611 | -24.6% (ewma) |
| TOTAL | 148,250 | 143,174 | realized +3.4% |

VERDICT: REJECT EWMA-600, ADOPT REALIZED-3600 in the frozen config. EWMA wins calm regimes by ~1% but FAILS the consistency rule with -24.6% in whipsaw (fast vol estimate overreacts on choppy tape - same behavior seen live Saturday where ewma "dug deeper"). Realized is regime-robust and +3.4% overall. CORROBORATION: the live shadow (realized hold > ewma hold both Sat +404 vs +270 and Sun) was SIGNAL not noise - current regime is volatile/whipsaw, exactly where realized wins. Bonus: simpler config (one fewer param); ewma total 143,174 == the memory's frozen-config backtest figure, confirming the memory carried the worse estimator; switching to realized lifts it to 148,250. EXECUTION NOTE (from this session): both backtest and live shadow run at effective capture_frac=1.0; capture is NOT a meaningful optimism source at $50 (fills always complete on these books - user-confirmed); the honest live haircut is the markout deflator ~0.45-0.66 applied to gross (latency+adverse-selection), giving realized 90d range $66.7k-$97.8k. ewmatest trades deleted post-scoring.

## 2026-06-14 ~13:30 UTC: HOUR-OF-DAY x DAY-TYPE on hold config -> NO US-hours gate; only Saturday-skip is real

Tested the "weekend climbs in US opening hours" hypothesis (from a vivid live Sat-06-13 evening climb). Hold config, W1+W2+W3, sigma>=3, $/trade by hour UTC x day-type:
- WEEKDAY: profitable EVERY hour (+1.25..+13.76/t). US-hrs(12-21) +7.37/t vs rest +6.08/t -> only +1.29 better, both strongly +. No gate helps; trade all hours.
- SATURDAY: US-hrs(12-21) NEGATIVE -2.15/t (t=-1.7); 18-19 UTC toxic (-9.1, -12.3/t). The OPPOSITE of a climb. Per-window US-hrs flips sign: W1 -3.20 / W2 +2.11 / W3 -5.70 = NOISE.
- SUNDAY: positive everywhere; US-hrs +6.17/t vs rest +4.65/t (Sunday just good, not a US-hours gate).
VERDICT: REJECT a US-hours/time-of-day gate. Weekdays profitable all day; the only reliable calendar gate is Saturday-skip (and within Saturday the 18-21 UTC US-evening is the toxic pocket, which skip already removes). The live Sat-06-13 evening green was a single-window draw running OPPOSITE the historical Sat US-evening toxicity - the one-vivid-window trap again. No config change. todstudy trades deleted.

## 2026-06-14 ~20:00 UTC: clip-2 SIDE-FLIP suppression hypothesis -> REJECTED (flip clip-2 is the BEST clip type)

From a live shadow example (slug ...422500: clip-2 flipped to the opposite side, entered up@0.27 during a rally, lost on the reversal) we hypothesized: clip-2 side-flip = whipsaw "chasing" risk; restrict clip-2 to clip-1's side (pure average-down) and/or gate by regime; suspected this was WHY ewma is worse in whipsaw. Tested OFFLINE on W3 (whipsaw) baseline hold trades (group by open_ts_ns, order by decision_ts, classify clip-2+ as same-side vs flip-side):
- clip-2 SAME-side: n=1615, NET +$2,037, +$1.3/clip, 58% win.
- clip-2 FLIP-side: n=409, NET +$6,260, +$15.3/clip, 60% win.
- Suppressing flip clip-2 REMOVES $6,260 = -24.1% of W3 NET.
VERDICT: REJECT. Flip clip-2 is the MOST profitable clip type IN whipsaw (the window we predicted it'd be worst). Mechanism reframe: it's not "chasing" - it's the core fade edge on the 2nd clip (BTC moves -> opposite side gets cheap AND belief moves with it -> buy a genuinely underpriced side at high edge; the book lagged a real move). The ...422500 loss was a reversal = the minority (40%). COROLLARY: ewma's whipsaw weakness is NOT via clip-2 side-flip (side-flip is +EV); it's the fast vol estimate mis-estimating sigma in choppy tape directly. NO code change; clip-2 logic stays. Classic one-vivid-example trap, killed by aggregate backtest. (W3-only; W1/W2 would likely show flip clip-2 even better. Offline on existing baseline.trades.jsonl.)
