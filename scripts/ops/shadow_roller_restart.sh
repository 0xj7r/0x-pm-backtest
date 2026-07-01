#!/usr/bin/env bash
# Start shadow BTC 5m roller logger (split/dump/redeem would-events only).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOME="${HOME:-$(getent passwd "$(whoami)" 2>/dev/null | cut -d: -f6)}"
HOME="${HOME:-/home/ubuntu}"
OUT_DIR="${SHADOW_ROLLER_OUT:-$HOME/data/pm-alpha/shadow-roller}"
LOG="${SHADOW_ROLLER_LOG:-$HOME/data/pm-alpha/shadow-roller.log}"
PID_FILE="${SHADOW_ROLLER_PID:-$HOME/data/pm-alpha/shadow-roller.pid}"

CLIP_USD="${ROLLER_CLIP_USD:-1111}"
DUMP_CLIP="${ROLLER_DUMP_CLIP:-200}"

if pgrep -f "shadow_roller_logger.py" >/dev/null 2>&1; then
  pkill -INT -f "shadow_roller_logger.py" || true
  sleep 2
fi

mkdir -p "$OUT_DIR"
nohup python3 "$SCRIPT_DIR/shadow_roller_logger.py" \
  --out-dir "$OUT_DIR" \
  --clip-usd "$CLIP_USD" \
  --dump-clip-shares "$DUMP_CLIP" \
  >> "$LOG" 2>&1 &
echo $! > "$PID_FILE"
echo "shadow-roller pid=$(cat "$PID_FILE") clip=${CLIP_USD} dump_clip=${DUMP_CLIP}"