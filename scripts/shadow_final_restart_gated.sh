#!/usr/bin/env bash
# Restart shadow-final with SSOT decide gates (mom30 + min_entry_ask 0.45).
# shadow_exec_tail is untouched — it follows gated would_enter from JSONL.
set -euo pipefail

BIN="${SHADOW_FINAL_BIN:-$HOME/pm-backtest/target/release/pm-app}"
OUT_DIR="${SHADOW_FINAL_OUT:-$HOME/data/pm-alpha/shadow-final}"
LOG="${SHADOW_FINAL_LOG:-$HOME/data/pm-alpha/shadow-final.log}"
PID_FILE="${SHADOW_FINAL_PID:-$HOME/data/pm-alpha/shadow-final.pid}"

GATE_FLAGS=(
  --skip-spot-misalign-s 30
  --min-entry-ask 0.45
)

BASE_FLAGS=(
  --edge-threshold 0.12
  --perp-price-weight 0.75
  --exit-after-s 0
  --rearm-edge 0.08
  --max-clips 2
  --min-entry-sigma-bps 3.0
  --vol-estimator realized
  --vol-lookback-s 3600
  --skip-saturday
  --out-dir "$OUT_DIR"
)

if pgrep -f "pm-app shadow.*shadow-final" >/dev/null 2>&1; then
  pkill -INT -f "pm-app shadow.*shadow-final" || true
  sleep 3
  pkill -KILL -f "pm-app shadow.*shadow-final" 2>/dev/null || true
fi

mkdir -p "$OUT_DIR"
nohup "$BIN" shadow "${BASE_FLAGS[@]}" "${GATE_FLAGS[@]}" >> "$LOG" 2>&1 &
echo $! > "$PID_FILE"
echo "shadow-final pid=$(cat "$PID_FILE") gates=mom30,min_ask=0.45"
echo "warmup: vol3600 buffer needs ~1h before entries resume"