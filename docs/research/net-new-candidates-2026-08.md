# Net-new strategy candidates, 2026-08-24

Synthesis of four parallel research sweeps (TWAP-settlement mechanics, flow and
behavioral structure, binary market-making theory, structural/cross-instrument;
full transcripts in the 2026-08-24 session scratchpad: grok_twap_edges.md,
grok_flow_edges.md, grok_mm_design.md, grok_structural_edges.md). Everything
here is filtered against the falsified-ideas inventory in
strategy-refresh-2026-08.md; nothing below re-proposes a dead idea. All are
hypotheses for the rebuilt framework's gates, none are deployment candidates.

The one-line thesis across all four sweeps: after TWAP, the durable edges are
MODEL edges (pricing the average correctly when the book still prices a
snapshot) and SUBSIDIZED LIQUIDITY (the reward pool pays for presence), plus
knowing when NOT to quote. Speed is dead as an edge and merely adequate as a
requirement; ~700ms qualifies for everything below.

## Tier 1: model edges (framework-native, test first)

1. **Residual-strike engine (K-star).** Inside the final 60s the contract is an
   Asian digital: Up wins iff the remaining average clears
   K* = (W*K - e*Abar) / r, where Abar is the locked partial average. After 40s
   with Abar 8bps above K, K* is ~24bps below spot and remaining std ~0.6bps:
   decided, while snapshot-priced quotes still print ~50c. Fee curve collapses
   exactly as conviction rises, so taking is cheap when the model is surest.
   Counterparties: snapshot bots, retail marking Binance-last vs the published
   price-to-beat, reward-paid makers who do not skew. Decay: months for the
   crude version; second-order (tick-weighting, replica basis) slower.
   CRITICAL implementation trap: the RTDS rolling 60s TWAP topic mid-window
   averages PRE-window prices; the locked Abar must be rebuilt from the raw
   Chainlink stream over [window_open, now], and Chainlink does not publish
   sampling weights, so the replica needs a measured basis haircut.
   Data needed: Chainlink Data Streams capture (Plan 3 ingestion) + post-Aug-14
   tape. Lit anchor: Turnbull-Wakeman 1991, LME TAPO practice.

2. **TWAP-fade (the fade reborn on correct math).** A late spot wick now enters
   settlement with weight r/W and must persist as the start of the remaining
   path; a binary that reprices a 2s wick as a flip is overreacting by
   construction. Fade it with a jump-vs-flicker persistence flag. This is the
   cheap-underdog fade re-founded on the averaging law instead of manipulated
   snapshot flips; expect smaller but cleaner tails than Feb-Jun. Also
   front-loaded delta: a move in the first seconds of the window both locks
   average and shifts the state (near-decisive); a move with 2s left locks a
   sliver. Lit anchor: Evans 2018 pre/post-fix reversal, funding-TWAP practice.

3. **Pre-window 1/3 rule.** Before the averaging window opens, effective
   variance is sigma^2*((t_rem - w) + w/3), ~7% less vol than snapshot pricing
   on a 5m market: favorites are MORE favorite, dogs MORE dead. Worth 1-2c ATM,
   which is under the taker fee: this is a MAKER edge (quote at bridge-fair,
   let snapshot-priced flow cross). Our twap_digital already computes it.
   Decays fast among pros; persists vs retail.

4. **Nested remaining-window term structure.** 5m, 15m, 1h, 4h windows overlap
   on one underlying. At open all ATM digitals are ~50c and there is no trade;
   MID-WINDOW, after nested child windows resolve, the parent is a remaining
   digital on a known strike and the child is ATM on a new one: invert both
   books against the same-oracle remaining-window CDF and trade the richer
   mispricing. Nobody has published this; our Feb-Jul tape can test the
   snapshot-era version tomorrow. Special case: every third 5m close coincides
   with a 15m close (same settlement average A_T, different strikes): a digital
   call spread on A_T; trade the pair when one book has digested Abar and the
   other has not, preferably by making on the stale side.

5. **Hourly as the Binance-true slow book.** 1h markets STILL settle on the
   Binance 1H candle via UMA (~2h challenge), unchanged by the TWAP migration.
   Diagnostic first: is the 1h CLOB, 5 minutes before close, priced like a
   Binance remaining-digital or like the Chainlink 5m odds? If the latter, a
   slow-book model edge exists at capacity ~$0.75M/day with 700ms being
   irrelevant. Also measure hour-boundary oracle same-side rates before anyone
   dreams of a box trade (path identity is not oracle identity; naive 1h-vs-
   nested-5m boxes fail on strike mismatch).

## Tier 2: the maker program (one build, many pieces)

6. **TWAP-aware inventory-managed maker.** The literature seat exists now:
   Feil-Nendel (arXiv:2607.17991, Jul 2026) solves MM with binary settlement;
   their headline is that the optimal quoter gives up ~0.6% mean P&L for a 3x
   variance cut and 4x smaller terminal inventory. Composite design, each part
   sourced: reservation price = twap_digital (never the CLOB mid), logit-space
   quoting, terminal penalty gamma_T*q^2*p*(1-p) that STRENGTHENS into the
   window, GLT hard inventory gates, spot-toxicity alpha (OFI/VPIN computed on
   Binance/HL, used only to pull quotes, never as direction), Guilbaud-Pham
   taker unwind (flatten by taking once |Abar - K| > 2 sigma_rem or too little
   time to round-trip at our measured 1.6% fill rate), and reward-formula
   awareness: score is quadratic in tightness inside a 1.5c band, 3:1
   two-sided balance rule, tails demand token two-sided size. Forfeit reward
   score rather than hold ATM inventory through the pin: last-60s ATM is a
   forbidden state (0DTE desk practice; ESMA binary-book postmortems).
   Public tape so far is reward-positive fill-negative at retail scale; our
   queue calibration (fills = 1.6% of posted, pro-rata sims 14x optimistic) is
   the differentiator nobody public has.
   Realistic prize: BTC-5m pool is $300k/mo but split across ~288 markets/day
   (~$34/market/day) and all farmers; plus 20% maker rebate; plus selective
   fills. A calm-regime floor, not a fortune.

7. **Quote masks (adjunct to 6, cheapest experiments in the set).** No-quote or
   3x-widen windows around 08:30/14:00 ET scheduled prints and CoinGlass/HL
   liquidation bursts; size-down weekends and the 04:00/20:00 UTC pockets (the
   only public 5m MM book with clean session data bleeds exactly there). Score
   adverse-selection bps saved vs reward score lost.

## Tier 3: measurements that gate the above

8. **Post-TWAP flow tape.** Maker/taker x price-band x seconds-to-close x
   session histogram on 5m/15m since Aug 14: confirms whether the U-shaped
   late-extreme taker flow (the maker's food) survived TWAP. Grounds 6 and 7.
9. **Taker-rebate incidence.** wV = size x (1-p) x 2.3 pays cheap-side takers
   ~2x per dollar: our fade entries EARN outsized rebates. Model taker rebates
   in the engine's accounting (they are real cash, paid daily); measure whether
   rebate-farming flow crowds the cheap side post-May-28.
10. **Chainlink replica basis.** Build the Abar replica from raw streams,
    compare to official prints over a week, size the haircut for candidate 1.
11. **Redemption/ops pipeline.** Auto-redeem via relayer (25 req/min cap),
    sell-at-99c vs redeem-at-1.00 accounting, 1h UMA 2h capital lock. Velocity
    and leak-prevention, not alpha; a prerequisite for any live 5m loop.

## Explicitly not pursuing (and why, one line each)

- Last-tick or sub-second anything: TWAP made the race pay impact for 60s.
- Arithmetic-vs-geometric Asian corrections: O(0.005bp) at this vol and window.
- VPIN/OFI as direction: decayed to negative net on perps; use only as a pull
  signal.
- Deribit-IV fair value at minutes horizon: wrong instrument, re-confirmed.
- 94-99c favorite scalps, naive both-sides MM, regime gates: falsified in-house.
- Settlement pushing: dead by design, and was never our trade.

## Engine/backlog implications (feeds Plan 2/3 tickets)

- twap_digital's locked input must come from a raw-stream replica, never the
  RTDS rolling topic (doc note + loader design).
- Add reward/rebate accounting to the engine: taker-rebate wV cashflows,
  maker rebate, and a simulator of the public liquidity-reward score formula
  (quadratic tightness, 3:1 rule, per-minute sampling).
- Data ingestion (Plan 3): Chainlink Data Streams capture becomes a
  first-class feed next to Binance spot/perp.
- The strike-basis re-measurement doctrine now includes TWAP-vs-Binance basis
  and the replica haircut.
