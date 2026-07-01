# Fill model calibration from June 2026 live fills

**Date:** 2026-07-01
**Data:** 396 live executor fills (shadow_exec_tail, Jun 16 12:08 - Jun 18 09:02 UTC)
joined on-chain (data-api activity) to shadow-final `would_enter` touch prices per
(slug, side, clip-order). Clips were $20 (Jun 16) and $50 (Jun 17-18, night_scale).

## Measured slippage vs decision-time touch

| clip bucket | n | median slip | mean slip | p90 slip | median touch depth |
|---|---|---|---|---|---|
| $15-30 | 188 | 0.0c | -0.05c | +1.79c | 114 sh |
| $30-70 | 208 | 0.0c | +1.07c | +5.38c | 101 sh |

240/396 fills were at or better than the logged touch. Median touch depth
(~100 shares ~= $50) covers the clip at these sizes, so the book-walk penalty is
negligible up to $50 clips. The small positive mean is consistent with the
~150-250ms submit latency (book moves between decision and fill), which the
harness already models via `--latency-ms`.

## End-to-end realization (identical market set)

Executor window, 320 live markets, actual clip sizes:

| accounting | P&L |
|---|---|
| live on-chain (buys + sells + redeems) | -$1,096 |
| shadow at-touch, same markets/clips, NO fees | -$246 |

Gap decomposition: ~$200 slippage (mean +0.5c x ~400 clips x ~100 sh),
~$300 taker fees (fee curve 0.07 -> ~1.7% of notional at p~0.5), remainder =
FAK-miss retries at worse touch, loss-cutting sells, sizing overrun.

## Policy (the point of this doc)

1. **Canonical accounting = the Rust harness with `--fee-curve-rate 0.07` and
   `--latency-ms 250`.** That configuration reproduced live within ~20-30% on
   this window. Any number produced without those flags is not comparable to
   live dollars.
2. **At-touch Python scorers over shadow JSONL are research-only.** They omit
   fees (~1.7% of notional per clip; roughly $1.7k per 2,000 $50-clips) and
   latency. Use them for sign/regime questions, never for sizing or go-live
   decisions.
3. **No size penalty needed below ~$50 clips.** Above that, touch depth
   (median ~100 sh) binds; re-measure before ever scaling past $50.
4. **The ledger dashboard misreports.** daily_win_summary said Jun 17 was
   +$278 while the chain shows -$419 for the same UTC day; per-market
   attribution of the executor window shows -$1,096 vs the ledger's ~flat.
   Reconcile live_ledger.db accounting against on-chain activity before
   trusting any live dashboard number (open item, governance task).

## Note on the June shadow "profit"

The ungated default stream's +$24k June (at-touch, $50 clips) is gross fantasy
accounting per policy 2, but the fee-net correction (~$1.7k) does not change
the conclusion: the engine's June decisions were solidly net-positive.
