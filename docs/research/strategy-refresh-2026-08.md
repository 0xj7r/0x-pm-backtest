# Strategy refresh, 2026-08-23

Synthesis of (a) the repo's full internal verdict record, (b) external web research
(grok, 2026-08-23; full transcripts in the session scratchpad: grok_wallets.md,
grok_market_changes.md, grok_strategies.md), and (c) the August 2026 platform
changes. This is the candidate map for the first research cycle on the rebuilt
framework. Written the day reset phase 1 completed.

## 1. The platform changed under us (August 2026)

Confirmed against docs.polymarket.com/changelog/predictions:

- **TWAP settlement.** Since 2026-08-07, 5m/15m/4h crypto up-down markets resolve
  on a Chainlink TWAP, not a single price snapshot (5m window 30s, widened to 60s
  on 2026-08-14; 15m/4h at 60s). Hourly markets still resolve on Binance candles.
- **Why.** A Stanford/SMU study (arXiv:2606.31675) documented last-10s Binance
  settlement pushes: ~$8.2M extracted over Feb-Apr, 821 wallets, near-certain
  favorites flipped ~34% of the time in pushed cycles vs ~1% otherwise. The
  signature was largely absent on 15m.
- **Taker delay cut 250ms to 50ms** on 2026-08-17 (crypto/finance up-down).
- **Maker subsidy.** $1M/month liquidity-rewards pool on the TWAP crypto markets
  (August: $300k on BTC-5m alone) on top of the unchanged 20% maker rebate.
  Crypto taker fee coefficient unchanged at 0.07.
- **API break.** Since 2026-07-24, POST /order(s) returns tradeIDs, not
  transactionHashes (breaks the agent's old fill parsing). Data-API redeem rows
  are now per outcome, positions carry entry-fee fields (2026-08-10).
- BTC-5m remains liquid: ~$13.5M daily volume, ~$12.7k book per window (Gamma,
  2026-08-23).

**Consequences for our record:**

1. Every pre-Aug-7 5m backtest models a settlement rule that no longer exists.
2. Worse: the manipulation flips paid CHEAP UNDERDOG holders, which is exactly
   the exo_fade's payoff tail. Part of the fade's Feb-Jun validated P&L was
   plausibly free-riding a manipulation flow that TWAP has now removed. The June
   whipsaw pain is also partly explained (retail was being farmed).
3. The 15m record is cleaner: the manipulation signature was largely absent
   there, so pre-Aug 15m results carry more forward-validity than 5m results.
4. The latency-truth tables (750/1250ms pricing) included the 250ms venue taker
   delay era; the chain is now ~200ms faster. Latency economics need re-running.
5. Data priority inverts: post-2026-08-14 tape (the settled 60s-TWAP regime) is
   the scarce, decision-relevant window. We hold zero data after 2026-07-10.

## 2. Internal verdict map (from the full doc sweep)

**Validated (pre-TWAP evidence, now needs TWAP-era reconfirmation):** the
exo_fade core (positive all five months at truthful latency), perp-led belief
(0.75), exit-30s convergence capture (sealed-holdout pass), entry-deadline gate,
rearm/max-clips re-entry, binance-proxy strike basis, BTC-15m satellite (June
OOS +$4,586 at 250ms / +$1,889 truthful, day-correlation to 5m only +0.23),
hold-to-redemption accounting equivalence, ungated-across-regimes.

**Falsified (do not re-run):** regime gates (all forms), dwell gate,
min_entry_ask, pair-lock hedge, cut-loser, max-pair-cost, maker ENTRY, paired MM
as previously built, back_to_explore, bonereaper late-favourite lanes (three
strikes), momentum overlays, cross-asset drift, ETH/SOL/XRP fade as-is,
calibrator v1 and continuation models, Kelly sizing, naive certainty sweep at
0.97-0.99, F6 flow-noise fade, sub-15s sniper copies, tail-convexity hedge on
BTC-5m/15m (unfillable below 0.26), sub-$1 crossed-pair arb, dual-surface exit,
tie-rule bias, rebate-tier engineering.

**Untested and still standing:** ETH-4h tail convexity (asks genuinely reach
0.001 there), C8 cross-asset lag, wallet-derived hypotheses (ce25 taker band,
late-window entry timing, laddered accumulation), sell-loser on both-sides
holds, fast/event-driven decide, consensus execution, flow-imbalance toxicity
features, TZOdds-style two-way scalper, past-close resolution marking, the two
stranded July verdicts (cheap-underdog realization; v1 gate) possibly
recoverable from the S3 shadow archive.

## 3. External evidence, ranked (grok survey, 2026-08-23)

What shows real profits publicly, by evidence quality:

| Strategy | Evidence | Alive post-TWAP? |
|---|---|---|
| Settlement push | Academic, $8.2M/2mo | Dead by design |
| Maker vs taker class transfer (15m) | Telonex week study: makers +$728k pre-fee, takers -$728k | Yes; structure unchanged |
| Late-window near-certainty sweep | BTC5MScour wallet: +$26.5k/26d, +1.8% ROI, buys 0.96-0.99 last 60-120s | Weakened by TWAP |
| Two-sided quoting + late directional load | SLIP-ME wallet: +$112k/23d, but LOSES the paired leg; profit is the last-90s load | Partially |
| Pair MM with tilt | FlippingSharks: +0.30% ROI, week-2 decay, weekend bleed | Yes, thin |
| Binance-to-CLOB latency snipe | OffGrid: 29 wallets >98% WR week 1, NEGATIVE week 2 | Mostly dead |
| Exhaustion fade / underdog / BS-IV blogs | Marketing, no audited P&L | Unproven |

Cross-cutting external facts worth keeping: the 5m book is calibrated but not
sharp (Gruener, SSRN 6863546: a second-feed side check called 81% vs the
market's 64%, and the gap is information lag, not longshot bias); naive 94c
favorite scalps are breakeven-to-negative after fees; Poly-vs-Kalshi oracle
disagreement is ~6%; Binance-vs-Chainlink side disagreement ~15% on small moves.

## 4. Candidate slate for the next cycle (ranked)

Standing posture (owner directive, 2026-08-23): all previous strategies are
dead until they re-earn trust. Nothing here is a deployment candidate; every
line is a hypothesis that enters through the rebuilt framework's gates
(truthful latency, fee-net, jittered replay, multi-window validation,
TWAP-aware settlement) from zero. The fade's June prod losses decomposed mostly
into bugs/sizing/architecture rather than the belief model, but its backtest
tail is now also suspect on independent grounds (section 1, manipulation flow),
so it gets no benefit of the doubt.

1. **TWAP-aware exo_fade revalidation (the core question).** Reprice the belief
   as P(60s TWAP >= K), a Brownian-bridge average, not P(S_T >= K); re-run the
   fade on post-Aug-14 tape as its own regime. Expect the underdog tail to pay
   less than the Feb-Jun record suggests (manipulation flow gone) and late
   entries to matter differently (a TWAP is harder to flip late). The exit-30s
   variant may gain relative to hold-to-redemption for the same reason.
2. **BTC-15m fade satellite, promoted.** Cleanest forward-valid record
   (manipulation-free pre-TWAP, 60s TWAP window unchanged since Aug 7,
   latency-robust, low correlation to 5m). Arguably now the LEAD candidate for
   first live re-arm rather than the 5m core.
3. **Maker/quoting with subsidies modeled.** The $300k/mo BTC-5m rewards pool
   plus 20% rebate plus the queue-model calibration we already built (fill rate
   1.6% of posted, pro-rata sims 14x optimistic) makes this worth one honest
   backtest. Known killers to model: adverse selection, exit-fills-only-on-
   winners, TWAP-lengthened end-window inventory risk, 1.5c qualifying spread.
4. **Latency re-pricing + fast engine.** With the venue delay at 50ms and the
   ~500ms decision-timer phase fixable (fast engine already built), the taker
   chain drops toward ~400-700ms. Re-run the latency-truth table; edges priced
   dead at 1250ms may be alive at 700ms.
5. **Wallet-emulation research loop.** See section 5.
6. **Cheap experiments queue:** sell-loser on both-sides holds; score the
   stranded July soak evidence from s3://pm-research-data-prod/shadow/pm-alpha/
   dublin/ (cheap-underdog realization, v1 gate retro-verdict); ETH-4h tail
   convexity with the F11 methodology.

Explicitly NOT pursuing: latency-race sniping (conceded; TWAP + competition),
settlement-push-adjacent anything, regime-prediction gates (falsified class),
naive certainty sweeps.

## 5. Wallet-emulation workstream (method)

Tooling: ~/go/polymarket-research (wallet_profiler package; walks the Data API
per wallet, computes redeem-based realized P&L, market mix, clip distributions,
entry timing). Discovery sites, best-in-class per the survey: Polyanna
(polyanna.app; bot score, copyability, capital-efficiency boards), PolyIntel
(polyintel.io; Outcome-vs-Trading P&L split, style taxonomy, sybil clustering),
Wallet Master (walletmaster.tools; 180+ filters incl. profit concentration),
Shadower (Elo, bots excluded), OVERROUND (skill-vs-luck check; excludes 5m by
policy), Polynode copy-pnl endpoint (simulated copier P&L with slippage and a
toxic-for-copying flag). PR&R teardowns (polyresearchrobotics.com: BTC5MScour,
SLIP-ME, FlippingSharks) are free reverse-engineering to mine before doing our
own.

Selection screens that survived scrutiny: drop raw PnL rank; require 50-100+
resolved markets; Wilson-bounded win rate; the PolyLens rule WR minus average
entry price > 0; profit concentration top-3 markets under ~15%; bot/sybil
filters; copy-toxicity backtest with lag and 1-2% slippage.

Doctrine (from our own June lessons): observed fills are the shadow of a
strategy, not the strategy. Timings and clip sizes are artifacts of the
operator's latency, queue position, bankroll, and risk rules. A wallet yields a
HYPOTHESIS about a decision rule; the hypothesis is implemented as a strategy
module and validated in our engine under our latency, fees, and sizing exactly
like an original idea. Never copy fills.

## 6. Data implications

- Backfill priority: 2026-08-14 to present FIRST (the live TWAP regime), then
  Jul 11 to Aug 13 (transition), then nothing older is urgent (we hold Feb-Jul).
- Pre-Aug-7 5m analyses need a manipulation-regime flag; the Stanford paper's
  abnormal-close-flow method is reproducible from Binance aggTrades we hold.
- New feeds worth ingesting: the Chainlink TWAP streams (RTDS topics
  crypto_prices_twap_thirty/sixty) so belief and resolution share a basis; the
  strike-basis doctrine now applies to TWAP-vs-Binance, and needs re-measuring.
- polymarket-agent needs the tradeIDs response-shape fix before any re-arm.
