#!/usr/bin/env bash
# Deterministic 1800 vs 3600 realized-vol lookback comparison, frozen fade config,
# across the three standing windows. Aggregate JSON only (no per-trade files).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/release/pm-app
OUT=/tmp/lookback_cmp
mkdir -p "$OUT"

COMMON="--local-cache-dir data/cache \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m- \
  --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks \
  --latency-ms 150 --stop-before-close-s 90 --fee-curve-rate 0.07 \
  --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --edge-thresholds 0.12 --exit-after-s 0 --rearm-edge 0.08 --max-clips 2 \
  --min-marginal-edge 0.04 --vol-estimator realized"

run() { # label start end lookback
  echo "[$(date -u +%H:%M:%S)] running $1 (lb=$4) $2..$3"
  $BIN alpha $COMMON --vol-lookback-s "$4" --date-start "$2" --date-end "$3" \
    --out-json "$OUT/$1.json" > "$OUT/$1.log" 2>&1
  echo "[$(date -u +%H:%M:%S)] done $1"
}

# W3 first (fail-fast triage), then W2, then W1 (longest).
run w3_lb1800 2026-05-07 2026-05-18 1800
run w3_lb3600 2026-05-07 2026-05-18 3600
run w2_lb1800 2026-04-01 2026-04-30 1800
run w2_lb3600 2026-04-01 2026-04-30 3600
run w1_lb1800 2026-02-12 2026-03-31 1800
run w1_lb3600 2026-02-12 2026-03-31 3600
echo "ALL_DONE"
