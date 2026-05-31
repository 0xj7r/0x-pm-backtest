# Paired-MM Per-Day, Regime-Tagged (May 2026) — does the edge concentrate in calm?

Script: `scripts/mm_paired_by_day.py` (reuses `mm_paired_sim.py` internals). Run 2026-05-31.

## Framing (important)
Our ENTIRE BTC-5m book sample is May 7-20 (14 days, 4000 markets). There is no earlier
month captured, so "May performance" is the whole backtest. We CANNOT test the paired-MM
in a genuinely trending regime from this data. What we CAN do: split May's 14 days into
their calmer and more-volatile thirds and ask whether the MM's edge concentrates in the
calm days (its regime) or holds across both.

Regime axis = mean absolute 5-minute BTC move across each day's markets (bps). TRENDING =
top third (>= 7.1 bps); else calm. Config: clip 10, no rebate; recommended = ungated.

## Result (clip 10, ungated, no rebate)

| date | regime | vol_bps | mkts | net | /mkt | pos% | resid% |
|------|--------|---------|------|------|------|------|--------|
| 05-07 | TREND | 7.6 | 260 | $28.29 | 0.109 | 56 | 19.0 |
| 05-08 | calm | 6.4 | 286 | $28.43 | 0.099 | 52 | 18.5 |
| 05-09 | calm | 3.5 | 285 | $25.34 | 0.089 | 54 | 22.0 |
| 05-10 | calm | 5.5 | 286 | $45.68 | 0.160 | 56 | 15.6 |
| 05-11 | TREND | 7.8 | 286 | $48.31 | 0.169 | 56 | 21.7 |
| 05-12 | calm | 6.1 | 278 | $32.41 | 0.117 | 58 | 21.5 |
| 05-13 | calm | 5.2 | 80 | $0.68 | 0.008 | 48 | 28.5 |
| 05-14 | TREND | 8.4 | 286 | $22.07 | 0.077 | 52 | 22.2 |
| 05-15 | TREND | 7.2 | 279 | $20.80 | 0.075 | 52 | 23.0 |
| 05-16 | calm | 4.2 | 286 | $39.63 | 0.139 | 56 | 22.1 |
| 05-17 | calm | 4.5 | 286 | $43.56 | 0.152 | 55 | 18.5 |
| 05-18 | TREND | 7.9 | 286 | $31.88 | 0.111 | 52 | 22.2 |
| 05-19 | calm | 6.8 | 286 | $48.44 | 0.169 | 57 | 22.9 |
| 05-20 | calm | 5.9 | 283 | $47.33 | 0.167 | 58 | 19.9 |

- TOTAL ungated: $462.83 / 14 days = **$33.06/day**.
- CALM days (9): $34.61/day. TREND days (5): $30.27/day.
- TOTAL gated: $315.65 = $22.55/day (gate forgoes ~780 markets).

## Reading

1. **Positive every single day.** Worst is 05-13 (+$0.68) on a thin 80-market partial-data
   day. The edge is not concentrated in a few days.

2. **Essentially regime-robust within this sample** (calm $34.6 vs mild-trend $30.3 per day).
   Strict pairing is why: it captures the spread whether or not there is mild directional
   drift. The worry that it would only work in calm does not materialize here.

3. **CAVEAT — the "trend" gradation is mild** (7-8 bps vs 4-6 bps). This is NOT a real
   trending regime (the kind br2 profits on). A genuinely sustained one-way move would
   strand the residual leg (one side fills, cannot pair, resolves adverse). This sample
   cannot show that. It is exactly why the live residual cap (=1) and late-pull exist.

4. **CAVEAT — this base sim is UNCAPPED.** The $33/day clip-10 includes ~20% residual P&L
   (a directional component). The cap-1 spread-dominated config was ~$24.82/day
   (`calm_regime_paired_mm_gate_cap_sweep`), and the pure-spread floor ~$14.5/day. The
   repeatable number is the lower, spread-dominated one, not $33.

5. **The regime gate REDUCES net here** ($22.55/day) because May's universe is ~99% in-band,
   so it mostly forgoes good markets. But on the genuinely higher-vol days (05-12: ungated
   $32 vs gated $8; 05-18: $32 vs $15) it roughly halves exposure — the tail insurance for
   stressed regimes we do not have in-sample.

## Bottom line
Within the only regime we have data for (May calm), the paired-MM is robustly daily-positive
and not fragile to mild vol. The untested risk is a real sustained trend, which is precisely
what the residual cap + late-pull guard against live. The decisive unknown remains QUEUE
CAPTURE (pro-rata assumption), resolvable only by INC3 tiny-real fills.
