# polymarket-backtest

Pure-Rust backtesting, paper, and live framework for Polymarket BTC/ETH
updown markets. Runs cloud-first against Telonex/Binance data mirrored to S3
(no local data dependency for the core engine), and is built so that the
framework itself enforces the constraints that past live losses violated
(truthful latency, fee-net accounting, jittered-replay validation), rather
than relying on discipline at run time.

## Workspace

```
crates/
├── pm-types/            # market/tape/spot types (ReplayEvent, SpotHistory, TradeHistory, MarketId)
├── pm-risk/              # Kelly/fractional sizing, PortfolioState (drawdown, daily/per-market caps)
├── pm-telonex-loader/    # S3/local-cache streaming loaders (book, trades, onchain, Binance); nautilus-free
├── pm-model/             # canonical 4-score model; removal deferred to Plan 2 (still wired into runner.rs)
├── pm-alpha/             # belief (BSM digital), vol, fee curve, ExoState features, equivalence machinery
├── pm-strategy/          # Strategy trait + exo_fade (Plan 2 extraction reference) + NoopStrategy
├── pm-shadow/            # live-twin log-only engine (JSONL stream, executor tail seam)
└── pm-app/               # CLI: discover-day | walk-forward | shadow | paper | live | equivalence
```

## Quickstart

```bash
# Build
cargo build --release -p pm-app

# Authenticate to S3 (us-east-1, bucket pm-research-data-prod)
eval "$(AWS_PROFILE=visumlabs aws configure export-credentials --format env)"
export PM_TELONEX_REGION=us-east-1

# Discover markets for a day from S3/availability API.
./target/release/pm-app discover-day --date 2026-05-12 --out /tmp/markets.jsonl

# Run walk-forward. exo_fade is the only strategy currently wired up.
./target/release/pm-app walk-forward \
    --markets /tmp/markets.jsonl \
    --strategies exo_fade \
    --starting-cash 100 --max-clip-usdc 5 --spot-symbol BTCUSDT \
    --use-outcome-label \
    --out-markets /tmp/wf.jsonl --out-summary /tmp/wf-summary.json
```

`--strategies` accepts `exo_fade` or `noop`; there is nothing else to pick.
`--profile` is still accepted for backward compatibility but applies nothing
and warns; profile-driven strategy overrides were removed with the legacy
strategies.

## State

A framework reset executed on 2026-08-23 per
[docs/superpowers/specs/2026-08-23-framework-reset-design.md](docs/superpowers/specs/2026-08-23-framework-reset-design.md):
every prior strategy (back_to_explore, paired_mm, bonereaper_v2, convex, and
the rest) was deleted, along with pm-engine, pm-copytrade, the nautilus tree,
and about half of the codebase that had accumulated as dead weight around a
healthy, equivalence-proven core.

`exo_fade` is retained, unmodified in behavior, only as the reference
implementation for Plan 2's engine extraction (walkforward.rs/runner.rs into
a new pm-backtest crate). Once that extraction's pinned-tape regression
passes, exo_fade is deleted too. **Zero deployable strategies is the
intended end state of this reset**: writing a new one is Plan 2+ work,
starting from a framework that cannot report the kind of numbers that
misled the June 2026 live cycle.

For the reasoning behind the reset, see
[docs/deep-review-2026-07-10.md](docs/deep-review-2026-07-10.md). For what
still needs deciding before Plan 2 starts, see
[docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md). For live/paper deployment
rules, see [docs/PROD.md](docs/PROD.md). The constraints-as-code doc
(`docs/CONSTRAINTS.md`) is itself a Plan 2 deliverable and does not exist yet.
