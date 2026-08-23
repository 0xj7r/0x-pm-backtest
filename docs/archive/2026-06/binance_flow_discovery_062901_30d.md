# Binance Order-Flow + Spot Accel Reversal Signal Discovery

Source: `/tmp/btc5m_postfill_markets_062901_full.jsonl`
Target label: `crossed_mid_after_fill` (post-fill path). Focus = last 30d drawdown window.
Test fills: 1241 | Toxic in window: 486 | Profitable non-toxic: 745

This pass specifically tests the three families that were *not* in the earlier regime/price/model-feature analysis.

## New Signal Families — Standardized Mean Difference (toxic vs profitable non-toxic)

| Feature | Toxic Mean | Good Mean | SMD | Single AUC (test) |
|---|---:|---:|---:|---:|
| binance_flow_imbal_5s | 0.0629 | 0.0644 | -0.002 | 0.504 |
| binance_flow_imbal_15s | -0.0650 | 0.0775 | -0.243 | 0.433 |
| binance_flow_imbal_30s | -0.1037 | 0.0922 | -0.389 | 0.393 |
| binance_adverse_vol_5s | 2.6373 | 1.2965 | +0.150 | 0.556 |
| binance_adverse_vol_15s | 6.6101 | 2.3048 | +0.334 | 0.622 |
| binance_adverse_vol_30s | 9.1837 | 3.4787 | +0.406 | 0.638 |
| binance_large_adverse_count_10s | 0.0000 | 0.0000 | +0.000 | 0.505 |
| binance_trade_intensity_15s | 16.7021 | 12.7488 | +0.294 | 0.602 |
| spot_ret_5s | -0.0000 | 0.0000 | -0.076 | 0.503 |
| spot_ret_15s | -0.0000 | 0.0000 | -0.214 | 0.450 |
| spot_ret_30s | -0.0001 | 0.0001 | -0.349 | 0.415 |
| spot_accel_15s_vs_30s | 0.0000 | -0.0000 | +0.286 | 0.590 |
| spot_accel_5s_vs_15s | 0.0000 | -0.0000 | +0.226 | 0.550 |

## Base Features (price / model / regime) on the same test window for reference

| Feature | SMD | Single AUC |
|---|---:|---:|
| price | -0.345 | 0.364 |
| side_model_p | -0.408 | 0.386 |
| side_edge_vs_fill | +0.006 | 0.551 |
| risk_score | +0.155 | 0.553 |
| regime_whipsaw_score | +0.054 | 0.517 |
| regime_reversal_pressure | +0.093 | 0.527 |
| regime_path_efficiency | -0.181 | 0.459 |
| market_yes_range_so_far | +0.137 | 0.525 |

## New-features-only logistic (train on pre-window, test on drawdown window)
Test AUC using **only** the Binance flow + spot accel family: **0.604**

## Interpretation & Next Steps

If any of the new Binance flow or accel features show |SMD| materially larger than the base set (~0.4 was the previous best), or if the new-features-only model reaches AUC > ~0.60 on the exact 30d drawdown window, we have a real, previously untested signal.
That signal would let us:
- Size the favourite loads more aggressively on the ones the flow says are stable.
- Concentrate the convex tail protection on the fragile subset (higher coverage frac only where the signal is bad).
- Potentially reduce the blanket convex premium on the full history while still protecting the tail.

Run this against the exact jsonl produced by the current convex_scaled / convex_reversal backtest for the cleanest apples-to-apples read.