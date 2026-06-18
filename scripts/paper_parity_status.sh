#!/usr/bin/env bash
# Quick paper-parity health check (Dublin or local).
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
DATA="${DATA:-$HOME/data/pm-alpha}"
SINCE_HOURS="${SINCE_HOURS:-2}"

echo "=== Paper parity status (last ${SINCE_HOURS}h) ==="
echo "fade.kill: $([[ -f "$HOME/fade.kill" ]] && echo ACTIVE || echo absent)"
pgrep -af "shadow_exec_tail" || echo "shadow_exec_tail: NOT RUNNING"
pgrep -af "pm-app shadow.*shadow-final" | head -1 || echo "shadow-final: NOT RUNNING"
echo ""

REPO="$REPO" SINCE_HOURS="$SINCE_HOURS" MAX_ORPHANS=0 MAX_MISSED=2 \
  "$REPO/scripts/parity_monitor.sh" || true

echo ""
echo "Recent parity stats from executor log:"
grep -E "parity_stats|SUBMITTED \(paper\)|kill-switch" \
  "${DATA}/shadow_exec_tail.log" 2>/dev/null | tail -8 || true