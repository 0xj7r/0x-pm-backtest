# ETH-5m fade inversion: diagnosis

Date: 2026-06-12/13. Window W3 (2026-05-07..05-18), passive-exit combo.

## Verdict

BUG. The ETH-5m fade run blended the BTCUSDT perp price into the ETH belief.
This is a perp-symbol misrouting in the batch script, not an ETH efficiency
result. The inverted hit rate (0.334) and the broken `log_loss_exo` (2.00 vs
`log_loss_book` 0.49) are both explained by the contamination. Fixing it will
not make ETH automatically profitable, but it removes the systematic
wrong-side bias and gives an honest test. See the re-run options below.

## Evidence chain

1. The run config (`W3_eth5_passive.json`) has `perp_price_weight: 0.5` and
   `entry_mode: Fade`. The log proves the mismatch directly:
   - line 3: `perp state loaded symbol="BTCUSDT" days=12 trades=13026062`
   - lines 5+: `spot day loaded symbol="ETHUSDT"`
   So spot is correctly ETH, but the perp tape feeding the belief is BTC.

2. The batch script bakes the symbol into a shared string:
   `scripts/batch0612.sh:25` `COMBO="--perp-symbol BTCUSDT ..."`, and the ETH
   line (`${W}_eth5_passive`) reuses `$COMBO`. There is no per-asset perp
   routing: `crates/pm-app/src/alpha.rs:754` passes `args.perp_symbol`
   verbatim to `load_perp_state` for every market in the run. Whatever the
   operator types is used for all assets.

3. Contamination mechanism (`crates/pm-alpha/src/model.rs:73-91`,
   `effective_spot`):
   `eff_spot = (1-w)*spot_eth + w*(perp_now - basis)`,
   `basis = median_3600s(perp - spot_eth)` (`state.rs:102`).
   Substituting BTC perp for an ETH market, the distortion of eff_spot away
   from true ETH spot is
   `w * ((perp_btc_now - spot_eth_now) - median_1h(perp_btc - spot_eth))`,
   i.e. `w` times the deviation of the BTC-minus-ETH gap from its 1h median.
   Because the basis is a stale 1h median while `perp_btc_now` is live, this
   term is dominated by BTC's recent dollar move, injected at BTC's ~$100k
   scale.

4. Scale check (`data/runs/alpha/lane_xtoken/eth_w3.trades.jsonl`): ETH 5m
   strikes sit a median of $0.06 apart and ETH spot is ~$2,286. BTC moves tens
   to hundreds of dollars in a 5-minute window; with `w=0.5`, roughly half of
   that BTC dollar-move lands in eff_spot. A $50-100 distortion against $0.06
   strike spacing completely swamps the genuine ETH signal. The fade's belief
   direction becomes a function of BTC's recent ticks, not ETH's. BTC and ETH
   5m moves are correlated but not identical, so the fade systematically takes
   the wrong side: hit 0.334, gross-negative.

5. Why the LANE looked neutral while the FADE inverted: p_exo is not globally
   sign-flipped. On the lane trades, p_exo agrees with the realized outcome
   (P(up | p_exo>0.66)=0.921, P(up | p_exo<0.34)=0.092, anti-fraction 0.086),
   because both the lane's book-favourite rule and the belief track the obvious
   near-close favourite. The contamination only distorts the cases where the
   belief diverges from the book by more than the 0.16 edge threshold, which is
   exactly the set the FADE trades on (and the lane never trades: zero lane rows
   have `|p_exo - mid| >= 0.16`). So the divergence between lane-neutral and
   fade-inverted is itself a fingerprint of a belief-construction bug localized
   to the large-edge tail, not an asset property.

## Fix and re-run

Root cause: single global `--perp-symbol` with no per-asset routing, plus a
batch script that hardcoded BTCUSDT into the shared `$COMBO`.

Cache constraint: ETHUSDT perp aggTrades are NOT in the cache. Only
`channel=agg_trades/symbol=ETHUSDT` (spot) exists;
`channel=futures_agg_trades` has BTCUSDT, SOLUSDT, XRPUSDT but no ETHUSDT.
So `--perp-symbol ETHUSDT` would log "perp trades day missing", `state.perp`
would be empty, and `effective_spot` would silently fall back to spot_now
(no contamination, but also no perp signal).

Two correct re-runs:

A. Honest ETH fade with NO perp blend (immediate, no data fetch). Drop the
   perp entirely so eff_spot = ETH spot:

   ./target/fast/pm-app alpha \
     --local-cache-dir data/cache --down-assets data/manifests/canonical/down_all.jsonl \
     --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 \
     --stop-before-close-s 90 --fee-curve-rate 0.07 \
     --markets data/manifests/canonical/eth-updown-5m_up.jsonl --slug-prefix eth-updown-5m- \
     --date-start 2026-05-07 --date-end 2026-05-18 \
     --perp-price-weight 0 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08 \
     --exit-at-mid --passive-exit-timeout-s 60 --edge-thresholds 0.16 --exit-after-s 30 \
     --out-json data/runs/alpha/batch0612/W3_eth5_passive_noperp.json \
     --trades-out data/runs/alpha/batch0612/W3_eth5_passive_noperp.trades.jsonl

B. ETH fade WITH the correct ETH perp (after fetching ETHUSDT futures):
   first `python scripts/binance_perp_fetch.py` for ETHUSDT over 2026-05-06..05-18,
   then the same command as A but with `--perp-symbol ETHUSDT --perp-price-weight 0.5`.

Prediction: option A removes the wrong-side bias; expect the hit rate to rise
from 0.334 back toward the no-edge / book-consistent regime and
`log_loss_exo` to drop from 2.0 toward the BTC-fade range. Whether ETH then
clears costs (passive-exit fees were $6,868 on 4,332 trades) is a separate
question and the genuine ETH verdict should be read off run A, not the
contaminated run. Do not conclude "ETH unfadeable" from the current numbers.

## Note on per-asset routing (follow-up, not required for this diagnosis)

A durable fix is to route the perp symbol per market (infer ETHUSDT for
eth-updown slugs, BTCUSDT for btc-updown, etc.) inside the perp loader, or at
minimum to assert symbol-vs-slug consistency at run start so a BTC perp on an
ETH market fails loudly instead of silently corrupting the belief.
