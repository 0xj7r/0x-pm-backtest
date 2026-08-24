# polymarket-backtest

Pure-Rust backtesting framework for Polymarket BTC/ETH updown markets. Runs
cloud-first against Telonex/Binance data mirrored to S3 (no local data
dependency for the core engine), and is built so that the framework itself
enforces the constraints that past live losses violated (truthful latency,
fee-net accounting, jittered-replay validation), rather than relying on
discipline at run time.

**There are no deployable strategies, by design.** The framework is the
deliverable; see [State](#state).

## Workspace

```
crates/
├── pm-types/            # market/tape/spot types (ReplayEvent, SpotHistory, TradeHistory, MarketId)
├── pm-risk/              # Kelly/fractional sizing, PortfolioState (drawdown, daily/per-market caps)
├── pm-telonex-loader/    # S3/local-cache streaming loaders (book, trades, onchain, Binance); nautilus-free
├── pm-model/             # canonical 4-score model + online meta-calibrator (engine research layer)
├── pm-alpha/             # belief (BSM digital), vol, fee curve, ExoState features, decide_entry SSOT
├── pm-strategy/          # Strategy trait + NoopStrategy + the test-only golden fixture
├── pm-backtest/          # the engine: fills, accounting, walk-forward, portfolio, scorecard
├── pm-shadow/            # live-twin log-only engine (JSONL stream, executor tail seam)
└── pm-app/               # CLI: shadow | alpha | walk-forward | discover-* | prep-cache | summarize-markets
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

# Run walk-forward. `noop` is the default and emits no orders: it exercises
# the loader, fill engine and accounting without taking a position.
./target/release/pm-app walk-forward \
    --markets /tmp/markets.jsonl \
    --strategies noop \
    --starting-cash 100 --max-clip-usdc 5 --spot-symbol BTCUSDT \
    --use-outcome-label \
    --out-markets /tmp/wf.jsonl --out-summary /tmp/wf-summary.json
```

`--strategies` accepts `noop` and `fixture`. Neither is deployable. `fixture`
is a deterministic test-only strategy that anchors the golden replay gate and
is rejected unless `--allow-fixture` is also passed; it exists so the gate
covers the order/fill/settlement path, not so anyone trades it.

## State

A framework reset executed on 2026-08-23 per
[docs/superpowers/specs/2026-08-23-framework-reset-design.md](docs/superpowers/specs/2026-08-23-framework-reset-design.md):
every prior strategy (back_to_explore, paired_mm, bonereaper_v2, convex, and
the rest) was deleted, along with pm-engine, pm-copytrade, the nautilus tree,
and about half of the codebase that had accumulated as dead weight around a
healthy, equivalence-proven core.

Phase 2 completed the reset on 2026-08-24: the engine was extracted into
`pm-backtest`, and then `exo_fade` and `mayjune_fade` (the last strategies)
were deleted. **Zero deployable strategies is the intended end state**, not an
unfinished migration. Writing a new one starts from a framework that cannot
report the kind of numbers that misled the June 2026 live cycle.

What guards the engine now is the pinned-tape golden replay: a fixed day
(2026-06-25, 288 markets) replayed through the test-only `fixture` strategy
and hashed. Run `bash scripts/research/golden_replay.sh check`; it must print
`GOLDEN: IDENTICAL`. See [tests/golden/README.md](tests/golden/README.md) for
what is hashed and why the hash must not be re-recorded to make a refactor
pass.

The `exo_fade_equivalence` GATE B binary (which proved the backtest and live
paths built byte-identical decision inputs for the fade) retired with the
fade. The PATTERN is the standing lesson from the June failure and should be
rebuilt for whatever strategy comes next: prove the two paths BUILD identical
decision inputs, not merely that fill rates look similar.

For the reasoning behind the reset, see
[docs/deep-review-2026-07-10.md](docs/deep-review-2026-07-10.md). For what
still needs deciding before Plan 2 starts, see
[docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md). For deployment rules, see
[docs/PROD.md](docs/PROD.md). For the ten constraints the
framework enforces as code, not discipline, see
[docs/CONSTRAINTS.md](docs/CONSTRAINTS.md).
