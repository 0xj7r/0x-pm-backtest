#!/usr/bin/env bash
# Launch color-coded tmux session for all shadow-final WS feed telemetry.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COLORIZE="${COLORIZE_FEEDS:-$SCRIPT_DIR/colorize_feeds.py}"
JSONL_DIR="${SHADOW_FINAL_OUT:-$HOME/data/pm-alpha/shadow-final}"
ENGINE_LOG="${SHADOW_FINAL_LOG:-$HOME/data/pm-alpha/shadow-final.log}"
SESSION="${FEEDS_TMUX_SESSION:-wsfeeds}"

REF_JSONL="$(ls -t "$JSONL_DIR"/shadow-*.jsonl 2>/dev/null | head -1 || true)"
if [[ -z "${REF_JSONL:-}" ]]; then
  echo "no shadow JSONL in $JSONL_DIR" >&2
  exit 1
fi
if [[ ! -f "$ENGINE_LOG" ]]; then
  echo "engine log missing: $ENGINE_LOG" >&2
  exit 1
fi

feed_cmd() {
  local mode=$1
  printf "tail -n 20 -F '%s' '%s' 2>/dev/null | python3 -u '%s' %s; exec bash" \
    "$REF_JSONL" "$ENGINE_LOG" "$COLORIZE" "$mode"
}

tmux kill-session -t "$SESSION" 2>/dev/null || true

tmux new-session -d -s "$SESSION" -n feeds -x 220 -y 60 "$(feed_cmd summary)"
tmux split-window -h -t "$SESSION:0" "$(feed_cmd binance)"
tmux select-pane -t "$SESSION:0.0"
tmux split-window -v -t "$SESSION:0.0" "$(feed_cmd perp)"
tmux select-pane -t "$SESSION:0.1"
tmux split-window -v -t "$SESSION:0.1" "$(feed_cmd book)"
tmux select-pane -t "$SESSION:0.2"
tmux split-window -v -t "$SESSION:0.2" "$(feed_cmd kraken)"
tmux select-pane -t "$SESSION:0.3"
tmux split-window -v -t "$SESSION:0.3" "$(feed_cmd coinbase)"

tmux set-option -t "$SESSION" remain-on-exit on
tmux set-window-option -t "$SESSION:0" pane-border-status top
tmux set-pane-option -t "$SESSION:0.0" pane-border-format ' SUMMARY (spot+book ages) '
tmux set-pane-option -t "$SESSION:0.1" pane-border-format ' BINANCE spot WS '
tmux set-pane-option -t "$SESSION:0.2" pane-border-format ' PERP futures WS '
tmux set-pane-option -t "$SESSION:0.3" pane-border-format ' POLYMARKET book WS '
tmux set-pane-option -t "$SESSION:0.4" pane-border-format ' KRAKEN lead-lag '
tmux set-pane-option -t "$SESSION:0.5" pane-border-format ' COINBASE lead-lag '
tmux select-pane -t "$SESSION:0.0"

echo "wsfeeds session ready"
echo "  JSONL=$REF_JSONL"
echo "  LOG=$ENGINE_LOG"
echo "attach: tmux attach -t $SESSION"
tmux list-sessions 2>/dev/null | grep "$SESSION" || true