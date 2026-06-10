# Alpha Hunt 003: Short-Horizon Asset Matrix

**Date:** 2026-06-10
**Protocol:** per family, tune (May 21-24) -> frozen test (May 25-28) -> June 1-7; canonical true-resolution manifests (Telonex markets parquet), real NO ladders, look-ahead-free strike, exit-30s, 150 ms.

## Verdicts

| family | tune | frozen test | June | verdict |
|---|---|---|---|---|
| btc-updown-5m | (hunt 001/002) | +$2,283 | **+$11,912** (hardened holdout) | **champion** |
| btc-updown-15m | +$1,470 (0.16), hit 66% | +$220, hit 59% | n/a locally (S3 follow-up) | **promising, thin sample** |
| eth-updown-15m | +$71-263 | -$184, hit 38% | n/a locally | rejected (again) |
| sol-updown-5m | -$1,657 | -$1,974, hit 31% | -$1,054 | rejected |
| sol-updown-15m | -$970 | -$598, hit 30% | -$324 | rejected |
| xrp-updown-5m | -$1,221 | -$2,194, hit 28% | -$1,502 | rejected |
| xrp-updown-15m | -$216 | -$414, hit 36% | -$515 | rejected |

(ETH-5m was rejected in hunt 002: ~flat on its frozen test.)

## The structural finding: feed leadership decides everything

SOL/XRP are not "no edge" (which looks like ~45-50% hit minus costs); they are **systematically backwards** (28-36% hits) while the belief's log-loss is normal (~= book). Interpretation: the fade is a *feed-leadership* trade. On BTC, Binance spot is the price-discovery venue, so our feed leads their book and "dislocations" are their staleness. On SOL/XRP, price discovery is distributed (perps, other venues); Binance spot prints lag information their books already priced — the "dislocation" is OUR staleness, and the book keeps moving against the entry.

A token-side inversion bug was ruled out: the first hunt-003 pass DID have inverted books from a bad manifest assumption (log-loss 0.8-0.9 — impossible-bad), was caught by exactly that signature, and discarded; this pass uses explicit canonical token sides and shows sane log-loss with bad trading hits — a different, real phenomenon.

## Follow-ups recorded (not free lunches; each needs its own tune/test cycle)

1. **Faster alt reference feed**: perp prices typically lead spot; the perp dataset already fetched can be tried as the SOL/XRP belief input.
2. **The inverse hypothesis**: 28-36% hit inverted is 64-72% — i.e., on alts, *follow* the book's early move (alignment) rather than fade it. This is effectively the directional model's territory.
3. **btc-15m scale-up**: positive on every split so far but only ~80-200 trades per window; the canonical manifest has 46k btc-15m markets back to Oct 2025 for a proper validation.

## Status of the deployable set

BTC-5m remains the only deployment-grade cell (Feb-June validated, hardened, stress-tested). BTC-15m is the nearest expansion candidate. Alts await the directional/feed-leadership work.
