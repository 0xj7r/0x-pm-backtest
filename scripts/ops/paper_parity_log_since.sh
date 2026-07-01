#!/usr/bin/env bash
# Parity check using only log lines after paper executor restart (avoids pre-paper noise).
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DATA="${DATA:-$HOME/data/pm-alpha}"
LIVE_LOG="${LIVE_LOG:-$DATA/shadow_exec_tail.log}"
SHADOW_DIR="${SHADOW_DIR:-$DATA/shadow-final}"
COMPARE="${COMPARE:-$REPO/scripts/ops/compare_live_ref.py}"
SINCE_MARKER="${SINCE_MARKER:-shadow_exec_tail: JSONL consumer}"

MARKER_LINE="$(grep -n "$SINCE_MARKER" "$LIVE_LOG" 2>/dev/null | tail -1 | cut -d: -f1 || true)"
if [[ -z "${MARKER_LINE:-}" ]]; then
  echo "No paper restart marker in $LIVE_LOG" >&2
  exit 1
fi

MARKER_TS="$(sed -n "${MARKER_LINE}p" "$LIVE_LOG" | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}' | head -1 || true)"
HOURS_SINCE=2
if [[ -n "${MARKER_TS:-}" ]]; then
  HOURS_SINCE="$(python3 -c "
from datetime import datetime, timezone
ts='${MARKER_TS}'
m=datetime.fromisoformat(ts.replace('Z','+00:00'))
if m.tzinfo is None:
    m=m.replace(tzinfo=timezone.utc)
now=datetime.now(timezone.utc)
print(max((now-m).total_seconds()/3600, 0.05))
")"
fi

TMP_LOG="$(mktemp)"
tail -n +"$MARKER_LINE" "$LIVE_LOG" > "$TMP_LOG"

shopt -s nullglob
combined="$(mktemp)"
cat "$SHADOW_DIR"/shadow-*.jsonl > "$combined"

echo "# Paper session since ${MARKER_TS:-line $MARKER_LINE} (${HOURS_SINCE}h window)"
python3 "$COMPARE" --shadow "$combined" --live-log "$TMP_LOG" --since-hours "$HOURS_SINCE"
rc=$?
rm -f "$TMP_LOG" "$combined"
exit $rc