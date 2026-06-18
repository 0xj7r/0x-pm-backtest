# polymarket-backtest — domain context

## Quant pipeline (canonical)

Every tradeable strategy follows the same layered pipeline. No layer may skip
another; backtest and live share identical decision code at each layer.

```
MarketData → FeatureState → AlphaModel → StrategyDecision → RiskEngine → Execution → Portfolio
```

| Layer | Crate | Responsibility |
|---|---|---|
| **MarketData** | `pm-types`, loaders | Spot, perp, book tapes; latency-shifted observations |
| **FeatureState** | `pm-alpha::state` | Typed exogenous snapshot (`ExoState`); no book in belief |
| **AlphaModel** | `pm-alpha::model` | `belief(ExoState) → P(up)`; strict exogenous |
| **StrategyDecision** | `pm-alpha::decide`, `pm-strategy` | Entry/exit gates, side pick, rearm state |
| **RiskEngine** | `pm-risk` | Kelly sizing, exposure caps, drawdown halt |
| **Execution** | `pm-app::runner` | Taker/maker fills, fees, slippage, latency |
| **Portfolio** | `pm-risk::PortfolioState` | Equity, daily loss, per-market caps |

## Strategy

A **Strategy** implements `pm_strategy::Strategy::on_event` and composes the layers
above. Configuration is a serde struct (`*Config`) loaded from TOML profiles.
Decision logic that must match live is pure functions in `pm-alpha::decide` (SSOT).

## Active strategies

| Name | Type | Status |
|---|---|---|
| `exo_fade` | Exogenous fade + timed exit | **canonical** — quant reference impl |
| `back_to_explore` | Two-sided ladder taker | legacy paired-maker |
| `paired_mm` | Sub-parity quoting | research |
| `bonereaper_v2` | Late favourite lanes | research |

## Quant signals (SSOT)

Belief: BSM binary digital \(\hat{p}_{\text{up}} = \Phi(\ln(S/K) / (\sigma_{\text{bar}}\sqrt{\tau}))\)
in `pm-alpha::fair_value`. Vol: trailing realized, bar-scaled (`pm-alpha::vol`). Edge:
\(\varepsilon_s = \hat{p}_s - a_s\); fee \(f(p) = 0.07\, p(1-p)\) per share. Decision:
`pm-alpha::decide::decide_entry` (SSOT for live + backtest). Full spec:
`docs/research/strategy-hunt/05-quant-signals.md`.

Discovery flow: signal screen (t-stat on \(\varepsilon^{\text{net}}\)) → alpha harness →
walk-forward @ $1K. See `docs/research/strategy-hunt/04-first-principles.md`.

## Validation

Backtests run through `pm-app walk-forward` with `--portfolio-mode` and
`--starting-cash`. The `alpha` subcommand is a fast research harness only; promoted
strategies must pass through walk-forward before deployment.