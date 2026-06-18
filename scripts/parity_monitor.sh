#!/usr/bin/env bash
# Cron-friendly LIVE↔REF parity wrapper with alert thresholds.
#
# Exit 0 when orphans/missed within limits; exit 1 on breach or compare failure.
# Optional Telegram via ~/.config/polymarket-watchdog/telegram.env (TG_TOKEN, TG_CHAT_ID).
set -euo pipefail

REPO="${REPO:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
DATA="${DATA:-$HOME/data/pm-alpha}"
OUT_DIR="${OUT_DIR:-$DATA/week_monitor}"
SHADOW_DIR="${SHADOW_DIR:-$DATA/shadow-final}"
LIVE_LOG="${LIVE_LOG:-$DATA/shadow_exec_tail.log}"
COMPARE="${COMPARE:-$REPO/scripts/compare_live_ref.py}"

SINCE_HOURS="${SINCE_HOURS:-4}"
MAX_ORPHANS="${MAX_ORPHANS:-0}"
MAX_MISSED="${MAX_MISSED:-0}"
ALERT_ORPHANS="${ALERT_ORPHANS:-1}"
ALERT_MISSED="${ALERT_MISSED:-1}"
TELEGRAM_ENV="${TELEGRAM_ENV:-$HOME/.config/polymarket-watchdog/telegram.env}"

mkdir -p "$OUT_DIR"
STAMP="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
LOG_FILE="$OUT_DIR/parity_monitor.log"

tg_send() {
  local msg="$1"
  local token="${TG_TOKEN:-}" chat="${TG_CHAT_ID:-}"
  [[ -n "$token" && -n "$chat" ]] || return 0
  curl -s "https://api.telegram.org/bot${token}/sendMessage" \
    -d "chat_id=${chat}" --data-urlencode "text=${msg}" >/dev/null 2>&1 || true
}

load_telegram() {
  if [[ -f "$TELEGRAM_ENV" ]]; then
    # shellcheck disable=SC1090
    set -a && source "$TELEGRAM_ENV" && set +a
  fi
}

if [[ ! -f "$COMPARE" ]]; then
  echo "[$STAMP] FAIL: compare script missing: $COMPARE" | tee -a "$LOG_FILE"
  exit 1
fi
if [[ ! -f "$LIVE_LOG" ]]; then
  echo "[$STAMP] FAIL: live log missing: $LIVE_LOG" | tee -a "$LOG_FILE"
  exit 1
fi

shopt -s nullglob
shadow_files=( "$SHADOW_DIR"/shadow-*.jsonl )
if [[ ${#shadow_files[@]} -eq 0 ]]; then
  echo "[$STAMP] FAIL: no shadow JSONL under $SHADOW_DIR" | tee -a "$LOG_FILE"
  exit 1
fi

combined="/tmp/shadow-final-combined.jsonl"
cat "${shadow_files[@]}" > "$combined"
DAY_UTC="$(date -u +%Y-%m-%d)"

load_telegram

set +e
out="$(python3 "$COMPARE" \
  --shadow "$combined" \
  --live-log "$LIVE_LOG" \
  --day "$DAY_UTC" \
  --since-hours "$SINCE_HOURS" 2>&1)"
rc=$?
set -e

orphans="$(printf '%s\n' "$out" | grep -Eo 'orphans=[0-9]+' | tail -1 | cut -d= -f2 || true)"
missed="$(printf '%s\n' "$out" | grep -Eo 'missed_ref=[0-9]+' | tail -1 | cut -d= -f2 || true)"
orphans="${orphans:-unknown}"
missed="${missed:-unknown}"

{
  echo "[$STAMP] parity_monitor since=${SINCE_HOURS}h max_orphans=$MAX_ORPHANS max_missed=$MAX_MISSED"
  printf '%s\n' "$out"
} | tee -a "$LOG_FILE"

breach=0
if [[ "$orphans" != "unknown" && "$orphans" -gt "$MAX_ORPHANS" ]]; then
  breach=1
  if [[ "$orphans" -ge "$ALERT_ORPHANS" ]]; then
    tg_send "parity ALERT: orphans=$orphans (max $MAX_ORPHANS) window=${SINCE_HOURS}h"
  fi
fi
if [[ "$missed" != "unknown" && "$missed" -gt "$MAX_MISSED" ]]; then
  breach=1
  if [[ "$missed" -ge "$ALERT_MISSED" ]]; then
    tg_send "parity ALERT: missed_ref=$missed (max $MAX_MISSED) window=${SINCE_HOURS}h"
  fi
fi

if [[ "$rc" -ne 0 || "$breach" -eq 1 ]]; then
  echo "[$STAMP] FAIL rc=$rc orphans=$orphans missed=$missed" | tee -a "$LOG_FILE"
  exit 1
fi

echo "[$STAMP] PASS orphans=$orphans missed=$missed" | tee -a "$LOG_FILE"
exit 0