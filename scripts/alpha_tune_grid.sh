#!/usr/bin/env bash
# pm-alpha hunt: base-model tune grid on May 7-18 (latency x threshold x vol lookback).
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p data/runs/alpha/tune
for VL in 900 1800 3600; do
  ./target/fast/pm-app alpha \
    --markets data/manifests/may2026_focused/markets_btc.jsonl \
    --date-start "${DATE_START:-2026-05-07}" --date-end "${DATE_END:-2026-05-18}" \
    --local-cache-dir data/cache \
    --replay-event-cache-dir data/cache/replay_events \
    --latency-sweep --edge-thresholds 0.03,0.05,0.08,0.12 \
    --vol-lookback-s "$VL" \
    --out-json "data/runs/alpha/tune/${OUT_PREFIX:-base}_vol${VL}.json" \
    > "data/runs/alpha/tune/${OUT_PREFIX:-base}_vol${VL}.log" 2>&1
  echo "done vol=$VL"
done
echo ALL_DONE
