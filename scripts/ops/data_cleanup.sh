#!/usr/bin/env bash
# Safe local data cleanup: frees disk without breaking validation/backtest.
#
# What this removes (default):
#   1. raw/telonex partitions for dates that already have merged tick caches
#   2. raw/binance partitions outside the active May–June 2026 window
#   3. target/debug build artifacts
#
# Preserved:
#   - data/cache/ticks/* (all tick days)
#   - raw/telonex for dates WITHOUT ticks/ (e.g. 2026-05-29..31 if no ticks)
#   - raw/binance 2026-05-06 .. 2026-06-17
#   - data/manifests, data/golden, data/runs/june_gated_daily
#   - data/cache/telonex_markets.parquet
#
# Restore deleted raw from S3:
#   ./scripts/pipeline/prep_cache.sh 2026-05-29 2026-05-30 2026-05-31
#   ./scripts/pipeline/prep_cache.sh 2026-06-01 2026-06-07
#   ./target/release/pm-app prep-cache --cache-dir data/cache --markets <manifest> ...
#
# Usage:
#   DRY_RUN=1 ./scripts/ops/data_cleanup.sh
#   ./scripts/ops/data_cleanup.sh
#   ./scripts/ops/data_cleanup.sh --log-only
set -euo pipefail
cd "$(dirname "$0")/../.."

CACHE="${CACHE:-data/cache}"
LOG="${LOG:-data/runs/data_cleanup.log}"
DRY_RUN="${DRY_RUN:-0}"
DO_DEBUG="${DO_DEBUG:-1}"
DO_TELONEX="${DO_TELONEX:-1}"
DO_BINANCE="${DO_BINANCE:-1}"
BINANCE_KEEP_START="${BINANCE_KEEP_START:-2026-05-06}"
BINANCE_KEEP_END="${BINANCE_KEEP_END:-2026-06-17}"

if [[ "${1:-}" == "--log-only" ]]; then
  [[ -f "$LOG" ]] && cat "$LOG" || { echo "no log at $LOG"; exit 1; }
  exit 0
fi

mkdir -p "$(dirname "$LOG")"
exec > >(tee -a "$LOG") 2>&1
echo "=== data_cleanup.sh $(date '+%Y-%m-%dT%H:%M:%S%z') DRY_RUN=$DRY_RUN ==="
echo "disk before: $(df -h . | tail -1)"

freed_kb=0
removed_n=0

rm_path() {
  local p="$1"
  local reason="$2"
  [[ -e "$p" ]] || return 0
  local kb
  kb=$(du -sk "$p" 2>/dev/null | awk '{print $1}')
  if [[ "$DRY_RUN" == "1" ]]; then
    echo "DRY  would rm -rf $p  (${kb}K): $reason"
  else
    echo "DEL  rm -rf $p  (${kb}K): $reason"
    rm -rf "$p"
  fi
  freed_kb=$((freed_kb + kb))
  removed_n=$((removed_n + 1))
}

# --- 1. target/debug ---
if [[ "$DO_DEBUG" == "1" && -d target/debug ]]; then
  rm_path target/debug "cargo debug artifacts; rebuild with cargo build"
fi

# --- 2. raw telonex where ticks exist ---
if [[ "$DO_TELONEX" == "1" && -d "$CACHE/ticks" && -d "$CACHE/raw/telonex" ]]; then
  read -r tel_kb tel_n < <(python3 - "$CACHE" "$DRY_RUN" <<'PY'
import os, shutil, subprocess, sys

cache, dry = sys.argv[1:3]
tick_root = os.path.join(cache, "ticks")
ticks = sorted(
    n for n in os.listdir(tick_root)
    if os.path.isdir(os.path.join(tick_root, n))
)
freed_kb = 0
removed = 0
for day in ticks:
    for ch in ("book_snapshot_25", "trades"):
        p = os.path.join(
            cache, "raw/telonex/exchange=polymarket",
            f"channel={ch}", f"date={day}",
        )
        if not os.path.exists(p):
            continue
        kb = int(subprocess.check_output(["du", "-sk", p]).split()[0].decode())
        tag = "DRY " if dry == "1" else "DEL "
        print(f"{tag} rm -rf {p}  ({kb}K): tick cache exists for {day}", file=sys.stderr, flush=True)
        if dry != "1":
            shutil.rmtree(p)
        freed_kb += kb
        removed += 1
print(f"{freed_kb} {removed}")
PY
)
  freed_kb=$((freed_kb + tel_kb))
  removed_n=$((removed_n + tel_n))
  echo "kept raw/telonex for any date without data/cache/ticks/<date>/"
fi

# --- 3. old binance outside active window ---
if [[ "$DO_BINANCE" == "1" && -d "$CACHE/raw/binance" ]]; then
  while IFS= read -r day; do
    [[ -z "$day" ]] && continue
    while IFS= read -r p; do
      [[ -z "$p" ]] && continue
      rm_path "$p" "binance outside $BINANCE_KEEP_START..$BINANCE_KEEP_END"
    done < <(find "$CACHE/raw/binance" -type d -name "date=${day}" 2>/dev/null)
  done < <(python3 - "$CACHE" "$BINANCE_KEEP_START" "$BINANCE_KEEP_END" <<'PY'
import os, sys
from datetime import date, timedelta
cache, start_s, end_s = sys.argv[1:4]
start = date.fromisoformat(start_s)
end = date.fromisoformat(end_s)
keep = set()
d = start
while d <= end:
    keep.add(d.isoformat())
    d += timedelta(days=1)
days = set()
for root, dirs, files in os.walk(os.path.join(cache, "raw/binance")):
    for name in dirs:
        if name.startswith("date="):
            days.add(name[5:])
for day in sorted(days - keep):
    print(day)
PY
)
  echo "kept binance: $BINANCE_KEEP_START .. $BINANCE_KEEP_END"
fi

echo ""
echo "=== summary ==="
if [[ "$DRY_RUN" == "1" ]]; then
  echo "DRY RUN: no files deleted"
else
  freed_gb=$(awk "BEGIN {printf \"%.2f\", $freed_kb/1024/1024}")
  echo "freed: ${freed_gb} GB (${freed_kb} KB)"
  echo "paths removed: ${removed_n}"
fi
echo "disk after: $(df -h . | tail -1)"
echo "cache size: $(du -sh "$CACHE" 2>/dev/null | awk '{print $1}')"
echo "ticks days: $(ls "$CACHE/ticks" 2>/dev/null | wc -l | tr -d ' ')"
echo "log: $LOG"