#!/usr/bin/env bash
# pm-alpha hunt: base-model tune grid on May 7-18 (latency x threshold x vol lookback).
# NB: no --replay-event-cache-dir — the JSONL cache bloats to ~70GB over May.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p data/runs/alpha/tune
pids=()
for VL in 900 1800 3600; do
  ./target/fast/pm-app alpha \
    --markets data/manifests/may2026_focused/markets_btc.jsonl \
    --date-start "${DATE_START:-2026-05-07}" --date-end "${DATE_END:-2026-05-18}" \
    --local-cache-dir data/cache \
    --latency-sweep --edge-thresholds 0.03,0.05,0.08,0.12 \
    --vol-lookback-s "$VL" \
    --out-json "data/runs/alpha/tune/${OUT_PREFIX:-base}_vol${VL}.json" \
    > "data/runs/alpha/tune/${OUT_PREFIX:-base}_vol${VL}.log" 2>&1 &
  pids+=($!)
done
fail=0
for pid in "${pids[@]}"; do
  wait "$pid" || fail=1
done
[ "$fail" = 0 ] && echo ALL_DONE || { echo SOME_FAILED; exit 1; }
