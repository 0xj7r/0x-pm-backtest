#!/usr/bin/env bash
# Gated mom30_ask045 daily P&L for June — base + execution stress rows per day.
#
# Variants per day:
#   base    — optimistic book (depth_capture=1.0)
#   depth25 — 25% depth capture (live competition stress)
#   stress  — depth25 + skip_touch_level (lose the race at touch)
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/june_gated_daily}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
DATE_START="${DATE_START:-2026-06-01}"
DATE_END="${DATE_END:-2026-06-16}"
SKIP_EXISTING="${SKIP_EXISTING:-1}"
mkdir -p "$OUT"

if [[ ! -x "$BIN" ]]; then
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
  --skip-spot-misalign-s 30
  --min-entry-ask 0.45
)

daily="$OUT/daily.tsv"
python3 - "$daily" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.is_file():
    path.write_text("date\tvariant\tn_trades\tnet_usd\thit_pct\tn_markets_run\n")
    raise SystemExit(0)
lines = path.read_text().splitlines()
if not lines:
    path.write_text("date\tvariant\tn_trades\tnet_usd\thit_pct\tn_markets_run\n")
    raise SystemExit(0)
if "variant" in lines[0]:
    raise SystemExit(0)
# Migrate legacy 5-column TSV → variant=base
out = ["date\tvariant\tn_trades\tnet_usd\thit_pct\tn_markets_run"]
for line in lines[1:]:
    if not line.strip():
        continue
    d, n, net, hit, mkts = line.split("\t")
    out.append(f"{d}\tbase\t{n}\t{net}\t{hit}\t{mkts}")
path.write_text("\n".join(out) + "\n")
print(f"Migrated legacy {path} → variant=base rows")
PY

append_row() {
  python3 - "$@" <<'PY'
import json, sys
from pathlib import Path

path, day, variant, tsv = sys.argv[1:5]
r = json.load(open(path))
cell = (r.get("sweep") or [r])[0]
agg = cell.get("report", {}).get("aggregate") or {}
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", agg.get("net_pnl_usd", 0)))
wins = int(agg.get("n_wins", 0))
hit = 100.0 * wins / n if n else 0.0
n_mkts = int(cell.get("n_markets_run", agg.get("n_markets_run", 0)))

lines = Path(tsv).read_text().splitlines()
header, body = lines[0], lines[1:]
body = [ln for ln in body if ln.strip() and not ln.startswith(f"{day}\t{variant}\t")]
body.append(f"{day}\t{variant}\t{n}\t{net:.2f}\t{hit:.1f}\t{n_mkts}")
Path(tsv).write_text(header + "\n" + "\n".join(body) + "\n")
print(f"  {day} {variant}: n={n} NET=${net:+,.0f} hit={hit:.1f}%")
PY
}

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

echo "June gated days: ${DATES[*]}"

run_variant() {
  local day=$1
  local variant=$2
  shift 2
  local extra_flags=("$@")
  local json="$OUT/${day}_gated_${variant}.json"
  local log="$OUT/${day}_gated_${variant}.log"
  if [[ "$variant" == "base" && ! -s "$json" && -s "$OUT/${day}_gated.json" ]]; then
    cp "$OUT/${day}_gated.json" "$json"
  fi
  if [[ "$SKIP_EXISTING" == "1" && -s "$json" ]]; then
    echo "== skip $day $variant (exists) =="
    append_row "$json" "$day" "$variant" "$daily"
    return 0
  fi
  echo "== gated $day $variant =="
  "$BIN" alpha "${COMMON[@]}" "${extra_flags[@]}" \
    --date-start "$day" --date-end "$day" --out-json "$json" > "$log" 2>&1
  append_row "$json" "$day" "$variant" "$daily"
}

for day in "${DATES[@]}"; do
  run_variant "$day" base
  run_variant "$day" depth25 --depth-capture-frac 0.25
  run_variant "$day" stress --depth-capture-frac 0.25 --skip-touch-level
done

python3 - "$daily" <<'PY'
import sys
from collections import defaultdict
from pathlib import Path

rows = []
for line in Path(sys.argv[1]).read_text().splitlines()[1:]:
    if not line.strip():
        continue
    parts = line.split("\t")
    if len(parts) == 5:
        d, n, net, hit, mkts = parts
        rows.append((d, "base", int(n), float(net), float(hit), int(mkts)))
    elif len(parts) >= 6:
        d, var, n, net, hit, mkts = parts[:6]
        rows.append((d, var, int(n), float(net), float(hit), int(mkts)))

by_var = defaultdict(list)
for r in rows:
    by_var[r[1]].append(r)

print(f"{'variant':<8} {'days':>5} {'trades':>7} {'NET':>12} {'hit%':>7} {'$/tr':>8} {'$/day':>8}")
print("-" * 62)
for var in ("base", "depth25", "stress"):
    rs = by_var.get(var, [])
    if not rs:
        continue
    n = sum(r[2] for r in rs)
    net = sum(r[3] for r in rs)
    hit = sum(r[4] * r[2] for r in rs) / n if n else 0
    days = len(rs)
    print(
        f"{var:<8} {days:>5} {n:>7} ${net:>10,.0f} {hit:>6.1f}% "
        f"${net/n if n else 0:>+7.2f} ${net/days if days else 0:>+7,.0f}"
    )
PY

echo ""
column -t "$daily" 2>/dev/null || cat "$daily"