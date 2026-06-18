#!/usr/bin/env bash
# Ingest Jun 8–17 BTC-5m books (Telonex API), binance spot/perp, and refresh manifest.
# Unblocks gate_reenable_compare JUN1017 and june_live_backtest parity runs.
set -euo pipefail
cd "$(dirname "$0")/.."

START="${START:-2026-06-08}"
END="${END:-2026-06-17}"
MARKETS_PQ="${MARKETS_PQ:-data/cache/telonex_markets.parquet}"
MANIFEST_DIR="${MANIFEST_DIR:-data/manifests/june2026_live}"
BIN="${BIN:-./target/release/pm-app}"

echo "== Telonex books+trades $START .. $END (API) =="
python3 scripts/telonex_fetch_range.py "$START" "$END" --up-only --markets-parquet "$MARKETS_PQ"

echo "== Binance spot BTCUSDT $START .. $END =="
python3 scripts/binance_spot_fetch.py BTCUSDT "$START" "$END"

echo "== Binance perp BTCUSDT $START .. $END =="
python3 scripts/binance_perp_fetch.py BTCUSDT "$START" "$END"

echo "== Markets parquet (for labeled manifest) =="
if [[ ! -f "$MARKETS_PQ" ]]; then
  curl -sL -o "$MARKETS_PQ" "https://api.telonex.io/v1/datasets/polymarket/markets"
fi

echo "== Build june2026_live manifest from parquet =="
mkdir -p "$MANIFEST_DIR"
python3 scripts/telonex_markets_to_manifests.py "$MARKETS_PQ" "$MANIFEST_DIR"

# Slice btc-5m Up rows for the live window and merge into canonical.
python3 - "$START" "$END" "$MANIFEST_DIR" <<'PY'
import json, sys
from pathlib import Path
start, end, mdir = sys.argv[1:4]
src = Path(mdir) / "btc-updown-5m_up.jsonl"
rows = []
for line in src.read_text().splitlines():
    if not line.strip():
        continue
    r = json.loads(line)
    if start <= r["date"] <= end:
        rows.append(r)
out = Path("data/manifests/june2026_live/markets_btc_jun8_17.jsonl")
out.write_text("".join(json.dumps(r) + "\n" for r in sorted(rows, key=lambda x: (x["close_ts"], x["asset_id"]))))
print(f"wrote {len(rows)} rows -> {out}")

canon = Path("data/manifests/canonical/btc-updown-5m_up.jsonl")
existing = {}
for line in canon.read_text().splitlines():
    if not line.strip():
        continue
    r = json.loads(line)
    existing[r["asset_id"]] = r
for r in rows:
    existing[r["asset_id"]] = r
merged = sorted(existing.values(), key=lambda x: (x["close_ts"], x["asset_id"]))
canon.write_text("".join(json.dumps(r) + "\n" for r in merged))
print(f"merged canonical btc-updown-5m_up.jsonl -> {len(merged)} rows")
PY

echo "== Per-day cache counts =="
python3 - "$START" "$END" <<'PY'
import sys
from pathlib import Path
start, end = sys.argv[1:3]
root = Path("data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25")
d = start
from datetime import date, timedelta
cur = date.fromisoformat(start)
stop = date.fromisoformat(end)
while cur <= stop:
    day = cur.isoformat()
    p = root / f"date={day}"
    n = len(list(p.glob("asset_id=*"))) if p.exists() else 0
    print(f"  {day}: {n} assets")
    cur += timedelta(days=1)
PY

echo "INGEST_JUNE_LIVE_DONE"