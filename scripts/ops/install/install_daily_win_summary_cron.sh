#!/usr/bin/env bash
# Schedule 08:00 local (Europe/Dublin) green-day summary (log only if NOTIFY=none).
set -euo pipefail

SCRIPT="${SCRIPT:-$HOME/pm-backtest/scripts/ops/shadow_daily_win_summary.py}"
MARKER="shadow-daily-win-summary"
TZ_NAME="${DAILY_SUMMARY_TZ:-Europe/Dublin}"
HOUR="${DAILY_SUMMARY_HOUR:-8}"
NOTIFY="${DAILY_SUMMARY_NOTIFY:-none}"

mkdir -p "$HOME/data/pm-alpha/week_monitor"

TMP=$(mktemp)
crontab -l 2>/dev/null | grep -v "shadow_daily_win_summary.py" | grep -v "$MARKER" > "$TMP" || true

# Cron uses server local time; Dublin box is typically UTC: pin via TZ= for BST/GMT.
cat >> "$TMP" <<EOF
# $MARKER: log on server; Mac relay sends WhatsApp on green days
0 ${HOUR} * * * TZ=${TZ_NAME} python3 ${SCRIPT} --notify ${NOTIFY} >> $HOME/data/pm-alpha/week_monitor/daily_win_cron.log 2>&1
EOF

crontab "$TMP"
rm -f "$TMP"

echo "Installed daily win summary at ${HOUR}:00 ${TZ_NAME}"
crontab -l | grep -F "$MARKER" || true