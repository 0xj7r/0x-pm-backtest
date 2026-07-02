#!/usr/bin/env bash
# Cron wrapper for shadow_week_monitor.py (observe-only week run).
set -euo pipefail

REPO="${REPO:-$HOME/pm-backtest}"
SCRIPT="${SCRIPT:-$REPO/scripts/ops/shadow_week_monitor.py}"
DATA="${DATA:-$HOME/data/pm-alpha}"

# Fallback script locations
if [[ ! -f "$SCRIPT" ]]; then
  SCRIPT="$HOME/pm-backtest/scripts/ops/shadow_week_monitor.py"
fi

export PYTHONUNBUFFERED=1
exec python3 "$SCRIPT" \
  --data-dir "$DATA" \
  --compare-script "${COMPARE_SCRIPT:-$HOME/pm-backtest/scripts/ops/compare_live_ref.py}" \
  "$@"