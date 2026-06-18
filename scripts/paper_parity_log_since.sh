#!/usr/bin/env bash
# Parity check using only log lines after paper executor restart (avoids pre-paper noise).
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
DATA="${DATA:-$HOME/data/pm-alpha}"
LIVE_LOG="${LIVE_LOG:-$DATA/shadow_exec_tail.log}"
SHADOW_DIR="${SHADOW_DIR:-$DATA/shadow-final}"
COMPARE="${COMPARE:-$REPO/scripts/compare_live_ref.py}"
SINCE_MARKER="${SINCE_MARKER:-shadow_exec_tail: JSONL consumer}"

MARKER_LINE="$(grep -n "$SINCE_MARKER" "$LIVE_LOG" 2>/dev/null | tail -1 | cut -d: -f1 || true)"
if [[ -z "${MARKER_LINE:-}" ]]; then
  echo "No paper restart marker in $LIVE_LOG" >&2
  exit 1
fi

TMP_LOG="$(mktemp)"
tail -n +"$MARKER_LINE" "$LIVE_LOG" > "$TMP_LOG"

shopt -s nullglob
combined="$(mktemp)"
cat "$SHADOW_DIR"/shadow-*.jsonl > "$combined"

python3 "$COMPARE" --shadow "$combined" --live-log "$TMP_LOG" --since-hours "${SINCE_HOURS:-0}"
rc=$?
rm -f "$TMP_LOG" "$combined"
exit $rc