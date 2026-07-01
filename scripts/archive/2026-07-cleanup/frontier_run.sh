#!/bin/bash
# Participation frontier sweep: per-threshold runs (one threshold per run so
# --trades-out covers every cell) over spot-only vs perp-led, BTC-5m and 15m.
# Tune window May 7-18 only.
set -euo pipefail

REPO=/Users/jackreid/go/polymarket-backtest
OUT=${1:?usage: frontier_run.sh <out_dir>}
mkdir -p "$OUT"
cd "$REPO"

THRESHOLDS="0.06 0.08 0.10 0.12 0.14 0.16 0.20"

run_cell() {
  local horizon=$1 state=$2 thr=$3
  local markets prefix perp_args=""
  if [ "$horizon" = "5m" ]; then
    markets=data/manifests/canonical/btc-updown-5m_up.jsonl
    prefix=btc-updown-5m-
  else
    markets=data/manifests/canonical/btc-updown-15m_up.jsonl
    prefix=btc-updown-15m-
  fi
  [ "$state" = "perp" ] && perp_args="--perp-symbol BTCUSDT --perp-price-weight 0.5"
  local tag="${horizon}_${state}_${thr/./}"
  if [ -s "$OUT/$tag.json" ]; then echo "skip $tag (exists)"; return; fi
  echo "=== $tag ==="
  ./target/fast/pm-app alpha \
    --local-cache-dir data/cache \
    --markets "$markets" --slug-prefix "$prefix" \
    --down-assets data/manifests/canonical/down_all.jsonl \
    --tick-cache-dir data/cache/ticks \
    --date-start 2026-05-07 --date-end 2026-05-18 \
    --latency-ms 150 --vol-lookback-s 3600 \
    --exit-after-s 30 --stop-before-close-s 90 \
    --edge-thresholds "$thr" $perp_args \
    --out-json "$OUT/$tag.json" \
    --trades-out "$OUT/$tag.trades.jsonl" 2>&1 | grep -E '^\s+150ms|alpha run:' || true
}

for state in spot perp; do
  for thr in $THRESHOLDS; do
    run_cell 5m "$state" "$thr"
  done
done
for state in spot perp; do
  for thr in $THRESHOLDS; do
    run_cell 15m "$state" "$thr"
  done
done
echo "ALL DONE"
