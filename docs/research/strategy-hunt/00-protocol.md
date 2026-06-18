# Strategy Hunt Protocol (fresh start, 2026-06-16)

Independent of prior autoloop verdicts. Goal: systematically discover which strategy
families have positive fee-net EV at a **$1K bankroll** across the multi-market
dataset we actually have cached.

## Data inventory

| Source | Coverage |
|---|---|
| Tick cache | 113 days, 2025-11-26 → 2026-06-10 (`data/cache/ticks/`) |
| Canonical manifests | 374k markets, 7 assets × {5m,15m,4h} (`data/manifests/canonical/`) |
| Binance spot | BTC/ETH/SOL/XRP agg_trades in `data/cache/raw/binance/` |
| Binance perp | futures agg_trades, funding, metrics (BTC primary) |
| AWS S3 | Full history via `pm-research-data-prod` when local cache gaps |

## Bankroll model

- Starting equity: **$1,000**
- Alpha harness: `--notional-usdc 25` (2.5% per clip; matches deployment frame)
- Walk-forward: `--starting-cash 1000 --portfolio-mode --clip-fraction-of-equity 0.025 --max-clip-usdc 30`
- Fees: `--fee-curve-rate 0.07` (Polymarket crypto taker curve)
- Latency: `--latency-ms 150` (conservative vs Dublin→London ~120ms)

## Strategy families (testable now)

| ID | Family | Mechanism | Harness expression |
|---|---|---|---|
| F1 | Exo fade | BSM belief disagrees with book; capture convergence | Fade, exit 30s, passive exit |
| F2 | Exo aligned | Belief agrees with book; ride continuation | Aligned, align_min 0.55 |
| F3 | Maker fade | Rest bid below ask; zero-fee entry, hold | maker_entry_offset 0.01 |
| F4 | Late-window | Enter only in final N seconds of window | enter_within_close_s |
| F5 | Pair completion | Lock profit when opposite ask cheap enough | pair_completion_margin |
| F6 | Regime gate | Trade only expanded or only calm tape | skip_calm / only_calm |
| F7 | Hold-to-res | No timed exit; resolution settlement | exit_after_s 0 |
| F1 | exo_fade | **Canonical quant impl** — full pipeline via `pm-strategy` | walk-forward `exo_fade` |
| W1 | back_to_explore | Paired maker / policy replay | walk-forward strategy |
| W2 | paired_mm | Sub-parity two-sided quoting | walk-forward strategy |
| W3 | bonereaper_v2 | Late favourite + tail lanes | walk-forward strategy |

**Promotion rule:** strategies validated in the `alpha` harness must be ported to a
`pm-strategy` implementation (like `exo_fade`) before they count as deployable.
The harness is for fast research; walk-forward is the authoritative backtest.

## Markets to screen

**Tier 1** (deepest books, best feed): btc-updown-5m, btc-updown-15m, eth-updown-5m
**Tier 2** (alt feed-leadership test): sol-updown-5m, xrp-updown-5m
**Tier 3** (exploratory): doge-updown-5m, hype-updown-5m, eth-updown-15m

## Validation windows (no leakage)

| Window | Dates | Purpose |
|---|---|---|
| TUNE | 2026-02-12 → 2026-04-30 | Selection, parameter choice |
| VERIFY | 2026-05-07 → 2026-05-18 | Confirm without re-tuning |
| HOLDOUT | 2026-05-19 → 2026-05-28 | One-shot final test (winners only) |

June 2026 is sealed. Never use it for selection.

## Adoption criteria

Signal screen (Phase A, TUNE only): t-stat\((\varepsilon^{\text{net}}) > 2\) on fee-adjusted
edge distribution (`scripts/quant_signal_screen.py`). Formulae: `05-quant-signals.md`.

A strategy is **viable** if ALL of:
1. Fee-net NET > 0 on TUNE and VERIFY
2. Daily Sharpe > 1.0 on VERIFY (annualized: \(\bar{P}_d / \sigma(P_d) \cdot \sqrt{252}\))
3. Worst VERIFY day > -5% of starting bankroll (-$50)
4. Hit rate > 0.50 on VERIFY (for directional strategies)
5. Survives 25% depth-capture stress OR documents live-capture discount
6. Log-loss: \(\mathcal{L}_{\text{exo}} < \mathcal{L}_{\text{book}}\) on VERIFY (model beats market)

Strategies passing TUNE+VERIFY get exactly **one** HOLDOUT run.

## Scoring

Primary: fee-net NET. Secondary: daily Sharpe, net/trade, trades/day (capacity),
max drawdown proxy (worst day). Delete `*.trades.jsonl` after scoring to save disk.