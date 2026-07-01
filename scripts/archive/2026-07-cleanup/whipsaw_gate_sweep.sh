#!/usr/bin/env bash
# Counterfactual gate replay on trade JSONL (legacy Python validator).
#
# Primary SSOT backtest: ./scripts/whipsaw_backtest.sh (gates in pm_alpha::decide).
#
# Uses existing alpha harness output (VERIFY baseline) when present;
# otherwise runs one baseline backtest.
#
# Usage:
#   ./scripts/whipsaw_gate_sweep.sh
#   TRADES=data/runs/p-improvement/perp_w90.trades.jsonl ./scripts/whipsaw_gate_sweep.sh
set -euo pipefail
cd "$(dirname "$0")/.."

TRADES="${TRADES:-data/runs/open-entry/baseline.trades.jsonl}"
BIN="${BIN:-./target/fast/pm-app}"

if [[ ! -s "$TRADES" ]]; then
  echo "No trades at $TRADES — running VERIFY baseline..."
  mkdir -p data/runs/open-entry
  "$BIN" alpha \
    --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
    --local-cache-dir data/cache \
    --date-start 2026-05-07 --date-end 2026-05-18 \
    --edge-threshold 0.12 \
    --notional-usdc 50 \
    --perp-price-weight 0.75 \
    --latency-ms 250 \
    --trades-out "$TRADES" \
    > data/runs/open-entry/whipsaw_baseline_run.log 2>&1
fi

python3 scripts/whipsaw_gate_validate.py "$TRADES"