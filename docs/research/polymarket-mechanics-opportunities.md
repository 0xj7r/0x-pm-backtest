# Polymarket Mechanics: Opportunity Review (June 2026)

Scope: a full documentation sweep of Polymarket mechanics (CTF, websockets, maker programs, combos/RFQ, builder codes, API trading, data/resolution) generated a candidate opportunity list. Every candidate was then put through two independent adversarial reviews: an economics skeptic (does the money survive contact with our measured live data?) and an evidence skeptic (do the cited docs actually say this, and do our markets qualify?). A candidate is dead only if both skeptics refuted it, but verdicts below also weigh how decisive a single refutation was. Several candidates were proposed multiple times by different agents; duplicates are merged.

The headline result is negative in a useful way: none of the flashy structural ideas survived (dual-surface exits, synthetic maker entries via split, sub-$1.00 pair arb, tie-rule bias). What survived is operational: a kill-switch and latency bundle, two accounting reconciliations of money already owed to us, one measurement diagnostic that may explain part of the live-vs-backtest capture gap, and a free settlement-truth data feed worth archiving.

## 1. Executive summary

- The Yes and No books are one unified book at the matching engine (MINT/COMPLEMENTARY/MERGE crossing). This single fact killed four separately-proposed "dual-surface" ideas: a resting No bid at (1-p) is the same economic order as a Yes ask at p, facing identical taker flow. There is no second fill surface on these markets, ever. Treat this as settled.
- Free, gasless split/merge/redeem is real and documented, but it was never the binding cost on any closed item. It changes capital-recycling speed, not breakevens. The pair-arb breakeven still includes two taker fees (~3.5c at mid prices); our own prior measurement (crossings 0.026% of the time, <1s, sub-$1 depth) stands.
- Maker-entry constructions (split then maker-sell the unwanted side) are a re-skin of the mint-then-sell variant we already tested and rejected, and of the maker-entry study on the late-favourite lane that measured net -$304 vs +$1,281 taker control with a ~4pp conditional hit-rate penalty. Adverse selection, not fees, is the binding constraint, and nothing in the docs changes it.
- Two rebate programs are paying us (or about to): the taker-rebate tier ladder (live 2026-05-28, crypto wV weight 2.3x, we are likely Silver 8%, possibly borderline Gold 18%) and the maker rebate pool (20% of taker fees, ~$0.35 back per $50 passive-exit clip at mid prices). Both are auto-enrolled. Neither justifies changing trading behavior; both justify a reconciliation script and a fee-model update.
- The 250ms itode taker delay is confirmed live on BTC 5m. Our FAK limits are already tight (at-touch, edge-capped), so the proposed "fix" was already deployed; but the diagnostic (markout of fills vs Binance move in the 250ms post-send window) is the cheapest remaining attack on the 0.45 capture ratio and should be run.
- The resolving Chainlink feed is free on RTDS (crypto_prices_chainlink, no auth, ~1Hz, 18-decimal benchmark prices). It is not an alpha source (winner direction is basis-invariant, 8,308/8,308 measured) but it is the settlement SSOT, and the official history API only serves ~30 days. Archive it; apply for the free sponsored Chainlink Data Streams key.
- Two genuinely new mechanics are watch-items, not trades: RFQ/combos (last-look quoting would change maker economics, but crypto up/down is not eligible today and our region access is unverified) and liquidity-reward funding (configured but unfunded on our books).

Expected incremental value of everything below, honestly: roughly $10-50/day at current scale, mostly from fee-model corrections, rebate accounting, and capture-ratio diagnostics, plus tail-risk insurance from the heartbeat kill-switch that is worth more than its daily P&L on one bad night. At 5x scale the rebate lines grow to ~$60-150/day (Gold tier sustains). No new edge of the magnitude of the existing fade or late-favourite sleeves was found.

## 2. Ranked opportunity table

| # | Opportunity | Verdict | $/day now | $/day at 5x | Build cost | Strongest skeptic objection | Does it hold? |
|---|---|---|---|---|---|---|---|
| 1 | Order-lifecycle ops bundle: heartbeat dead-man switch, batched ladders, push fills, persistent WS + new_market | BUILD NOW | $0-30 + tail insurance | $0-30 + insurance | 1-2 days | Capture gap is depth-bound, not our-latency-bound; new_market adds no fade P&L (second-zero books have no stale quotes) | Partly. Treat capture gains as upside; heartbeat and push fills justify the build alone. Add order-state reconciliation on heartbeat failures (a network blip cancels the passive exit silently). |
| 2 | Taker-rebate tier audit + fee-model fold-in | TEST CHEAP | $8-45 (already accruing) | $60-150 (Gold sustains) | Hours | Money accrues whether or not we audit; routing/boundary-management legs are void (wV per fee dollar is a constant 2.3/0.07) | Holds against the routing leg, not the audit. Both skeptics passed the audit version. Never churn for tier. |
| 3 | Maker-rebate capture accounting on passive exits | TEST CHEAP | $2-20 | $10-50 | Hours | Scale: a rounding adjustment to the core edge, not an edge | Holds, but both skeptics passed it: ~20% of own-fill fee-equivalent is near-deterministic, validation is free, and it tilts the passive-vs-taker exit comparison correctly. |
| 4 | itode 250ms markout diagnostic + backtest signal-horizon shift | TEST CHEAP | $0 direct (information) | n/a | Hours | The proposed FAK-limit fix already exists in live code (bonereaper_mm.rs caps at min(ask, kelly_p_adj - min_edge)); limits cannot filter fair-value decay anyway | Holds against the fix, not the measurement. The 250ms shift is measurement hygiene; the markout study sizes the maker-cancel adverse-selection channel and routes findings to the maker-direction work. |
| 5 | Chainlink settlement-truth logger (RTDS crypto_prices_chainlink) + sponsored Data Streams key | TEST CHEAP | $0-10 | $0-10 | 0.5-1 day | Winner direction is basis-invariant (8,308/8,308 windows); the crossed-mid tail is a real price move, not a strike artifact; tie/no-tick states are measure-zero (1Hz, 18-decimal feed) | Holds against the alpha claims. The archiving value stands: official history only serves ~30 days, and a live basis estimate beats the static 14bps offset. Keep beliefs single-basis. |
| 6 | Eligibility watcher: RFQ/combo flags + liquidity-reward funding on our series | TEST CHEAP | $0 (option value) | $0 (option value) | ~1 hour | A rewards flip would re-price a structure that failed on fill rate (1.6%) and toxicity, not on subsidy; RFQ flow is adversely selected by construction and our region access is unverified | Mostly holds. Stripped to a daily cron + alert with re-validation (never trading) as the trigger semantics. Run the Dublin geo-test first; if blocked, delete the RFQ branch. |
| 7 | Complete-set hygiene: never book-exit against own complementary inventory; merge any full set | PARK (adopt as guard) | ~$0 | ~$0 | Hours | Free redemption is the counterfactual, so merging saves no fees; taker sleeves almost never hold complete sets | Holds. Keep as a correctness invariant (an if-statement in the position ledger) that becomes load-bearing if any maker direction ever goes live. |
| 8 | Merge-first capital recycling + WS-triggered batched redeem | PARK | $0-3 | maybe $10-20 if capital-bound | 1 day if justified | Average dead capital is ~$10-20 against $2.8k; recoverable opportunity cost ~$1-3/day; Chainlink automation latency dominates and is not ours to optimize | Holds. Re-measure (capital-starvation events, resolved-latency distribution) when bankroll nears $10k or concurrency rises. |
| 9 | Crossed-pair alert, fee-adjusted, log-only | PARK (1-hour tripwire at most) | ~$0 | ~$0 | 1-2 hours | Prior measurement: crossings 0.026% of time, <1s, sub-$1 depth, pre-fee; the operator's own engine collapses crossed books via minting | Holds. If logged at all, threshold must be asks_sum < 1 - 0.07*(p_y(1-p_y)+p_n(1-p_n)), same-snapshot only. Never build auto-execute. |
| 10 | Cross-horizon extension: 15m fade | (Already in flight) | per existing validation | n/a | n/a | Not new: BTC-15m passed the full validation protocol 2026-06-11 and is a shadow candidate | Holds. Proceed on the existing roadmap, not as a new item. |
| 11 | Cross-horizon extension: 1h/4h | DEAD (as proposed) | ~$10 ceiling | unknown | n/a | 1h/4h resolve on Binance BTC/USDT candle open/close, not Chainlink: different product, different strike basis; staleness edge scales 1/sigma*sqrt(T), collapsing the tradeable zone to final minutes of a thin book | Both skeptics refuted. One free byproduct: 1h strikes are on our own signal feed (no USD/USDT basis offset), so a final-minutes shadow log is a cheap optional experiment. |
| 12 | Dual-surface passive exit (rest Yes ask + No bid, merge on fill) - proposed 4x | DEAD | $0 | $0 | n/a | The two orders are the same order: one unified book, identical counterparty flow via MINT/COMPLEMENTARY/MERGE; incremental fills are level sweeps that fill both atomically, flipping us short at maximal adverse selection | Decisive. Refuted by the proposal's own doc basis in all four variants. Salvage: a one-day shadow log of whether per-surface queues are ever asymmetric (surface SELECTION of a single order, never duplication). |
| 13 | Synthetic maker entry via split (late-favourite and fade) - proposed 3x | DEAD | $0, likely negative | $0 | n/a | Identical to the mint-then-sell variant already tested and rejected; the live maker-entry study measured -$304 vs +$1,281 taker control; fills anti-select (would-fill subset loses, no-fill subset hits 99.6%); breakeven conversion ~85-90%, measured maker fill rate ~1.6% | Decisive. The no-fill/would-fill partition is itself a salvageable veto signal for the taker sleeve (one backtest day, separate item). |
| 14 | Sub-$1.00 crossed-pair free money (breakeven moved to exactly 1.0000) | DEAD | $0 | $0 | n/a | Arithmetic error: buy-both is two taker legs at 0.07*p*(1-p) each; breakeven is ~0.965 at mids, not 1.0000; gas/CTF fees were never the binding cost | Decisive (both skeptics, twice). Superseded by #9 in log-only, fee-adjusted form. |
| 15 | Tie-rule bias ("unchanged resolves Up" on calm windows) | DEAD | $0 | $0 | n/a | Measured on 8,308 windows already on disk: exact ties = 0, min nonzero gap $0.0087; the resolving benchmark is a continuous ~1Hz 18-decimal aggregate, not a deviation-gated tick; bias bounded ~1e-5 vs 175bp fee | Decisive. The >= rule is mathematically real and worth <1bp; hard-code as zero. |
| 16 | Taker-rebate tier engineering (routing aggression for wV, boundary churn) | DEAD | negative EV | negative EV | n/a | wV per fee dollar is constant (32.86x); churn costs ~2-3c per $1 wV vs second-order rebate value; threshold-chasing is taker-side reward farming and risks the inauthentic-trading clause | Decisive for the routing/churn legs. The audit leg survives as #2. |
| 17 | Builder-code self-attribution | DEAD | ~$0 | ~$0 | n/a | Only cash channel is the pro-rata weekly rewards pool (noise at our volume) and self-flow attribution risks failing rewards approval as self-dealing; no platform-fee rebate | Holds. One free byproduct kept in section 4: builder-tagged aggressor flow as a fill-quality signal, if visible in fills. |

## 3. Surviving opportunities

### 3.1 Order-lifecycle ops bundle (BUILD NOW)

Mechanism. Four documented mechanics, all verified against current docs, that harden the live taker edges and the just-validated passive rest-at-mid exit:

1. Heartbeat dead-man switch. Chained POST /heartbeats; if a valid heartbeat is not received within 10s (+5s buffer) the exchange cancels ALL open orders server-side. Stronger than `touch live.kill`, which needs the process alive. This is the precondition for running the passive exit overnight. Mandatory companion: order-state reconciliation on every heartbeat 400/expired-id and on reconnect, because a >10s network blip from Dublin cancels the resting exit while the process believes it is live, converting the fee-saving exit into hold-to-expiry on exactly the bad nights.
2. Batched ladders. POST /orders takes up to 15 signed orders, processed in parallel (no intra-batch ordering guarantee). Keep the time-critical first clip as a standalone POST /order; batch the rest plus the paired exit.
3. Push-based fills. The authenticated user channel (wss://ws-subscriptions-clob.polymarket.com/ws/user) pushes trade events at MATCHED with per-maker matched_amount, then MINED/CONFIRMED (or RETRYING/FAILED). Re-laddering and partial-fill handling react in milliseconds instead of a polling interval; FAILED settlement becomes a free kill-switch input.
4. Persistent market channel with custom_feature_enabled: true. Adds best_bid_ask (pre-computed top-of-book + spread), new_market (token IDs + fee_schedule pushed at listing, closing the Gamma polling gap at the 5m roll), and market_resolved (instant winning_asset_id for redemption). Roll markets via dynamic {operation: subscribe/unsubscribe} on one connection; never reconnect at the open.

Doc citations: docs.polymarket.com/trading/orders/create (heartbeat text verbatim), /api-reference/trade/post-multiple-orders (max 15, parallel), /market-data/websocket/user-channel, /api-reference/wss/market (dynamic subscribe, custom_feature_enabled).

Validation plan (each piece under an hour):
- Send chained heartbeats, kill the process, confirm server-side cancel-all fires within ~15s.
- Time a 5-order batch vs 5 sequential posts from Dublin.
- Diff user-channel MATCHED push timestamps against current fill-detection latency over one session.
- Log whether new_market arrives before the current Gamma poll at 10 consecutive 5m rolls; attribute zero dollars to it until this shows a gap.

Honest value: $0-30/day in capture-ratio nudges (the 0.45 deflator is likely depth/adverse-selection bound, so treat any gain as upside), plus tail insurance. Build: 1-2 days total; heartbeat and batching are hours each.

### 3.2 Rebate accounting bundle (TEST CHEAP, two reconciliations + one fee-model change)

Two separate programs, both auto-enrolled, both paid daily at midnight UTC in pUSD, both currently absent from our fee model.

Taker-rebate tiers (live since 2026-05-28). wV = Trade Size x (1 - Entry Price) x Category Weight x Bonuses; crypto weight 2.3x. Tiers: Bronze 3%/$2k, Silver 8%/$20k, Gold 18%/$200k, Platinum 32%/$1M, Diamond 44%/$4M, Obsidian 50%/$10M+ trailing 30-day wV. Key identity the skeptics surfaced: wV per dollar of taker fee paid is the constant 2.3/0.07 = 32.86 at every price, so tier is a pure function of total crypto taker fees paid and no routing choice can reach a tier more cheaply. In fee terms, Gold = ~$203/day of taker fees, Platinum = ~$1,014/day. At our ~$100-250/day gross taker fees we are solidly Silver, top end touching Gold: realistic $8-45/day, already accruing.

Maker rebates on passive exits. Crypto pool = 20% of that market's taker fees, distributed pro-rata by fee_equivalent = C x 0.07 x p(1-p) on FILLED maker orders, per-market. The structure self-normalizes: each maker fill earns back ~20% of the fee its filling taker paid, regardless of other makers. At p~0.5 a $50 clip (~100 shares) earns ~$0.35 per passive-exit fill. $1 pUSD daily minimum payout (~3 fills/day clears it).

Doc citations: docs.polymarket.com/trading/taker-rebates, /trading/fees, /market-makers/maker-rebates.

Validation plan (half a day total, no trading change):
1. Pull fills since 2026-05-28 only (no backfill exists). Compute wV as 32.86 x taker fees paid; confirm tier and that daily pUSD credits match. Open empirical question to settle from the credit stream: whether aggressive SELL legs earn wV (docs define Trade Size for buys; all examples are buys).
2. Run the passive-exit variant 3-5 days; reconcile daily maker-rebate credits against computed fee_equivalent share. Check whether sub-$1 accruals roll forward or are forfeited, and that the two credit streams are not double-counted in backtest economics.
3. Fold both into the fee model: effective taker fee = (1 - tier%) x 0.07 p(1-p); passive exit credit ~ +0.2 x 0.07 p(1-p) per maker-filled share. Re-score the passive-vs-taker exit comparison with both terms (note: moving exits to maker cuts taker wV roughly in half and can drop the tier; the comparison must be joint).

Hard rule from both skeptic passes: never add trades, loosen edge thresholds, or churn to cross a tier boundary. The only defensible boundary behavior is preferring taker exits over passive exits on trades already being taken, in the final days of a window where measured wV sits within a few percent of a boundary.

### 3.3 itode 250ms diagnostic + backtest alignment (TEST CHEAP, measurement only)

What is verified: BTC 5m markets return itode: true from GET clob.polymarket.com/clob-markets/{condition_id} (re-verified twice this week, different windows). Marketable orders are held 250ms, uncancelable, re-validated, then matched at the post-hold book. The FAK price field is a documented worst-price limit. What the econ skeptic established: our live code already sets taker limits at-touch and edge-capped (bonereaper_mm.rs:1880-1883, taker_slippage_ticks default 0.0), so there is no loose bound to tighten, and a price limit cannot filter the real toxicity channel anyway: makers cancel during our hold when Binance moves, so we keep fills precisely when the edge has decayed while the print stays inside any limit.

What survives, and is worth doing:
1. Markout study (1-2 hours): replay the last 2 weeks of live fills; bucket P&L by Binance move in the 250ms after order send. This sizes the maker-cancel adverse-selection channel and attributes the 0.45 capture deflator between itode toxicity and partial-fill depth. That attribution decides where effort goes next.
2. Backtest signal-horizon shift (~1 day): live fills execute at decision + 250ms + latency; the backtest fills at decision time. Align them. This is measurement hygiene and may change which configs win sweeps.
3. Config check (minutes): confirm the deployed edge06 config on whale-pair-dublin runs taker_slippage_ticks=0 (code default; no file override found, live box not checked).
4. If the markout shows material decay relative to entry edge, the coherent responses are raising min_edge on itode-flagged markets (cautiously: the participation optimum is interior, looser won the last sweep) and routing flow to the maker side, where we hold the cancel option instead of granting it. Read itode from the API at runtime; the flag has changed before.

### 3.4 Chainlink settlement-truth logger + sponsored key (TEST CHEAP, data infrastructure)

What is verified: 5m and 15m up/down markets resolve solely on the Chainlink data stream (market rules verbatim; tie rule end >= start resolves Up). The same feed is relayed free, no auth, on wss://ws-live-data.polymarket.com topic crypto_prices_chainlink (btc/usd, eth/usd, sol/usd, xrp/usd), measured at ~1Hz with 18-decimal full_accuracy_value. The docs explicitly offer a sponsored Chainlink Data Streams API key to up/down traders (form: pm-ds-request.streams.chain.link). Note: 1h/4h up/down are a different product resolving on Binance BTC/USDT candle open/close.

What the skeptics killed: the alpha claims. Winner direction is basis-invariant (8,308/8,308 windows in our own strike study), the crossed-mid tail is a genuine price-move tail rather than a strike artifact, exact ties are measure-zero on this feed, and settlement-basis strikes mixed into Binance-state beliefs is a proven anti-predictive mistake (the June official-strikes rerun collapsed +$8.7k into -$1.5k).

What survives:
1. Permanent archiver of crypto_prices_chainlink (plus crypto_prices Binance on the same socket). The official crypto-price API serves only a rolling ~30-day history; Feb-Apr is already unobtainable. This closes the settlement-verification and same-basis-strike gap permanently. 0.5 day.
2. Apply for the sponsored Data Streams key (free; the form targets 15m traders, we trade 15m; state both). Use LWBA bid/ask and a live USD/USDT basis estimate to replace the static ~14bps offset in the binance_proxy strike. Beliefs stay single-basis.
3. Optional, prior-of-zero: replay br2's historical toxic late-favourite fills against logged Chainlink prints; build an entry veto only if it clears roughly the 5:1 loss-avoided-to-winners-forgone bar. The existing diagnosis (reversal at fill time, AUC ~0.51) predicts it fires rarely.

### 3.5 Eligibility watcher (TEST CHEAP, ~1 hour, then forget)

Two machine-readable flags would change maker economics if they ever flip on our books; both verified OFF today: gamma rfqEnabled=false and positionIds absent on crypto up/down (RFQ/combos are sports/event-market only), and clob rewards = {rates: null, min_size: 50, max_spread: 4.5} (liquidity rewards configured but unfunded).

Build: one daily cron polling gamma (rfqEnabled, positionIds) and clob.polymarket.com/markets/{conditionId} (rewards.rates) for the BTC/ETH/SOL/XRP up/down series; alert on any change. Trigger semantics are re-validation, never trading: a rewards flip feeds the funded rate into the realistic-fill sim (scripts/mm_paired_realistic_sim.py) with the min_size-50 exposure and a farmer-dilution haircut, against the pre-registered hurdle that reward income at qualifying size must exceed the measured adverse-selection + stranding bleed at that size; an RFQ flip triggers a fee/flow/last-look-terms investigation. One-time prerequisite: hit combos-rfq-api.polymarket.com from whale-pair-dublin; a non-trading Irish IP got a 403 region block, and if the trading IP is also blocked, delete the RFQ branch rather than monitor a flag we cannot act on (Ireland is not on the documented geoblock list; the 403 may be IP classification).

### 3.6 Hygiene items (PARK, adopt opportunistically)

- Complete-set guard: never send a fee-paying book exit for a token while holding its complement in the same condition; merge (or hold to free redemption) instead. A few lines in the position ledger. Worth ~$0 today (taker sleeves almost never hold complete sets) but prevents a class of error and becomes load-bearing if any maker direction goes live.
- Post-only GTD for the passive exit: expiration = now + 60 + N (the 1-minute GTD security threshold is documented). Guarantees maker pricing, self-expires stranded exits without burning the much tighter cancel-all budget (250 req/10s), and an INVALID_POST_ONLY_ORDER rejection is a free "mid already crossed, flip to taker" signal.
- Engine-restart playbook: on HTTP 425 stop entries; expect a 2-minute post-only window after every restart (only post-only orders and cancels accepted). Subscribe to t.me/polytradingapis for the ~2-day advance notice.
- Merge/redeem facts for the playbook: merge is atomic, fee-free, gasless via relayer (needs a Relayer API key from Settings); redeem burns the entire balance per condition, no amount parameter, indexSets=[1,2] sweeps both sides in one call; position IDs are deterministically computable offline. Live experience prefers redeem-only over active merging.
- Queue-surface selection experiment (the only residue of the dual-surface idea): a one-day shadow log of whether the Yes-ask queue at p and the No-bid queue at (1-p) are ever asymmetric in depth at the same instant. If the book is byte-exact mirrored on V2 as it was pre-migration, drop permanently; if not, route the SINGLE exit order to the shorter queue. Never rest both.

## 4. Doc facts worth keeping (reference)

Fees and rebates:
- Taker fee = C x feeRate x p x (1-p) per aggressive leg, buys AND sells; crypto feeRate 0.07 (100 shares @ 0.50 = $1.75). Makers never charged. Fee symmetry is a protocol requirement: fee(sell A @ p) == fee(buy A' @ 1-p); no fee-arb path exists between the books.
- Split, merge, redeem: zero protocol fee, zero gas via relayer (Relayer API key required). 1 Yes + 1 No = $1.00 pUSD always; merge atomic; redeem gated on resolved=true.
- Maker rebates: crypto 20% of taker fees (other categories 25%, geopolitics 0), per-market pro-rata by fee_equivalent C x 0.07 x p(1-p) on filled maker orders only; daily pUSD, $1 minimum.
- Taker-rebate tiers (live 2026-05-28): 3/8/18/32/44/50% at $2k/$20k/$200k/$1M/$4M/$10M trailing 30-day wV; wV = size x (1-entry) x category weight (crypto 2.3x) x bonuses; taker trades only; recalc and pay daily; no backfill; inauthentic-trading clause. Identity: wV per taker-fee dollar = 32.86, constant.
- Liquidity rewards on 5m/15m up/down: configured (min_size 50, max_spread 4.5c) but UNFUNDED (rates null). Quadratic two-sided scoring S = ((v-s)/v)^2 x b, scaling factor c = 3.0, midpoint band 0.10-0.90.
- Builder fees: flat-notional, up to 100bps taker / 50bps maker, additive on top of platform fees, no platform-fee rebate to builders. Builder-tagged aggressor flow (onchain `builder` field) is plausibly price-insensitive retail: candidate fill-quality signal if observable in fills.

Order mechanics:
- 250ms itode taker delay on selected crypto up/down (BTC 5m confirmed): uncancelable hold, re-validation, rejection on failed checks; detect via clob-markets itode flag (read at runtime; it has been toggled historically). FOK/FAK price = worst-price limit. FOK for entries where partial fills break sizing; FAK fills-and-cancels.
- Heartbeats: 10s window + 5s buffer cancels all open orders; chained heartbeat_id; 400 response carries the correct id for resync.
- Batch: POST /orders max 15, parallel; DELETE /orders max 3,000 ids; cancel-all only 250 req/10s. POST /order burst 5,000/10s.
- GTD: effective lifetime N requires expiration = now + 60 + N. Post-only on GTC/GTD only.
- Tick flips 0.01 -> 0.001 when price crosses >0.96 or <0.04 (tick_size_change event): finer pricing exactly in the late-favourite zone.
- Engine restarts: HTTP 425, then 2-minute post-only mode; ~2 days notice on t.me/polytradingapis.
- Matching: unified book; MINT/COMPLEMENTARY/MERGE paths cross complementary orders. Trade statuses MATCHED -> MINED -> CONFIRMED (or RETRYING -> FAILED), pushed on the user channel.

Data and resolution:
- 5m/15m up/down resolve on Chainlink Data Streams (USD); tie rule end >= start resolves Up; no UMA bond/challenge (automated settlement). 1h/4h resolve on Binance BTC/USDT candle open/close: different product, different basis.
- RTDS wss://ws-live-data.polymarket.com, no auth: crypto_prices (Binance, btcusdt...), crypto_prices_chainlink (btc/usd..., ~1Hz, 18-decimal), equity_prices (Pyth). Client PING every 5s. Market/user channels: client PING every 10s.
- Sponsored Chainlink Data Streams API key offered to up/down traders: pm-ds-request.streams.chain.link.
- Market microstructure: tick 0.01, min order 5 shares, negRisk=false (standard CTF Exchange V2 0xE111180000d2663C0091e4f400237545B87B996B; collateral pUSD 0xC011a7E12a19f7B1f670d46F03B03f3342E82DFB). Verify signing against V2 and pUSD approvals before relying on CTF plumbing.
- RFQ/combos: combos-only today, crypto up/down ineligible (positionIds null, rfqEnabled false); 400ms quote window, 10s requester acceptance, 1s last look (Tier 1); region-block observed from a non-trading Irish IP.

Settled negatives (do not re-propose without new documented mechanics):
- Dual-surface resting orders: the books are one book; no second fill surface exists.
- Split-then-maker-sell entries: identical to rejected mint-then-sell; adverse selection binding.
- Pair arb breakeven: still ~0.965 at mids after two taker legs; CTF leg was never the cost.
- Tie-rule bias: exact ties measured at 0 in 8,308 windows; worth <1bp.
- wV routing/boundary churn: wV per fee dollar is constant; churn is negative EV and risks the inauthentic-trading clause.

## 5. This week

Day 1 (hours, no trading changes):
1. Rebate reconciliation: fills since 2026-05-28, wV = 32.86 x taker fees, confirm tier and daily pUSD credits; settle the sell-leg wV question empirically. Start the maker-rebate reconciliation alongside the passive-exit runs.
2. itode markout study: bucket the last 2 weeks of live fills by Binance move in the 250ms post-send window. Confirm taker_slippage_ticks=0 on the live box.
3. Apply for the sponsored Chainlink key (free, form takes minutes). Stand up the RTDS dual-topic archiver.
4. From whale-pair-dublin: test combos-rfq-api access (decides whether the RFQ watch branch exists at all).

Days 2-4:
5. Build the ops bundle: heartbeat with order-state reconciliation, user-channel push fills, batched ladders, persistent market channel with custom_feature_enabled. Verify each with its one-hour test before relying on it.
6. Fold both rebate terms into the fee model and re-score passive-vs-taker exits jointly.
7. Add the 250ms signal-horizon shift to the backtest and re-check which configs win.

Day 5 (small):
8. Ship the eligibility-watcher cron and the complete-set guard. Optionally the fee-adjusted crossed-pair log line and the one-day queue-asymmetry shadow log; both are tripwires, not strategies.

Not this week, explicitly: anything from the settled-negatives list, any tier-boundary behavior, any maker-entry construction, any 1h/4h build beyond an optional final-minutes shadow log.
