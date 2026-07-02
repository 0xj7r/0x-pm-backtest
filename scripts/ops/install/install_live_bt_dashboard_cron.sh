#!/usr/bin/env bash
# Install 4-hourly live vs backtest dashboard on Dublin (alongside week_monitor).
set -euo pipefail

SCRIPT="${SCRIPT:-$HOME/pm-backtest/scripts/ops/live_vs_backtest_dashboard.py}"
BASELINE="${BASELINE_TSV:-$HOME/pm-backtest/data/runs/june_baseline_daily/daily.tsv}"
GATED="${GATED_TSV:-$HOME/pm-backtest/data/runs/june_gated_daily/daily.tsv}"
MARKER="live-bt-dashboard"
OUT="$HOME/data/pm-alpha/week_monitor"

mkdir -p "$OUT"

TMP=$(mktemp)
crontab -l 2>/dev/null | grep -v "live_vs_backtest_dashboard.py" | grep -v "$MARKER" > "$TMP" || true

FILL_AUDIT="${FILL_AUDIT_SCRIPT:-$HOME/pm-backtest/scripts/ops/fill_realization_audit.py}"

cat >> "$TMP" <<EOF
# $MARKER: rolling live vs backtest stats
17 */4 * * * python3 ${SCRIPT} --backtest-baseline-tsv ${BASELINE} --backtest-gated-tsv ${GATED} >> ${OUT}/live_bt_dashboard_cron.log 2>&1
47 */4 * * * python3 ${FILL_AUDIT} --since-hours 4 >> ${OUT}/fill_realization_cron.log 2>&1
EOF

crontab "$TMP"
rm -f "$TMP"

echo "Installed live vs backtest dashboard every 4h (offset :17)"
crontab -l | grep -F "$MARKER" || true