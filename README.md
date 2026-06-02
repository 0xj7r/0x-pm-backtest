# polymarket-backtest

Pure-Rust custom backtesting + paper/live framework for Polymarket BTC-5m markets.
The authoritative research path is the in-repo custom engine (`pm-app walk-forward`
→ `walkforward.rs` → `runner.rs` → `pm-strategy`), not Nautilus BacktestEngine.

## Workspace

```
crates/
├── pm-types/              # ReplayEvent, SpotTick/SpotHistory, TradeTick/TradeHistory, MarketId, BookLevel, PortfolioLimits
├── pm-telonex-loader/     # S3/local-cache streaming loaders
│   ├── book_snapshot       (Polymarket book_snapshot_25)
│   ├── polymarket_trades   (Polymarket trades channel — aggressor flow)
│   ├── polymarket_onchain  (Polymarket onchain_fills — whale_net_flow)
│   ├── binance_trades      (Binance agg_trades — BTC spot)
│   ├── nautilus_conv       (optional ReplayEvent → QuoteTick conversion)
│   └── s3                  (object_store-based S3 client)
├── pm-model/              # canonical 4-score model + online meta-calibrator
├── pm-strategy/           # strategy candidates + execution lanes
│   ├── signals             (direction/confidence/calibrated_p/risk scores)
│   ├── regime              (BtcRegime: Flat/Whipsaw/DirectionalSmooth/TrendingVolatile)
│   ├── back_to_explore     (BackToExplore paired-maker strategy)
│   ├── paired_mm           (PairedMmDense — unlawful-shear ladder)
│   ├── bonereaper_v2       (BonereaperV2 composite lanes)
│   └── archive             (legacy strategy implementations)
├── pm-risk/               # Kelly sizing + PortfolioState (drawdown, daily/per-market caps)
└── pm-app/                # CLI: discover-day | inspect-s3 | backtest-s3 | quotes-s3 | walk-forward
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

# OR discover from a local cached book partition without availability calls.
# This emits one canonical UP-token row per BTC 5m market and sets
# outcome=Unknown, so do not pass --use-outcome-label for these files.
./target/release/pm-app discover-local-cache-book-metadata \
    --cache-dir data/cache --date 2026-05-12 --out /tmp/markets.jsonl

# Run walk-forward across all active strategies
./target/release/pm-app walk-forward \\
    --markets /tmp/markets.jsonl \\
    --strategies "back_to_explore,paired_mm,bonereaper_v2" \\
    --starting-cash 100 --max-clip-usdc 5 --spot-symbol BTCUSDT \\
    --use-outcome-label \\
    --out-markets /tmp/wf.jsonl --out-summary /tmp/wf-summary.json
```

For full AWS portfolio runs, sizing grids, and checkpointed monitoring, see
[docs/aws-backtest-runbook.md](docs/aws-backtest-runbook.md).

If you only want one strategy, pass an explicit `--strategies` value (for example
`--strategies back_to_explore`) so legacy strategy knobs are not mixed into the
same manifest.

## Empirical findings

Current in-flight strategy focus is now:

- `back_to_explore`
- `paired_mm`
- `bonereaper_v2`

Recent active summaries and command history are tracked in:
- `docs/active_btc5m_experiments.md`
- `docs/autoresearch-ml-loop.md`

Strategy implementation history is archived in `crates/pm-strategy/src/archive`.
Non-active strategy profile experiments are archived under `configs/archive/2026-06-02`.

## Architecture notes

- **All data lives in S3** (`s3://pm-research-data-prod`, us-east-1). The binary never writes a local cache — deploys cleanly to AWS without filesystem mounting.
- **Maker + taker matcher** with configurable rebate (bps), fee (bps), and resting-order book that fills when the market crosses the limit.
- **Per-market exposure cap** prevents a single bad market from blowing up daily P&L. Critical: too tight a cap kills the edge ($15 → -$19; $50 → +$106).
- **Inventory-imbalance cancellation** in the runner cancels resting orders on the heavy side once `|yes_shares - no_shares|` exceeds threshold.
- **Markets dataset discovery** filters 1.4M-row Polymarket markets parquet to BTC-5m subset with `result_id` for resolved outcomes — 39,649 markets across 138 days available.

## Tests

```bash
cargo test --workspace
```

41 tests across the workspace, covering: signal math, regime classification, calibrated probability bounds, runner matcher (taker + maker), per-market exposure cap, whale-flow accounting, ring buffer stats.

## Status

The framework is a working research/backtest engine, but not production-live
complete yet:
- ✅ S3 streaming for book / trades / spot / onchain
- ✅ Maker matcher with rebates + per-market cap + inventory cancel
- ✅ Walk-forward harness (parallel pipelining)
- ✅ Markets-dataset discovery (138 days, 39k+ resolved markets)
- ✅ 4-score model + walk-forward online meta-calibrator
- ✅ Strategy candidates, including BonereaperV2 late/favourite lanes
- ✅ Optional QuoteTick conversion in loader crate
- ✅ AWS portfolio-grid launcher with checkpointed result uploads

Remaining for true production:
- Larger historical walk-forward validation across completed Telonex backfills
- Paper/live execution adapter with the same strategy/model path
- Robust nightly retraining and promotion gates
- Whale-flow features only available on dates ≤ 2026-04-28 (archive boundary)
