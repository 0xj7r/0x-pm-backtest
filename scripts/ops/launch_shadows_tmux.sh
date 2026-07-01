#!/usr/bin/env bash
# Side-by-side REF (shadow-final JSONL) + LIVE/PAPER (shadow_exec_tail.log).
# Must run as ubuntu (not root) so SSH `tmux attach` finds the socket.
set -euo pipefail

if [[ "$(id -un)" == "root" ]]; then
  exec sudo -u ubuntu -H "$0" "$@"
fi

HOME="${HOME:-/home/ubuntu}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COLORIZE_REF="${COLORIZE_REF:-$HOME/colorize_shadow.py}"
COLORIZE_LIVE="${COLORIZE_LIVE:-$HOME/colorize_live_exec.py}"
[[ -x "$COLORIZE_REF" ]] || COLORIZE_REF="$SCRIPT_DIR/colorize_shadow.py"
[[ -x "$COLORIZE_LIVE" ]] || COLORIZE_LIVE="$SCRIPT_DIR/colorize_live_exec.py"

JSONL_DIR="${SHADOW_FINAL_OUT:-$HOME/data/pm-alpha/shadow-final}"
LIVE_LOG="${SHADOW_EXEC_LOG:-$HOME/data/pm-alpha/shadow_exec_tail.log}"
SESSION="${SHADOWS_TMUX_SESSION:-shadows}"

REF_JSONL="$(ls -t "$JSONL_DIR"/shadow-*.jsonl 2>/dev/null | head -1 || true)"
if [[ -z "${REF_JSONL:-}" ]]; then
  echo "no shadow JSONL in $JSONL_DIR" >&2
  exit 1
fi
if [[ ! -f "$LIVE_LOG" ]]; then
  echo "executor log missing: $LIVE_LOG" >&2
  exit 1
fi

if [[ -f "$HOME/fade.kill" ]]; then
  LIVE_LABEL="PAPER"
  LIVE_COLOR='\033[33m'
else
  LIVE_LABEL="LIVE"
  LIVE_COLOR='\033[32m'
fi

tmux kill-session -t "$SESSION" 2>/dev/null || true

tmux new-session -d -s "$SESSION" -n trade -x 230 -y 52 \
  "tail -n 40 -F '$LIVE_LOG' 2>/dev/null | python3 -u '$COLORIZE_LIVE' '$LIVE_COLOR' '$LIVE_LABEL'; exec bash"

tmux split-window -h -t "$SESSION:0" \
  "tail -n 40 -F '$REF_JSONL' 2>/dev/null | python3 -u '$COLORIZE_REF' '\033[36m' REF; exec bash"

tmux set-option -t "$SESSION" remain-on-exit on
tmux set-window-option -t "$SESSION:0" pane-border-status top
tmux set-option -p -t "$SESSION:0.0" pane-border-format " $LIVE_LABEL executor "
tmux set-option -p -t "$SESSION:0.1" pane-border-format ' REF shadow-final '
tmux select-pane -t "$SESSION:0.0"

echo "shadows session ready"
echo "  REF=$REF_JSONL"
echo "  $LIVE_LABEL=$LIVE_LOG"
echo "attach: tmux attach -t $SESSION"
tmux list-sessions 2>/dev/null | grep "$SESSION" || true