#!/usr/bin/env bash
# Fail-fast data quality gate before backtest claims.
# Usage: ./scripts/pipeline/data_gate_june.sh [DATE_START] [DATE_END]
set -euo pipefail
cd "$(dirname "$0")/../.."

START="${1:-2026-06-10}"
END="${2:-2026-06-16}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
BOOKS="${BOOKS:-data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25}"
BINANCE="${BINANCE:-data/cache/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT}"
TICKS="${TICKS:-data/cache/ticks}"

FAIL=0
python3 - "$START" "$END" "$MANIFEST" "$BOOKS" "$BINANCE" "$TICKS" <<'PY'
import json, sys
from datetime import date, timedelta
from pathlib import Path

start, end, manifest, books, binance, ticks = sys.argv[1:7]
cur = date.fromisoformat(start)
stop = date.fromisoformat(end)

up_by_day = {}
for line in Path(manifest).read_text().splitlines():
    if not line.strip():
        continue
    r = json.loads(line)
    up_by_day.setdefault(r["date"], set()).add(r["asset_id"])

fail = 0
print(f"DATA GATE {start} .. {end}")
print(f"{'date':12s} {'manifest':>8s} {'up_books':>8s} {'book_dirs':>9s} {'binance':>7s} {'ticks':>7s}  status")
while cur <= stop:
    day = cur.isoformat()
    up = up_by_day.get(day, set())
    bp = Path(books) / f"date={day}"
    have = {p.name.split("=", 1)[1] for p in bp.glob("asset_id=*")} if bp.exists() else set()
    up_cov = len(up & have)
    bn = Path(binance) / f"date={day}"
    bnc = bn.exists() and any(bn.glob("*.parquet"))
    td = Path(ticks) / day
    tnc = len(list(td.iterdir())) if td.exists() else 0
    issues = []
    if len(up) < 280 and day not in ("2026-06-06", "2026-06-13"):
        issues.append(f"manifest={len(up)}")
    if up and up_cov < len(up):
        issues.append(f"missing_up_books={len(up)-up_cov}")
    if not bnc:
        issues.append("no_binance")
    if tnc < 200 and day not in ("2026-06-06", "2026-06-13"):
        issues.append(f"ticks={tnc}")
    status = "OK" if not issues else "FAIL " + ",".join(issues)
    if issues:
        fail = 1
    print(f"{day:12s} {len(up):8d} {up_cov:8d} {len(have):9d} {'Y' if bnc else 'N':>7s} {tnc:7d}  {status}")
    cur += timedelta(days=1)
sys.exit(fail)
PY
FAIL=$?
exit "$FAIL"