#!/usr/bin/env bash
# Restart shadow-final with SSOT decide gates (mom30 + min_entry_ask 0.45 + prod_gap_full).
# Regime stand-down flags removed 2026-07-01 (overfit; see shadow_final_gated_flags.sh).
# shadow_exec_tail is untouched: it follows would_enter from JSONL.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ops/shadow_final_gated_flags.sh
source "$SCRIPT_DIR/shadow_final_gated_flags.sh"

BIN="${SHADOW_FINAL_BIN:-$HOME/pm-backtest/target/release/pm-app}"
OUT_DIR="${SHADOW_FINAL_OUT:-$HOME/data/pm-alpha/shadow-final}"
LOG="${SHADOW_FINAL_LOG:-$HOME/data/pm-alpha/shadow-final.log}"
PID_FILE="${SHADOW_FINAL_PID:-$HOME/data/pm-alpha/shadow-final.pid}"

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
nohup "$BIN" shadow "${BASE_FLAGS[@]}" "${SHADOW_FINAL_GATED_FLAGS[@]}" >> "$LOG" 2>&1 &
echo $! > "$PID_FILE"
echo "shadow-final pid=$(cat "$PID_FILE") gates=mom30,min_ask=0.45,prod_gap_full"
echo "warmup: vol3600 buffer needs ~1h before entries resume"