# Strategy Hunt (fresh start, 2026-06-16)

Systematic validation of trading strategies against local multi-market data at **$1K
bankroll** ($25 clips). Independent of prior autoloop verdicts — every cell gets a
fresh backtest.

## Protocol

See [00-protocol.md](00-protocol.md).

## Infrastructure

| Script | Purpose |
|---|---|
| `scripts/strategy_hunt_matrix.sh` | Launch alpha + walk-forward matrix |
| `scripts/score_strategy_hunt.py` | Score results, write scorecard |

```bash
# Screen all families on VERIFY window (12 days)
WINDOWS=VERIFY STRATEGIES=all MARKETS=btc5m,btc15m,eth5m,sol5m,xrp5m MAXJOBS=3 \
  ./scripts/strategy_hunt_matrix.sh

# Confirm survivors on TUNE window (78 days)
WINDOWS=TUNE STRATEGIES=F1 MARKETS=btc5m ./scripts/strategy_hunt_matrix.sh

# One-shot holdout for viable cells
WINDOWS=HOLDOUT STRATEGIES=F1 MARKETS=btc5m ./scripts/strategy_hunt_matrix.sh

python3 scripts/score_strategy_hunt.py
```

## Reports

| File | Status |
|---|---|
| [scorecard.md](scorecard.md) | Auto-generated leaderboard |
| [01-tune-confirmation.md](01-tune-confirmation.md) | F1/F7 cross-window |
| [02-walkforward.md](02-walkforward.md) | W1/W2/W3 vs F1 |
| [03-aws-plan.md](03-aws-plan.md) | EC2 commands for winners |

## Early findings (btc5m, $1K)

| Strategy | VERIFY | TUNE | HOLDOUT | Verdict |
|---|---:|---:|---:|---|
| **F1 exo fade** (timed exit) | +$11,841 | +$33,334 | pending | **VIABLE** |
| F2 aligned | −$1,025 | — | — | REJECT |
| F7 hold-to-res | +$12,079 | +$53,671 | — | REJECT (tail risk) |
| W1 back_to_explore | −$85 | — | — | REJECT |
| W2 paired_mm | −$277 | — | — | REJECT |
| W3 bonereaper_v2 | +$327 | — | — | WEAK |

Full multi-market VERIFY matrix is running in background (~50 cells).