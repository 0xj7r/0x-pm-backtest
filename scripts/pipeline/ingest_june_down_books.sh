#!/usr/bin/env bash
# Fetch Down+Up Telonex books for Jun 8–16 (fixes up-only gap from ingest_june_live.sh).
set -euo pipefail
cd "$(dirname "$0")/.."

START="${START:-2026-06-08}"
END="${END:-2026-06-16}"
MARKETS_PQ="${MARKETS_PQ:-data/cache/telonex_markets.parquet}"

echo "== Telonex books+trades BOTH legs $START .. $END =="
python3 scripts/telonex_fetch_range.py "$START" "$END" --markets-parquet "$MARKETS_PQ"

echo "== Per-day book counts =="
python3 - "$START" "$END" <<'PY'
import sys
from pathlib import Path
from datetime import date, timedelta
start, end = sys.argv[1:3]
root = Path("data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25")
cur = date.fromisoformat(start)
stop = date.fromisoformat(end)
while cur <= stop:
    day = cur.isoformat()
    p = root / f"date={day}"
    n = len(list(p.glob("asset_id=*"))) if p.exists() else 0
    print(f"  {day}: {n} assets")
    cur += timedelta(days=1)
PY

echo "INGEST_JUNE_DOWN_DONE"