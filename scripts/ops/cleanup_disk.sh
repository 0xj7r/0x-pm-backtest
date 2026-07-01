#!/usr/bin/env bash
# Reclaim local disk without touching data/cache (re-downloadable from S3).
# Usage: ./scripts/ops/cleanup_disk.sh [--aggressive]
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "Before:"
df -g /System/Volumes/Data | tail -1

# Stop in-flight hunts (they write trade dumps).
pgrep -f "strategy_hunt_matrix" | xargs kill 2>/dev/null || true
pgrep -f "target/.*/pm-app alpha" | xargs kill 2>/dev/null || true

# Trade dumps (summaries in .json are kept).
find data/runs -name "*.trades.jsonl" -delete 2>/dev/null || true

# Large walk-forward per-market outputs (summaries kept).
find data/runs -name "markets.jsonl" -size +1M -delete 2>/dev/null || true

# Build artifacts (keep target/fast/pm-app for runs).
rm -rf target/debug target/doc target/release 2>/dev/null || true

# Regenerable research outputs.
rm -rf data/runs/whales data/runs/alpha/dir 2>/dev/null || true
find data/runs -name "*.log" -size +500k -delete 2>/dev/null || true

if [[ "${1:-}" == "--aggressive" ]]; then
  echo "AGGRESSIVE: pruning raw telonex cache older than 2026-02-01"
  echo "  (ticks cache preserved; re-fetch books from S3 if needed)"
  find data/cache/raw/telonex -mindepth 1 -maxdepth 3 -type d 2>/dev/null | while read -r d; do
    day=$(basename "$d" | rg -o 'date=[0-9-]+' | cut -d= -f2 || true)
    if [[ -n "$day" && "$day" < "2026-02-01" ]]; then
      rm -rf "$d"
    fi
  done
fi

echo "After:"
df -g /System/Volumes/Data | tail -1
du -sh data/cache data/runs target 2>/dev/null