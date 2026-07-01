#!/usr/bin/env bash
# Restart shadow_exec_tail only (preserves tail state + shadow-final buffers).
set -euo pipefail

BIN="${SHADOW_EXEC_BIN:-$HOME/deploy-main/polymarket-agent/target/release/shadow_exec_tail}"
WALLET_ENV="${WALLET_ENV:-$HOME/.config/polymarket-exec/wallet.env}"
SHADOW_ENV="${SHADOW_ENV:-$HOME/.config/polymarket-exec/shadow_exec_tail.env}"
LOG="${SHADOW_EXEC_LOG:-$HOME/data/pm-alpha/shadow_exec_tail.log}"
PID_FILE="${SHADOW_EXEC_PID:-$HOME/data/pm-alpha/shadow_exec_tail.pid}"

if [[ -f "$HOME/fade.kill" ]]; then
  echo "kill switch active — not restarting executor" >&2
  exit 1
fi

STATE="${PM_SHADOW_TAIL_STATE_PATH:-$HOME/data/pm-alpha/shadow_exec_tail_state.json}"
JSONL_DIR="${PM_SHADOW_JSONL_PATH:-$HOME/data/pm-alpha/shadow-final}"

if pgrep -f "target/release/shadow_exec_tail" >/dev/null 2>&1; then
  pkill -TERM -f "target/release/shadow_exec_tail" || true
  sleep 2
  pkill -KILL -f "target/release/shadow_exec_tail" 2>/dev/null || true
fi

# After a long pause (fade.kill), seek tail to EOF so we never replay stale would_enter.
if [[ -d "$JSONL_DIR" ]]; then
  LATEST=$(ls -t "$JSONL_DIR"/shadow-*.jsonl 2>/dev/null | head -1 || true)
  if [[ -n "${LATEST:-}" && -f "$LATEST" ]]; then
    OFF=$(wc -c < "$LATEST" | tr -d ' ')
    mkdir -p "$(dirname "$STATE")"
    printf '{"path":"%s","offset":%s}\n' "$LATEST" "$OFF" > "$STATE"
    echo "tail state -> EOF $LATEST offset=$OFF"
  fi
fi

set -a
# shellcheck disable=SC1090
source "$WALLET_ENV"
# shellcheck disable=SC1090
source "$SHADOW_ENV"
set +a

nohup "$BIN" >> "$LOG" 2>&1 &
echo $! > "$PID_FILE"
echo "shadow_exec_tail pid=$(cat "$PID_FILE") clip=${PM_SHADOW_CLIP_USD:-?}"