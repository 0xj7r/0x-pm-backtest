#!/usr/bin/env bash
# Install hands-off week monitor cron on Dublin. Disables night clip scaler.
set -euo pipefail

MONITOR="${MONITOR:-$HOME/pm-backtest/scripts/ops/shadow_week_monitor.sh}"
NIGHT_MARKER="shadow-night-monitor DISABLED"
WEEK_MARKER="shadow-week-monitor"

mkdir -p "$HOME/data/pm-alpha/week_monitor"

TMP=$(mktemp)
crontab -l 2>/dev/null \
  | grep -v "shadow_night_monitor.py" \
  | grep -v "shadow_week_monitor.sh" \
  | grep -v "$NIGHT_MARKER" \
  > "$TMP" || true

cat >> "$TMP" <<EOF
# $NIGHT_MARKER for hands-off week: re-enable manually
0 */4 * * * $MONITOR >> $HOME/data/pm-alpha/week_monitor/cron.log 2>&1  # $WEEK_MARKER
EOF

crontab "$TMP"
rm -f "$TMP"

echo "Installed crontab:"
crontab -l
echo ""
echo "Week monitor: every 4h -> ~/data/pm-alpha/week_monitor/"
echo "Wallet guard unchanged (every 2m, floor \$500)"
echo "Night clip scaler DISABLED"