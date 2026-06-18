# Strategy Hunt — Walk-Forward VERIFY (2026-06-16)

Walk-forward portfolio replay for the three legacy strategy families (W1–W3) on
btc-updown-5m, compared against the alpha F1 fade baseline on the same VERIFY window.

## Setup

| Parameter | Walk-forward (W1–W3) | Alpha baseline (F1) |
|---|---|---|
| Window | 2026-05-07 → 2026-05-18 (12 days) | same |
| Bankroll frame | $1,000 start, portfolio mode | $25 fixed clip (2.5% frame) |
| Clip sizing | `--clip-fraction-of-equity 0.025 --max-clip-usdc 30` | `--notional-usdc 25` |
| Fees | taker curve via engine defaults + maker rebates | `--fee-curve-rate 0.07` |
| Latency | engine default (0 ms taker in WF) | 150 ms |
| Markets | 3,425 succeeded / 3,456 in filtered manifest | 3,445 run / 3,456 considered |
| Replay | `--replay-sample-ms 1000` | 1 s decision grid |

**Note:** `pm-app walk-forward` does not accept `--date-start` / `--date-end`. VERIFY
markets were sliced via filtered manifest:

`data/manifests/strategy_hunt/btc-updown-5m_up_VERIFY.jsonl`

## Commands

```bash
MANIFEST=data/manifests/strategy_hunt/btc-updown-5m_up_VERIFY.jsonl
WF_COMMON="--markets $MANIFEST --local-cache-dir data/cache \
  --starting-cash 1000 --portfolio-mode \
  --clip-fraction-of-equity 0.025 --max-clip-usdc 30 \
  --use-outcome-label --spot-symbol BTCUSDT --replay-sample-ms 1000"

./target/fast/pm-app walk-forward $WF_COMMON --strategies back_to_explore \
  --out-summary data/runs/strategy_hunt/VERIFY_W1_bte_btc5m/summary.json \
  --out-markets data/runs/strategy_hunt/VERIFY_W1_bte_btc5m/markets.jsonl

./target/fast/pm-app walk-forward $WF_COMMON --strategies paired_mm \
  --out-summary data/runs/strategy_hunt/VERIFY_W2_paired_btc5m/summary.json \
  --out-markets data/runs/strategy_hunt/VERIFY_W2_paired_btc5m/markets.jsonl

./target/fast/pm-app walk-forward $WF_COMMON --strategies bonereaper_v2 \
  --out-summary data/runs/strategy_hunt/VERIFY_W3_br2_btc5m/summary.json \
  --out-markets data/runs/strategy_hunt/VERIFY_W3_br2_btc5m/markets.jsonl
```

F1 baseline (already complete): `data/runs/strategy_hunt/VERIFY_F1_fade_btc5m.json`

## Headline results

| ID | Strategy | NET PnL | Final equity | Return | vs F1 NET |
|---|---|---:|---:|---:|---:|
| **F1** | exo fade (alpha) | **+$11,841** | n/a† | n/a† | — |
| W1 | back_to_explore | −$85 | $915 | −8.5% | −$11,926 |
| W2 | paired_mm | −$277 | $723 | −27.7% | −$12,118 |
| W3 | bonereaper_v2 | **+$327** | **$1,327** | +32.7% | −$11,514 |

† F1 alpha sums per-trade fee-net PnL at fixed $25 clips without portfolio compounding;
there is no single compounded final-equity figure.

## Adoption gates (VERIFY)

| ID | NET > 0 | Daily Sharpe | Worst day | Hit rate | Verdict |
|---|---:|---:|---:|---:|---|
| F1 | **+$11,841** | 46.7 | +$415 | 63.0% | **VIABLE** |
| W1 | −$85 | −0.5 | −$467 | 44.6% | REJECT |
| W2 | −$277 | −25.1 | −$77 | 15.5% | REJECT |
| W3 | +$327 | 2.6 | −$334 | 28.9%‡ | REJECT |

‡ W3 market-level hit rate is low (28.9%) because many markets have mixed lane PnL;
per-fill-tag hit rates on active lanes are 75–87%.

Walk-forward Sharpe and worst-day computed from daily portfolio PnL
(`scripts/daily_pnl.py` on each `markets.jsonl`). F1 stats from
`scripts/score_strategy_hunt.py` on trade log.

## Daily PnL shape

All three walk-forward strategies share a brutal 2026-05-13 (−$467 W1, −$77 W2,
−$334 W3). W1 also bleeds on 2026-05-11 (−$453) and 2026-05-18 (−$309).
W2 bleeds steadily every day (−$12 to −$77). W3 is the only walk-forward family
with multiple strong up days (May 8 +$386, May 14 +$289, May 16 +$244) but path
max drawdown is 46%.

F1 fade had **every VERIFY day positive** (worst +$415).

## Mechanism notes

**W1 back_to_explore** — Heavy taker footprint (24,962 fills, 99.9% taker). Model
gate rejected 133k orders. High fill notional ($74k) with poor compounding: peaked
above $2,012 intrawindow but finished −8.5%. Maker participation is negligible.

**W2 paired_mm** — Pure maker (20,333 fills, 100% maker, zero slippage). Steady
daily bleed (−$12 to −$77/day) suggests sub-parity quoting does not recover fees /
adverse selection on this tape at 2.5% clips.

**W3 bonereaper_v2** — Sole walk-forward survivor on raw NET (+$327, 3,150 fills).
Late-favourite and tail lanes carry PnL; participation maker lane is net negative.
Compounds well on trend days but −$334 worst day fails the −5% bankroll floor.

## Comparison vs F1 fade

1. **Magnitude gap.** F1 NET (+$11,841) is **36×** W3 (+$327), the best walk-forward
   result. Even after accounting for harness differences (F1 non-compounding sum vs
   WF portfolio compounding), F1 dominates on this window.

2. **Risk profile.** F1: all days green, Sharpe 46.7. Walk-forward: all three breach
   worst-day gate; W1/W2 also fee-net negative.

3. **Capacity vs edge.** W1 trades 8× more fills than F3/W3 but loses money — volume
   without edge. W2 quotes every market but cannot extract spread. W3 is selective
   (3150 fills) and profitable but far below F1 per-day NET ($27/day vs $987/day).

4. **No walk-forward holdout candidate.** None of W1–W3 pass adoption gates. F1
   remains the only VERIFY survivor eligible for HOLDOUT (see `01-tune-confirmation.md`).

## Artifacts

| Run | Summary | Markets JSONL | Wall time |
|---|---|---|---|
| W1 | `data/runs/strategy_hunt/VERIFY_W1_bte_btc5m/summary.json` | `.../markets.jsonl` | ~12 min |
| W2 | `data/runs/strategy_hunt/VERIFY_W2_paired_btc5m/summary.json` | `.../markets.jsonl` | ~4 min |
| W3 | `data/runs/strategy_hunt/VERIFY_W3_br2_btc5m/summary.json` | `.../markets.jsonl` | ~5 min |
| F1 | `data/runs/strategy_hunt/VERIFY_F1_fade_btc5m.json` | `.../trades.jsonl` | ~1 min |

## Next steps

- Do **not** run HOLDOUT for W1–W3.
- Proceed with F1 HOLDOUT only (`WINDOWS=HOLDOUT STRATEGIES=F1 MARKETS=btc5m`).
- Consider fixing `strategy_hunt_matrix.sh` walk-forward stanza: replace
  `--date-start/--date-end` with manifest filtering (or add date flags to `walk-forward` CLI).
- Delete large `markets.jsonl` files after scoring if disk is tight (summaries retained).