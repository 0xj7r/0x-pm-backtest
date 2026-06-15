# Autoresearch backlog

Rules (every iteration, non-negotiable):
- Selection runs on ALL THREE exploration windows: W1 (2026-02-12..03-31 trend), W2 (04-01..04-30 mixed), W3 (05-07..05-18 whipsaw). A variant is adoptable only if it is consistent across regimes: it must not lose more than ~10% NET in any window it does not win, and per-window numbers are reported in every verdict. Never conclude from a single window; W3 alone is whipsaw-biased.
- Cheap triage is allowed: run W3 first only as a fail-fast (a variant that is catastrophic there can be rejected early); anything that survives triage MUST get W1+W2 before any verdict other than REJECT.
- NEVER run any date in 2026-05-19..2026-06-30. May 19-28 is the one-shot test window (weekly budget, human-approved use only); June is sealed holdout.
- Always --fee-curve-rate 0.07. Score on fee-net NET, daily Sharpe, and W1/W2/W3 consistency. Treat at-touch dollars as relative (live deflator ~0.45).
- Base config unless the hypothesis says otherwise: BTC-5m, --edge-thresholds 0.16 --perp-symbol BTCUSDT --perp-price-weight 0.5 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08 --exit-after-s 30 --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90.
- One hypothesis per iteration. Append verdict to ledger.md before picking the next.
- Disk guard: skip iteration if <20GB free on /System/Volumes/Data. Delete *.trades.jsonl after scoring (keep .json summaries and the ledger).
- Rebuild target/fast/pm-app and check error count before any run using new flags.

## Queue (priority order)

STATUS 2026-06-12: a detached batch (scripts/batch0612.sh, output data/runs/alpha/batch0612/, log batch.log there) is running H0/H1/H2/H4/H5/H6/H7/F1/ETH across W3 then W2 then W1. Iterations should SCORE completed batch outputs (fee-net NET, daily Sharpe, hit, per window; the established python pattern) and write ledger verdicts, NOT re-run them. F2/H3/F3/F4/F5 and the markout study are delegated to background agents writing reports into this directory; if a report file appears, fold its verdict into the ledger and queue any follow-ups it proposes. Only start a NEW harness run if the batch is fully done and scored.

Interleave: alternate one E-item (execution/tuning, numbered below) with one F-item (fundamental signal, F-section) each iteration, starting E.

1. H0 threshold refit under passive exit: --edge-thresholds 0.10,0.12,0.14,0.16 with --exit-at-mid --passive-exit-timeout-s 60. The 0.16 choice was fee-driven; with fees -43% the fee-net frontier may move back toward 0.12, which is also a participation lift. Report entries/day per threshold per window.
2. H1 passive-exit param sweep: --exit-at-mid --passive-exit-timeout-s in {30, 90, 120} vs the validated 60. Does a longer wait raise mid-fill rate without giving back whipsaw P&L?
2. H2 exit-horizon under passive exit: --exit-after-s in {45, 60, 90} with --exit-at-mid --passive-exit-timeout-s 60. Fee-free exits may favor longer convergence capture than the taker-optimal 30s.
3. H3 sigma-conditioned exit style: compare passive-exit-everywhere vs base-everywhere per regime cell (calm_low_vol vs expanded/whipsaw cells in the report json). If passive wins calm and loses whipsaw, estimate the value of switching on sigma_bar_bps.
4. H4 rearm interaction: combo with passive exit, rearm on vs off (--max-clips 1). Yesterday's live read suggests clip-2 bleeds; does the backtest agree under the passive exit?
5. H5 15m horizon with passive exit: btc-updown-15m manifests, same combo. Wider spreads may make the passive exit save more there.
6. H6 kelly sizing fee-net: --kelly-sizing on the passive-exit combo. Does sizing by edge beat flat $50 after fees at touch AND after the 0.45 deflator intuition?
7. H7 momentum overlay: --momentum-lookback-s 300 --momentum-weight in {0.1, 0.2} on the passive-exit combo.
8. H8 vol estimator: --vol-estimator ewma --ewma-halflife-s in {600, 1800} vs the 3600 rolling window, passive-exit combo.
9. H9 entry timing: --enter-within-close-s variants for the fade (does excluding the first 60s of each window, when books are thin, improve per-trade edge?). Requires checking flag semantics first; skip if it only gates lane-style entries.
10. H10 ETH-5m sanity under passive exit: prior alts verdict was DEAD for the fade family; only worth one cheap re-check because the passive exit changes fee economics materially. If still negative on W3, close permanently.

## Fundamental signal queue (F-items)

New alpha sources, not parameter tuning. Same rails apply. Prefer hypotheses runnable with existing harness flags or small feature additions; record a build-vs-skip note when the harness lacks support rather than hacking around discipline.

F1. Window-open momentum: does the first 30-60s return of a 5m window predict the close? Testable now: --momentum-lookback-s {30, 60} --momentum-weight {0.1, 0.3, 0.5} on a momentum-only config (edge threshold low, fade off if the harness supports aligned-mode as the momentum expression). Compare to fade on the same windows: complementary or overlapping P&L days?
F2. Session seasonality gating: process_markets already accepts seasonal tables. Build hour-of-day x day-of-week P&L cells from existing W1-W3 trades files (no new runs needed), test whether gating out structurally negative hours lifts NET and Sharpe out of sample across windows (fit on W3 cells, apply to W1/W2).
F3. [DONE 2026-06-13, see Done] Perp basis momentum as stake tilt: feature built (--basis-mom-agree/--basis-mom-disagree), ADOPT-CANDIDATE weak (whipsaw +6%/capital, trend/mixed +0.7-1.4%; offline +9-15% was exposure-inflated). Follow-up F3b queued.
F4. Cross-horizon prior: the 15m/1h market's implied probability as a prior for 5m fair value when the 5m book is thin. Offline study first: join 5m and 15m tapes on overlapping windows from existing caches, measure whether 15m mid leads 5m repricing.
F5. Binance liquidation cascades: forceOrder stream is free live and historical dumps exist. Hypothesis: large liquidation bursts predict continuation over the next 1-3 minutes, exactly the 5m horizon. Offline: download May liquidation data, align with W3 trades, check whether fade losses concentrate during cascades (filter) or whether cascades are an entry signal (aligned momentum).
F6. Polymarket aggressor-flow fade: from book tape deltas, infer taker bursts (size lifted at ask). Hypothesis: retail burst-buying of a side at extremes is fadeable beyond our BSM edge. Needs a tape-derived feature; spec offline from existing caches before any harness change.
F7. Whale-flow mirror (live-only study): watch the activity feed for the profitable late-favourite cohort wallets; measure (shadow, no orders) whether following their entries within 2s clears fees. Cannot be backtested honestly (feed history incomplete); park for a live measure-only module, do not block the loop on it.

## Follow-ups queued from reports (2026-06-12)

- F10 lane risk shaping (user-requested, HIGH): the lane's binary tail (11 burns 2026-06-12, clustered overnight) needs shaping. Three variants on the lane config (aligned 0.85, thr 0.02, enter-within-close 120, stop-before-close 5, hold, sigma>=4 floor offline), W3 + W1 + W2 on existing BTC-5m books: (a) convex tail hedge grid --tail-max-price {0.01, 0.02, 0.03, 0.08} x --tail-frac {0.1, 0.2, 0.3} (grid revised down after wallet 0x8d1d evidence: that trader buys 600-share tails at 1-2c, cost 2-4% of a favourite clip, ~100x convexity; also evaluate the 1-2c tail as a STANDALONE strategy: breakeven flip rate is ~1%+fees from decided states, measure actual flip frequency from the tapes); report NET, worst-day, max-burn-run, Sharpe vs unhedged (expect NET drag, question is tail relief per dollar); (b) daily loss budget simulation offline on lane trades (stop after K burns or $X loss per day, K in {2,3,4}, replay day by day); (c) fractional sizing realism check: burns cluster in time, so measure burn autocorrelation (do burns predict burns within 1-2h?) before crediting any diversification from smaller clips. Verdict = recommended live risk config for the lane.

- H11 timeout predictor (from H3 report): the passive-exit value reduces to predicting the 8-13% of exits that time out (-$21/trade vs +$3 when filled). Sigma does not predict timeouts; test book depth / quote stability at exit time as predictors from existing trades + tape, offline first.
- F8 weekend sizing tilt (from F2 report): Sat/Sun expectancy is stably 2-4x weaker than weekdays across all three windows but still positive; test a stake downweight (not a gate) with proper OOS protocol.
- LIVE (not a backtest item, from markout report): cut Binance-to-decision latency (101ms receipt lag vs 9ms Polymarket book); worth ~0.66 to ~0.89 mark-basis capture. No slip caps or abort-on-race logic (all counterfactuals lose).

## Done
- 2026-06-13 F3 basis-momentum stake tilt: feature BUILT (--basis-mom-agree/disagree, replay.rs sizing branch; basis_mom_60s_bps on Decision from PerpState+spot) + validated all 3 windows on base COMBO. ADOPT-CANDIDATE weak: exposure-matched +6.0% whipsaw / +1.4% trend / +0.7% mixed (same trades, same hit = pure sizing tilt); offline +9-15% was exposure-inflated (~half the raw NET gain is just +5% more capital). See ledger 17:10. FOLLOW-UP F3b (queued, HIGH): retest 1.25/0.75 on the FINALIZED hold config (perp-price-weight 0.75) - the heavier perp blend may already capture the basis signal (overlap/double-count); if +6% whipsaw survives there, bake as a free low-conviction overlay, else shelve. Also worth: scale multiplier with |s60| (monotone tercile pattern in f3 offline report).
- 2026-06-13 H5 perp-price-weight CLOSED (all 3 windows): pw0.75 ADOPT (W1 +2.4% trend / W2 -1.1% mixed / W3 +8.2% whip; regime-robust); validates the baked 0.75. pw1.0 rejected (whipsaw-only +15% but trails 0.75 in trend/mixed), pw0.25 loses everywhere. See ledger 16:25. NEXT E-items: H4/H2/H7-momentum W1 legs also complete in batch0612, fold to final verdicts (H4 leaning keep-clip2, H2 leaning REJECT-keep-30s, H7 ewma600 already baked).
- 2026-06-13 F10 lane risk shaping CLOSED: (b)(c) burn-count stops REJECT, (a) entry hedge REJECT FINAL all windows. Policy = fractional sizing + bankroll circuit breaker. Cheap-tail spun off to F11.

- 2026-06-12 H0 thresholds 0.10/0.12 REJECT (W2+W3); 0.14 pending W1, leaning REJECT. Keep 0.16. See ledger.
- 2026-06-12 H1 passive timeout 30/90/120 REJECT (W2+W3 flat to -5%); keep 60. See ledger.
- 2026-06-12 H3 sigma-conditioned exit REJECT (report h3_sigma_exit.md folded into ledger).
- 2026-06-12 H10/ETH recheck REJECT FINAL (W3 gross-negative, -$31.3k NET, hit 0.334); ETH fade family closed permanently.
- 2026-06-12 F1 aligned e30 REJECT (gross-negative); aligned hold pending W1/W2, day-corr +0.62 with fade already weakens complementarity.
- 2026-06-12 F2 seasonality gating REJECT (report f2_seasonality.md folded into ledger).
- 2026-06-12 markout attribution study complete (report markout_attribution.md folded into ledger; action is live latency, not backtest).
- 2026-06-12 W3 scored for H2/H4/H5/H6/H7: all INCONCLUSIVE-pending-W1W2. Promising: H5 pw0.75-1.0 (+8/+15% NET, Sharpe 44), H7 ewma600 (Sharpe 46.5 NET-flat), H6 15m additive (+$4.7k standalone). See ledger.
