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
| **Execution** | `pm-backtest::fills` | Taker/maker fills, fees, slippage, latency |
| **Portfolio** | `pm-risk::PortfolioState` | Equity, daily loss, per-market caps |

## Strategy

A **Strategy** implements `pm_strategy::Strategy::on_event` and composes the layers
above. Configuration is a serde struct (`*Config`) of compiled defaults plus CLI
flags. Decision logic that must match live is pure functions in
`pm-alpha::decide` (SSOT).

## Strategies

**There are none, deliberately.** The 2026-08 framework reset ended with zero
deployable strategies: the framework is the deliverable, and a new strategy
starts from a codebase that cannot report the numbers that misled the June 2026
live cycle. What `--strategies` accepts:

| Name | Type | Status |
|---|---|---|
| `noop` | Emits no orders | the default; exercises the loader, fill engine and accounting without taking a position |
| `fixture` | `ThresholdFadeStrategy`: buy the cheap side once per market, hold to resolution | TEST-ONLY. Not deployable, rejected without `--allow-fixture`. It exists solely to anchor the golden replay gate so the hash covers the order/fill/settlement path |

Neither is alpha. The fixture loses money on the pinned day, on purpose.

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

The engine's own regression gate is the pinned-tape golden replay:
`bash scripts/research/golden_replay.sh check` must print `GOLDEN: IDENTICAL`
after any change meant to be behavior-preserving. It replays a fixed day
through the `fixture` strategy and hashes the normalized output. The hash is an
anchor: a divergence means bisect the engine, not re-record the hash. See
`tests/golden/README.md`.