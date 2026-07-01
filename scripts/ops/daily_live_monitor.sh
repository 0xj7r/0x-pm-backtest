#!/usr/bin/env bash
# Daily live stack monitor: gated backfill (if needed), dashboard, parity, week_monitor.
#
# Intended for Dublin cron or manual run after market close UTC.
# Appends human logs + JSONL under ~/data/pm-alpha/week_monitor/.
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
DATA="${DATA:-$HOME/data/pm-alpha}"
MONITOR_DIR="${MONITOR_DIR:-$DATA/week_monitor}"
SHADOW_DIR="${SHADOW_DIR:-$DATA/shadow-final}"
LIVE_LOG="${LIVE_LOG:-$DATA/shadow_exec_tail.log}"
GATED_TSV="${GATED_TSV:-$REPO/data/runs/june_gated_daily/daily.tsv}"
COMPARE="${COMPARE:-$REPO/scripts/compare_live_ref.py}"
DASHBOARD="${DASHBOARD:-$REPO/scripts/live_vs_backtest_dashboard.py}"
PARITY_HOURS="${PARITY_HOURS:-24}"
SKIP_GATED="${SKIP_GATED:-0}"

mkdir -p "$MONITOR_DIR"
STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
LOG_FILE="$MONITOR_DIR/daily_live_monitor.log"
JSONL_FILE="$MONITOR_DIR/daily_live_monitor.jsonl"

log() {
  echo "[$STAMP] $*" | tee -a "$LOG_FILE"
}

maybe_run_gated_daily() {
  if [[ "$SKIP_GATED" == "1" ]]; then
    log "june_gated_daily: skipped (SKIP_GATED=1)"
    return 0
  fi
  local today
  today="$(date -u +%Y-%m-%d)"
  if [[ -f "$GATED_TSV" ]] && grep -q "^${today}" "$GATED_TSV" 2>/dev/null; then
    log "june_gated_daily: skip (today $today already in TSV)"
    return 0
  fi
  log "june_gated_daily: running (missing $today in $GATED_TSV)"
  SKIP_EXISTING=1 "$REPO/scripts/june_gated_daily.sh" >> "$MONITOR_DIR/gated_daily.log" 2>&1 || {
    log "june_gated_daily: WARN exit=$?"
  }
}

run_dashboard() {
  local baseline="/tmp/june_baseline_daily.tsv"
  local gated="/tmp/june_gated_daily.tsv"
  [[ -f "$REPO/data/runs/june_baseline_daily/daily.tsv" ]] && \
    cp "$REPO/data/runs/june_baseline_daily/daily.tsv" "$baseline" 2>/dev/null || true
  [[ -f "$GATED_TSV" ]] && cp "$GATED_TSV" "$gated" 2>/dev/null || true

  local out
  out="$(python3 "$DASHBOARD" \
    --backtest-baseline-tsv "$baseline" \
    --backtest-gated-tsv "$gated" \
    --out-dir "$MONITOR_DIR" 2>&1)" || true
  printf '%s\n\n' "$out" >> "$LOG_FILE"
  echo "$out"
}

run_parity() {
  if [[ ! -f "$COMPARE" || ! -f "$LIVE_LOG" ]]; then
    log "parity: skip (missing compare script or live log)"
    return 0
  fi
  shopt -s nullglob
  local shadow_files=( "$SHADOW_DIR"/shadow-*.jsonl )
  if [[ ${#shadow_files[@]} -eq 0 ]]; then
    log "parity: skip (no shadow JSONL)"
    return 0
  fi
  local combined="/tmp/shadow-final-combined.jsonl"
  cat "${shadow_files[@]}" > "$combined"
  local day_utc
  day_utc="$(date -u +%Y-%m-%d)"

  local out rc
  set +e
  out="$(python3 "$COMPARE" \
    --shadow "$combined" \
    --live-log "$LIVE_LOG" \
    --day "$day_utc" \
    --since-hours "$PARITY_HOURS" 2>&1)"
  rc=$?
  set -e
  printf '%s\n\n' "$out" >> "$LOG_FILE"
  echo "$out"
  log "parity: exit=$rc (window=${PARITY_HOURS}h)"
  return "$rc"
}

main() {
  log "=== daily_live_monitor start ==="
  maybe_run_gated_daily

  local dash_out parity_out parity_rc=0
  dash_out="$(run_dashboard)"
  parity_out="$(run_parity)" || parity_rc=$?

  python3 - "$STAMP" "$parity_rc" <<'PY' >> "$JSONL_FILE"
import json, sys
stamp, parity_rc = sys.argv[1], int(sys.argv[2])
print(json.dumps({
    "ts_utc": stamp,
    "event": "daily_live_monitor",
    "parity_exit_code": parity_rc,
}))
PY

  log "=== daily_live_monitor done parity_rc=$parity_rc ==="
  printf '%s\n' "$dash_out"
  printf '%s\n' "$parity_out"
  return "$parity_rc"
}

main "$@"