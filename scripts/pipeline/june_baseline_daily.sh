#!/usr/bin/env bash
# Ungated shadow-final parity: daily P&L for June (pre-gate live config).
# Runs every calendar day in [DATE_START, DATE_END] that has manifest rows.
set -euo pipefail
cd "$(dirname "$0")/../.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/june_baseline_daily}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
DATE_START="${DATE_START:-2026-06-01}"
DATE_END="${DATE_END:-2026-06-17}"
mkdir -p "$OUT"

if [[ ! -x "$BIN" ]]; then
  echo "Building $BIN ..."
  cargo build -p pm-app --release
fi

COMMON=(
  --markets "$MANIFEST"
  --local-cache-dir data/cache
  --tick-cache-dir data/cache/ticks
  --exit-after-s 0
  --perp-symbol BTCUSDT
  --perp-price-weight 0.75
  --vol-estimator realized
  --vol-lookback-s 3600
  --edge-thresholds 0.12
  --notional-usdc 50
  --latency-ms 250
  --max-clips 2
  --rearm-edge 0.08
  --clip-cooldown-ms 5000
  --min-entry-sigma-bps 3
  --skip-saturday
  --stop-before-close-s 90
  --min-marginal-edge 0.04
  --fee-curve-rate 0.07
)

daily="$OUT/daily.tsv"
echo -e "date\tn_trades\tnet_usd\thit_pct\tn_markets_run" > "$daily"

DATES=()
while IFS= read -r day; do
  DATES+=("$day")
done < <(python3 - "$MANIFEST" "$DATE_START" "$DATE_END" <<'PY'
import json, sys
from datetime import date, timedelta
manifest, start, end = sys.argv[1:4]
cur = date.fromisoformat(start)
stop = date.fromisoformat(end)
want = set()
while cur <= stop:
    want.add(cur.isoformat())
    cur += timedelta(days=1)
days = set()
for line in open(manifest):
    if not line.strip():
        continue
    r = json.loads(line)
    d = r.get("date", "")
    if d in want:
        days.add(d)
for d in sorted(days):
    print(d)
PY
)

if [[ ${#DATES[@]} -eq 0 ]]; then
  echo "No manifest rows for $DATE_START .. $DATE_END" >&2
  exit 1
fi
echo "June baseline days: ${DATES[*]}"

for day in "${DATES[@]}"; do
  echo "== baseline $day =="
  json="$OUT/${day}_baseline.json"
  log="$OUT/${day}_baseline.log"
  "$BIN" alpha "${COMMON[@]}" --date-start "$day" --date-end "$day" --out-json "$json" > "$log" 2>&1
  python3 - "$json" "$day" "$daily" <<'PY'
import json, sys
path, day, tsv = sys.argv[1:4]
r = json.load(open(path))
cell = (r.get("sweep") or [r])[0]
agg = cell.get("report", {}).get("aggregate") or {}
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", agg.get("net_pnl_usd", 0)))
wins = int(agg.get("n_wins", 0))
hit = 100.0 * wins / n if n else 0.0
n_mkts = int(cell.get("n_markets_run", agg.get("n_markets_run", 0)))
open(tsv, "a").write(f"{day}\t{n}\t{net:.2f}\t{hit:.1f}\t{n_mkts}\n")
print(f"  {day} n={n} NET=${net:+,.0f} hit={hit:.1f}% mkts={n_mkts}")
PY
done

echo ""
python3 - "$daily" <<'PY'
import sys
from pathlib import Path
rows = []
for line in Path(sys.argv[1]).read_text().splitlines()[1:]:
    if not line.strip():
        continue
    d, n, net, hit, mkts = line.split("\t")
    rows.append((d, int(n), float(net), float(hit), int(mkts)))
tot_n = sum(r[1] for r in rows)
tot_net = sum(r[2] for r in rows)
wavg_hit = sum(r[3]*r[1] for r in rows)/tot_n if tot_n else 0
print(f"TOTAL\t{tot_n}\t{tot_net:.2f}\t{wavg_hit:.1f}\t")
PY
column -t "$daily" 2>/dev/null || cat "$daily"