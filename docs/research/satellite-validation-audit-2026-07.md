# Satellite Strategy Validation Audit (2026-07-02)

Scope: every non-fade ("satellite") strategy claim in the repo, audited against the
canonical validation standard. The fade (exo_fade / Class A) is out of scope; it is
the only strategy known to pass.

**Validation standard applied.** Nothing is VALIDATED without out-of-sample evidence
on the canonical fee-net harness (`--fee-curve-rate 0.07 --latency-ms 250`, per
`docs/PROD.md` §4 and `docs/fill-model-calibration-2026-07.md`) or an equivalent
live/shadow record: selected on TUNE (2026-02-12 to 2026-04-30), confirmed frozen on
VERIFY (2026-05-07 to 2026-05-18) or later sealed data, passing the adoption gates in
`docs/research/strategy-hunt/07-strategy-forward-plan.md` §4 (fee-net NET > 0, daily
Sharpe > 1, worst day > -5% of bankroll, hit > 0.50 directional). June 2026 is BURNED
for selection; July 2026 is the sealed window.

Note on older evidence: most satellite backtests below ran at latency 150ms (or the
BR2 engine's own 500ms fill model), before the 250ms canonical recalibration. That
alone means no pre-July satellite number is on the canonical accounting basis.

## Summary

| # | Candidate | Evidence type | Selection hygiene | Verdict |
|---|---|---|---|---|
| 1 | BR2 `late_favourite` lane (bonereaper_v2 engine) | Full-history AWS grid Feb 27 to May 20 (+$4,727 attribution); VERIFY walk-forward @ $1K | Grid selected on the same full history it is judged on; VERIFY run exists and FAILS gates | **REJECTED-BY-EVIDENCE** (as deployable satellite at $1K) |
| 2 | BR2 cluster router (global regime classifier) | Offline 60/40 splits on Feb 27 to Mar 22 overlap + May overlap + synthetic policy search | Clusters are fill-derived (post-hoc), sizing synthetic, never engine-replayed; May sample flips polarity | **UNVALIDATED** |
| 3 | Regime gates skip_calm + skip_expanded_mixed (deployed Jun 19) | 6 days of live shadow tape (Jun 14-19); OOS Jun 20-30 | Selected and judged on the same 6 days; OOS blocked 99% of entries | **REJECTED-BY-EVIDENCE** (removed from prod 2026-07-01) |
| 4 | 4 directional satellite candidates (flow, basis+OI, alignment, reversal hybrid) | Design doc only; zero backtests | N/A (nothing run); premise retracted 2026-07-01 | **UNVALIDATED** |
| 5 | `clean_directional_pressure` (directional.rs) | Hand-picked weights, no fit, no run | Self-declared UNVALIDATED in code | **UNVALIDATED** |
| 6 | Whale split/redeem roller (0x4d64518a mirror) | Log-only shadow soak tooling; wallet observation; no P&L study | No backtest, no edge quantified anywhere in repo | **UNVALIDATED** |
| 7 | Both-sides-hold sell-loser idea | Queued idea (project memory only); no code, no doc, no run | N/A | **UNVALIDATED** |
| 8 | Tail convexity sleeve (Class C) on BTC 5m/15m | F11 tape scan, F10(a) hedge grid W1-W3, BR2 convex_tail full history, P3 aggregate | P3 "+65%" claim unsupported (see detail); direct tests all negative or null | **REJECTED-BY-EVIDENCE** (BTC short-horizon); ETH-4h deep tail remains an unvalidated lead |
| 9 | Late-favourite expiry lane (Class B, alpha harness) | Fee-net positive on W1/W2/W3 BTC at sigma>=4 (~$68-126/day per $50 clip); cross-asset fails | Config tuned ON May 7-18 (the VERIFY window); no untouched confirmation window; fails worst-day gate in WF frame | **PARTIAL** (edge direction real on BTC, config in-sample, economics thin, gates fail) |
| 10 | F6 Polymarket aggressor-flow fade | 4,451-event offline study W2+W3 | Clean (measured, not fit) | **REJECTED-BY-EVIDENCE** (NO-SIGNAL, t = -9.8) |
| 11 | Class routing C10 (regime-routed A/B/C allocation) | Doc table only, flagged "not yet validated" | Nothing run | **UNVALIDATED** |
| 12 | Multibook satellites (BTC-15m, ETH-5m/15m, XRP-5m) | Fee-net harness, but W1/W2 are 8-day samples (15m) / partial backfills (ETH); 150ms latency | Same windows used to explore and to judge; no HOLDOUT, no port, no live record | **PARTIAL** (BTC-15m, ETH-5m strongest; none deployable) |

Nothing in the satellite set meets the VALIDATED bar. The strongest genuine
candidates are #9 and #12, and both need a proper TUNE-select / VERIFY-or-sealed
confirm cycle at 250ms before any capital.

---

## 1. BR2 `late_favourite` lane (bonereaper_v2 engine)

**Claims audited.** `docs/archive/2026-06/active_btc5m_experiments.md`: selected 1K profile
`+$8,990.21` (+899%) over 23,705 markets Feb 27 to May 20, with
`br2_late_favourite_load +$4,726.87` attribution.
`docs/archive/2026-06/handoff/2026-06-19-regime-gates-high-variance-research.md` §6 markets BR2
`late_favourite` as the primary directional satellite.

**Evidence type.** AWS portfolio grid, own fill model (`lat500ms` labels, engine
defaults with maker rebates), not the canonical alpha harness. One window: the full
history. Plus one walk-forward VERIFY run at $1K
(`docs/research/strategy-hunt/02-walkforward.md`). Plus a live deployment cycle in
June (see project memory `june-deploy-cycle-postmortem`), which ended in account
drawdown driven mostly by non-BR2 factors but produced no clean positive live record
for BR2 either.

**Selection hygiene.** Poor. The +$8,990 profile is the best-of-grid on the full
Feb-May history; there is no held-out window. The same doc shows the decay in-sample:
last third `-$227.08`, last 30d `+$20.80`, with mid-wide final-range markets losing
`-$3,006.12`. Crossed-mid fills are a persistent toxic path
(`br2_late_favourite_load` crossed-mid `-$2,739.40` at the 8,500 checkpoint). The one
honest OOS-style test that exists, VERIFY walk-forward at $1K clips
(`--clip-fraction-of-equity 0.025 --max-clip-usdc 30`), returned **+$327, daily
Sharpe 2.6, worst day -$334, verdict REJECT** (fails the Sharpe and worst-day gates;
-$334 is 6.7x the -$50 bar at $1K).

**Known failures.** Mid-wide 0.78-0.93 final-range bucket structurally negative;
crossed-mid tail (memory: `br2-loss-limiting-hedge-first`); participation collapse in
the last 30d of its own selection window; every replay-safe throttle tested removed
positive PnL.

**Verdict.** REJECTED-BY-EVIDENCE as a deployable satellite at current sizing. The
full-history headline is selection-biased and off-canonical accounting; the only
gate-tested run fails.

**Proper validation requires.** Re-run the frozen selected profile (no re-grid)
through walk-forward on TUNE-only markets, freeze, then confirm on VERIFY and one
sealed July slice: `pm-app walk-forward --strategies bonereaper_v2 --portfolio-mode
--starting-cash 1000 ...` with the canonical manifest slices, scored fee-net. Pass
bar: VERIFY NET > 0, daily Sharpe > 1, worst day > -$50 at $1K, and last-30d-style
decay check (final-third NET > 0.5x first-third NET/day).

## 2. BR2 cluster router (`docs/archive/2026-06/global_regime_classifier_router.md`)

**Claims audited.** "Cluster router test PnL +$3,138.08 vs BR2-only +$2,294.25";
"best adaptive policy +$5,671.72, max DD 13.55%"; BR2 per-cluster table
(`clean_directional_path +$1,957.65 / 88.1%`, `expanded_reversal_pressure +$2,919.76
/ 76.6%`, `expanded_high_flip +$1,540.28 / 74.4%`) which the Jun 19 handoff resells
as satellite evidence.

**Evidence type.** Offline dataset joins on 6,500 overlapping markets (Feb 27 to
Mar 22) plus 5,752 May markets; 60/40 or chronological-fold splits; policy search is
synthetic sizing (scales already-realized per-market PnL), never replayed through the
engine. The doc says this itself: boosted overlays "are synthetic sizing results and
must be rerun through the actual engine before being trusted", and the router "is not
yet deployable as-is because its current diagnostic_cluster is fill-derived".

**Selection hygiene.** Weak on three axes. (a) The cluster features are derived from
strategy fills on the market being routed (post-hoc provenance, not the pre-route
live-safe layer, which was only later added to decision logs). (b) The per-cluster
BR2 PnL table is computed from the in-sample-selected full-history BR2 run (#1), so
it inherits that selection bias. (c) Polarity is sample-dependent: BR2-only on the
May overlap is **-$18.81**, and `expanded_high_flip` flips sign between periods
(-$701.69 early vs +$195.99 May), which the doc concedes means "do not use one global
rule". The candidate labels (BR2 strong in clean_directional / reversal_pressure)
therefore have no out-of-sample confirmation.

**Known failures.** May-sample collapse; feature-source `br2` makes the router
degenerate to BR2-only; legacy decision logs lack the regime fields
(pre-`4d8fe2fe` logs are explicitly "not deploy-validation evidence").

**Verdict.** UNVALIDATED. A real research signal (adaptive routing beat both fixed
strategies on the combined folds) but every number is off-harness, partially
lookahead in feature provenance, synthetic in sizing, and anchored to a REJECTed
underlying strategy.

**Proper validation requires.** The doc's own Next Work list: paired BTE/BR2 runs
with `--decision-log`, rebuild the dataset from pre-route rows
(`scripts/router_decision_log_dataset.py`), then replay the exported scale file
through the engine (`--back-to-explore-policy-scales-jsonl ...`) on held-out VERIFY
or sealed markets. Pass bar: engine-replayed routed NET > best fixed strategy on the
same held-out set with max DD no worse, on at least two disjoint windows.

## 3. Regime gates skip_calm + skip_expanded_mixed

**Claims audited.** Handoff §5: gated combo +$3,682 vs baseline +$3,059 on Jun
14-19; deployed to prod Jun 19.

**Evidence.** 6 days of live shadow tape, at-touch Python scoring (no fees, no
latency). Selected and evaluated on the same 6 days.

**Outcome.** OOS Jun 20-30 the gates blocked 99% of entries (10 entries in 10 days)
while the ungated stream was green every day (`docs/PROD.md` §2 history; retraction
banners on the handoff and on `docs/archive/2026-06/directional_satellite_candidates.md`). Removed
from prod 2026-07-01. June backfill (`docs/archive/2026-07/june-2026-backfill-results.md`) confirms
the ungated frozen config made +$17,689 fee-net over the same month.

**Verdict.** REJECTED-BY-EVIDENCE. Kept here because it is the cautionary template:
6-day in-sample selection, at-touch scoring, immediate deploy. Any future regime gate
must show all-window harness evidence plus a paper soak before prod (PROD.md
governance now requires exactly this).

## 4. Directional satellite candidates (`docs/archive/2026-06/directional_satellite_candidates.md`)

**Claims audited.** Four candidate designs (flow-following, basis+OI momentum,
multi-horizon alignment continuation, reversal-pressure late/tail hybrid), pitched as
complements on regimes where the fade "bleeds or is gated".

**Evidence type.** None. The doc is a design/backlog artifact from 3 research
subagents. No cell of the four candidates has ever been run through any harness. The
doc carries a **PREMISE RETRACTED 2026-07-01** banner: the regime gates it assumed
were removed as overfit, and the June backfill shows the ungated fade was green every
June trading day, so the "fade bleeds on these regimes" motivation is itself
unsupported on current evidence.

**Selection hygiene.** N/A, but the priors are adverse: the nearest tested relatives
are all rejected. F2/C2 aligned continuation: VERIFY -$1,025; ledger F1 aligned e30
gross-negative in all windows (W2 -$14,957); aligned hold REJECT (Sharpe 3.7-9.8,
day-corr +0.6 with the fade, fails complementarity); momentum overlay REJECT FINAL
(-9.8% even on trend tape). Candidate 4 leans on the BR2 cluster table (#2,
unvalidated). Candidate 2's basis signal exists as a validated sizing tilt
(F3, ADOPT-CANDIDATE weak) but has never been tested as an entry signal.

**Verdict.** UNVALIDATED (all four). Also note the motivation gap: before building
any of them, the June backfill regime-P&L re-grounding the banner demands has to
show a regime where the ungated fade actually loses persistently. The one prior
attempt to find gateable adverse regimes concluded they are not forecastable
(memory: `fade-adverse-window-not-gateable`; ledger 2026-06-13 10:05 shows
|corr| < 0.1 for every regime-timing signal tested).

**Proper validation requires.** Per candidate: implement behind the alpha harness
(`--aligned-mode` / `--dir-model` paths exist), screen on TUNE
(`WINDOWS=TUNE ./scripts/strategy_hunt_matrix.sh` frame, t-stat > 2 on fee-adjusted
edge), freeze, confirm on VERIFY at `--fee-curve-rate 0.07 --latency-ms 250`. Pass
bar: standard adoption gates plus day-correlation with the fade < +0.3 (the
complementarity claim is the whole point; F1-aligned failed at +0.6).

## 5. `clean_directional_pressure` (`crates/pm-alpha/src/directional.rs:132`)

**Claims audited.** None made, to its credit. The doc comment states: "UNVALIDATED
research ... component weights are hand-picked, not fit; do not use in any deployed
config until it prints positive on VERIFY". Weights are literal constants
(`basis*6.0 + flow*1.8 + oi*28.0 + funding*0.5`, clamped to ±0.35).

**Evidence type.** Unit tests of mechanics only. No backtest of the signal's P&L
anywhere in docs or `data/runs` references. The trained sibling `DirModel`
(logistic head fit by `scripts/dir_train.py`) likewise has no documented
TUNE/VERIFY P&L evidence, only training existence.

**Verdict.** UNVALIDATED (self-declared, correctly). Risk to watch: the function is
exported from a production crate, so a future config could consume it without
tripping any gate. Keep the comment's prohibition binding.

**Proper validation requires.** Ablation inside the harness on TUNE (pressure as
entry filter and as sizing tilt, vs baseline), then frozen VERIFY confirm. Pass bar:
adds NET or Sharpe on both TUNE and VERIFY without reducing the other; if used
directionally, hit > 0.50 on its own trades.

## 6. Whale split/redeem roller (`scripts/ops/shadow_roller_logger.py`)

**Claims audited.** Implicit only: the tool mirrors wallet 0x4d64518a's
split/dump/redeem cadence ("log-only soak for split/dump/redeem timing ... parity
checks against live wallet activity"). `whale_split_redeem_analyze.py` (now in
`scripts/archive/2026-07-cleanup/`) profiles rolling-capital wallets from activity
JSONL.

**Evidence type.** Observation of someone else's flow plus a would-have-done event
logger. There is no P&L model, no backtest, no doc quantifying what the roller earns,
what our version would earn, or why the edge would survive our execution. No
`docs/research/whale_*.md` covers 0x4d64518a. The adjacent mechanics research
(`docs/research/polymarket-mechanics-opportunities.md`) is actively discouraging:
split/merge/redeem is free but "was never the binding cost on any closed item";
sub-$1.00 pair arb is dead (two taker fees vs ~1.2c spread, maker-only per
whale_2855); split-then-maker-sell is DEAD (adverse selection, measured -$304 vs
+$1,281 taker control); merge-first capital recycling is PARK at ~$1-3/day of
recoverable value at current bankroll.

**Selection hygiene.** N/A; nothing has been selected because nothing has been
measured. The dump leg (selling the loser at <= penny bids before close) is the only
potentially novel economics, and it is exactly the piece with zero measurement.

**Verdict.** UNVALIDATED. Currently a data-collection tool, not a strategy. The
archive of the analyzer script suggests it is already deprioritized; the docs should
say so explicitly rather than leaving "roller" in ops as an implied candidate.

**Proper validation requires.** First a wallet-economics study (does 0x4d64518a
actually net positive after gas/fees, from `whale_pull.py` data), then an offline
tape replay of the dump leg (loser best_bid >= penny_max frequency and depth vs the
~1.1% fee-inclusive breakeven, same method as F11), before any harness work. Pass
bar: measured fee-net positive expectancy with capacity > $50/window; otherwise
close it like F11.

## 7. Tail convexity sleeve (Class C)

**Claims audited.** 07-forward-plan Class C: "PROMISING, highest new-signal priority
after Class A HOLDOUT", citing VERIFY P3 fade+tail `+$19,585` as "+65% vs fade-alone
narrative" and whale 8d1d's +0.126 edge/$1 in the 0.0-0.1 bucket.

**Evidence and why the headline claim is unsound.** Three direct tests exist and all
are negative or null:

- **F11 deep tail** (`docs/research/autoloop/f11_deep_tail.md`): the <= 0.05 and
  even <= 0.20 underdog ask level **does not exist** in BTC-5m/15m W3 books. Global
  min yes_ask across all 4,596 tapes is 0.26 (15m: 0.43). Zero fills possible, taker
  or maker. Verified three independent ways.
- **F10(a) lane tail hedge** (ledger 2026-06-13 04:20): REJECT FINAL across
  W1/W2/W3; 1c tails never fill, 3c tails worsen the worst day in every window.
- **BR2 `br2_convex_tail`**: net **-$93.83** over the full 23,705-market history
  (11/174 tail wins); the cov75 broader-coverage variant lost more (-$130.06) and
  did not improve drawdown.

Against that, the P3 artifact (`VERIFY_P3_fade_tail_btc5m.json`, +$19,585) is
recorded at exactly the same figure as the timed champion
(`champion_f1.json`, +$19,585, same theta/clip/exit). Given F11's finding that no
sub-0.10 ask ever exists in-window on this tape, the tail sleeve most plausibly
filled nothing and P3 equals its own fade baseline; the "+65% vs fade alone" line
compares against the differently-configured +$11,841 F1 matrix cell. The 07 doc
itself flags "isolate standalone tail entries next", which was never done. The
B_tail bucket inside the ungated fade (+$6,067 on 89 trades) is a property of the
fade's own entries, not evidence for a separate sleeve.

**Verdict.** REJECTED-BY-EVIDENCE for BTC-5m/15m expressions (both hedge-at-entry
and standalone cheap tail). The 07 doc's "PROMISING" rating and PR7 should be
downgraded or annotated. One residual open lead: **ETH-4h deep tail** (ledger
2026-06-13 09:05: asks reach 0.001, 180k ticks <= 0.05 persisting to final 300s,
~6 markets/day capacity), which is UNVALIDATED and tiny.

**Proper validation requires** (ETH-4h lead only). Offline flip-rate vs fee-inclusive
breakeven study on ETH-4h tapes (F11 methodology), then if positive a TUNE screen at
`--fee-curve-rate 0.07 --latency-ms 250`. Pass bar: realized win rate of <= 0.05
entries exceeds priced rate + fees with t > 2, plus capacity accounting.

## 8. Late-favourite expiry lane (Class B, `docs/archive/2026-06/late-favourite-lane.md`)

**Claims audited.** Lane cells C / E090 "advance-worthy on margin (+1.2 to +1.6pp,
9-10/12 green days)"; 07 doc Class B "VERIFY result +$392 NET, 93.0% hit"; ledger
F9/F10; bonereaper favourite-play equivalence (BTC taker +$1,460 = "exactly our
LANE").

**Evidence type.** The best-evidenced satellite. Fee-net harness (fee-curve 0.07,
150ms) on all three windows for the sigma>=4 lane: W3 +$1,514 (+$126/day), W2
~+$77/day, W1 ~+$68/day, all per $50 clip (ledger F10(a) controls). Cross-asset
transfer tested and failed (SOL/XRP -1.0pp to +0.1pp vs breakeven; ETH -0.5 to
-1.0pp below breakeven, F9). Maker entry tested and rejected (adverse selection
inverts the edge). Burn-stop and tail-hedge risk shaping tested and rejected;
overnight align-min-mid 0.90 is an adopt-candidate on 12 days only.

**Selection hygiene.** Compromised in one important way: the lane doc labels May
7-18 the "tune window", but that IS the protocol's VERIFY window. Every lane
configuration choice (thr 0.02, 120s window, mid 0.85/0.90, sigma>=4, perp@0.5) was
made on May 7-18; the W1/W2 positives are confirmations of a W3-selected config, in
the right direction (older data confirming newer selection) but the config has never
been confirmed on any window that postdates its selection. Also note the caveat the
main matrix table is pre-fee (only the maker-study control and ledger F9/F10 rows
are fee-true), and everything is at 150ms, which flatters a final-120s taker
strategy more than most.

**Known failures.** Binary -$50 full-clip losses (one loss erases 11-20 wins); May
15 crossed-mid tail day is the drawdown day in every cell; W3 walk-forward frame
worst day -$334 fails the -5% gate (07 doc Class B verdict); economics thin (best
~$126-162/day per $50 clip vs fade ~$1,500/day); BTC-only; ETH lane loses on
calibration; possibly redundant with hold@0.12 covering calm tape (ledger 06-13
06:15 strategic note).

**Verdict.** PARTIAL. The BTC favourite-underpricing edge is real across three
regimes fee-net (and independently corroborated by the bonereaper favourite-play
readout), but the specific config is in-sample on the VERIFY window, it fails the
worst-day adoption gate under the walk-forward frame, and no HOLDOUT, port, or live
record exists. "Parked pending live infra" is the honest current status; it should
not be marketed as validated calm-day floor.

**Proper validation requires.** Freeze cell C (or E090) exactly as documented; run
the sealed July window once at `--fee-curve-rate 0.07 --latency-ms 250` (the config
has already consumed TUNE-era windows as confirmation, so July is the only clean
look): `pm-app alpha --aligned-mode --align-min-mid 0.85 --enter-within-close-s 120
--exit-after-s 0 --fee-curve-rate 0.07 --latency-ms 250 --min-entry-sigma-bps 4`.
Pass bar: hit minus fee-adjusted breakeven >= +1.0pp, worst day > -$50 per $1K
frame, >= 60% green days. If it passes, PR8-style port with explicit worst-day
shaping before any capital.

## 9. F6 aggressor-flow fade (`docs/research/autoloop/f6_aggressor_flow.md`)

**Claims audited.** The calm-regime complement hypothesis: book moves without spot
moves revert and are fadeable.

**Evidence type.** Clean offline study: 4,451 events, W2 (n=3,344 flow) + W3
(n=425), optimistic simplifications (mid marks, no depth, no latency), still
decisively negative: -2.14c/share at 30s, t = -9.8, win 0.42; gross edge statistically
zero, so maker execution cannot rescue it. The classifier premise is inverted
(info-class moves revert MORE than flow-class).

**Selection hygiene.** Good; measured, not fit, and the negative is robust across
horizons, windows, and vol regimes.

**Verdict.** REJECTED-BY-EVIDENCE (closed 2026-06-12). Two hygiene notes: (a) any
plan or memory listing "F6" in a calm-day portfolio floor contradicts this doc and
should be corrected; (b) naming collision: strategy-hunt protocol "F6" is a regime
gate family, autoloop "F6" is this dead aggressor-flow fade. Disambiguate in future
docs. Reopening requires actual trade prints with aggressor flags, and the burden of
proof is overcoming a measured zero gross edge.

## 10. Class routing C10 (regime-routed Class A/B/C allocation)

**Claims audited.** 07 doc §2 routing table (expanded_mixed -> Class A full,
clean_directional -> Class B satellite only, alts -> skip), labeled "future C10, not
yet validated".

**Evidence.** The table is a reading of F1 VERIFY regime attribution, not a routing
backtest. Nothing run. The two adjacent facts cut against it: dynamic regime timing
of the fade is not forecastable (ledger 06-13 10:05: all |corr| < 0.1), and the one
deployed regime-routing attempt (#3) failed OOS at 99% blockage.

**Verdict.** UNVALIDATED. Any routing claim needs the #2 decision-log dataset path
plus engine replay before it is anything more than a hypothesis.

## 11. Multibook satellites (BTC-15m, ETH-5m/15m, XRP-5m)

**Claims audited.** Ledger multibook final: "BTC-5m core + BTC-15m additive +
ETH-5m confirmed (2/3 regimes) + ETH-15m (W3) + XRP-5m marginal sprinkle".

**Evidence type.** Fee-net harness (0.07, 150ms) but uneven coverage: BTC-15m hold
W3 +$8,428 full window, W1/W2 only +$2.7-4.4k on 8-day samples; ETH-5m W3 +$4,487
and W2 (19d partial) +$9,729 but W1 trend -$360 on 4 days only (open); ETH-15m W3
only; XRP-5m +$1,684 (t=1.89) on one 12-day window, high-flip-concentrated. SOL and
all-4h are structurally closed (book out-forecasts the model; the correct kill).

**Selection hygiene.** Moderate. These are the same config as the fade (frozen
elsewhere), so per-cell parameter overfit is low; but window coverage is sampled and
the "confirmed" language outruns it (the 09:45 ledger entry itself corrects the
09:40 "promoted/confirmed" claim). No cell has a HOLDOUT/sealed look, a
pm-strategy port, or a live/shadow record; all numbers predate the 250ms
recalibration.

**Verdict.** PARTIAL. Best-supported expansion inventory in the satellite set, and
correctly gated in 07 (PR10: multi-market only after decision-parity PR6 passes).
Not deployable evidence yet.

**Proper validation requires.** Full (not sampled) W1/W2 backfills for BTC-15m and
ETH-5m at `--fee-curve-rate 0.07 --latency-ms 250` with the frozen fade config, then
one sealed July confirmation per cell. Pass bar: standard adoption gates per cell,
plus ETH-5m specifically must resolve the trend-regime question with a full W1.

## 12. Both-sides-hold sell-loser idea

**Claims audited.** None in-repo; exists only as a queued idea in project memory
(when the fade holds both sides of a whipsaw market at combined cost < $1, test
selling the re-reversed losing leg vs redeeming at $0).

**Evidence.** No code, no doc, no run found in the repo (searched docs/ and
crates/). The nearest measured relatives are mildly encouraging (flip-side clip-2 is
the most profitable clip type, +$15.3/clip W3; passive exit mechanics exist in the
harness) but nothing tests the sell-loser leg itself.

**Verdict.** UNVALIDATED (idea stage). Cheap to test offline from existing
baseline.trades.jsonl before any harness feature: identify both-sides-held markets,
price the losing leg's best bid over the residual window, compare recovered value vs
fees. Pass bar for promotion to a harness feature: recovered value > fees on TUNE
with t > 2.

---

## Cross-cutting contradictions found

1. **VERIFY-window contamination in the lane doc.** `docs/archive/2026-06/late-favourite-lane.md`
   calls May 7-18 "tune window ONLY" while `00-protocol.md`/`07` designate those
   dates as VERIFY. The lane (and the whole F9/F10 series) spent the fade's
   confirmation window on selection. Not fatal for the fade (different strategy) but
   it means Class B has no untouched pre-July window left.
2. **Class C headline vs its own artifacts.** `04-first-principles.md` C4
   "+$19,585, +65% vs fade alone, PROMISING" coexists with F11 (no sub-0.26 ask ever
   exists on the same tape), F10(a) REJECT FINAL, and BR2 convex_tail net-negative.
   The P3 aggregate equals `champion_f1.json` to the dollar; the uplift attribution
   was never isolated and is almost certainly a baseline mismatch.
3. **Handoff §6 vs walk-forward doc.** The Jun 19 handoff sells BR2 as the
   directional satellite using cluster PnL from the in-sample full-history run,
   while `02-walkforward.md` (same repo, three days earlier) records BR2 as REJECT
   on VERIFY at $1K. The handoff quotes "W3 WEAK +$327" but omits the gate verdict.
4. **Router doc internal tension.** "Current Evidence" presents BR2 cluster strength
   from full-history fills; the May section then shows BR2-only at -$18.81 and
   `expanded_high_flip` flipping sign. The Routing Hypothesis section partly
   acknowledges this; the directional-satellite doc cites only the favourable half.
5. **Retraction propagation (resolved 2026-08-23).** The 2026-07-01 retraction
   banners (satellite doc, handoff) were correct but the downstream artifacts
   they motivated (`configs/exo_fade_chop_router.toml`, `configs/mayjune_btc5m.toml`
   regime flags, `scripts/score_regime_gate_sweep.py`) persisted without banners
   until the 2026-08-23 framework reset deleted the entire `configs/` directory
   (inert under the profile-removal) and the regime-gate sweep script; the
   unreconciled mayjune gate-polarity question is now moot.
6. **F6 naming collision** (protocol regime-gate F6 vs autoloop aggressor-flow F6),
   plus any "calm-day floor" narrative that still lists F6 or a whale mirror as
   components: the repo evidence says F6 is dead and the roller is unmeasured.

## Bottom line

The satellite book as currently documented contains zero validated strategies. Two
candidates have partial, real evidence (late-favourite lane; BTC-15m/ETH-5m
multibook) and a defined path to a clean verdict via the sealed July window at
canonical accounting. Four are rejected by the repo's own measurements (BR2 lane at
$1K gates, Class C on BTC short-horizon, F6, regime gates). Everything else is
design-stage. Until a satellite passes TUNE-select/sealed-confirm at
`--fee-curve-rate 0.07 --latency-ms 250` plus the adoption gates, portfolio docs
should describe the book as fade-only.
