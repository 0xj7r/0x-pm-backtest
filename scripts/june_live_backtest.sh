#!/usr/bin/env bash
# Live-parity June backtest with per-day P&L breakdown.
#
# Uses tick-cache days present under data/cache/ticks (Jun 8–9 missing locally;
# Jun 11+ not in manifest yet).
#
# Usage:
#   ./scripts/june_live_backtest.sh
#   VARIANT=rearm70 ./scripts/june_live_backtest.sh
#   VARIANT=baseline,rearm70 ./scripts/june_live_backtest.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/june_live_parity}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"
TICKS="${TICKS:-data/cache/ticks}"
VARIANT="${VARIANT:-baseline}"

mkdir -p "$OUT"

# Shadow-final / live parity (hold-to-redemption fade).
COMMON=(
  --markets "$MANIFEST"
  --local-cache-dir "$CACHE"
  --tick-cache-dir "$TICKS"
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

gate_flags() {
  case "$1" in
    baseline) ;;
    rearm70)  echo --max-rearm-entry-ask 0.70 ;;
    flip)     echo --skip-expanded-high-flip ;;
    *) echo "unknown variant $1" >&2; exit 1 ;;
  esac
}

if [[ ! -x "$BIN" ]]; then
  echo "Building $BIN ..."
  cargo build -p pm-app --release
fi

DATES=()
while IFS= read -r day; do
  DATES+=("$day")
done < <(find "$TICKS" -maxdepth 1 -mindepth 1 -type d -name '2026-06-*' -exec basename {} \; | sort)
if [[ ${#DATES[@]} -eq 0 ]]; then
  echo "No June tick-cache days under $TICKS" >&2
  exit 1
fi

echo "June tick-cache days: ${DATES[*]}"
# Manifest may list dates without tick cache (e.g. 2026-06-08/09 after partial S3 sync).
python3 - <<'PY'
import json
from pathlib import Path
manifest = Path("data/manifests/canonical/btc-updown-5m_up.jsonl")
ticks = Path("data/cache/ticks")
manifest_days = sorted({json.loads(l)["date"] for l in manifest.read_text().splitlines() if l.strip() and json.loads(l)["date"].startswith("2026-06")})
cached = {p.name for p in ticks.iterdir() if p.is_dir() and p.name.startswith("2026-06")}
missing = [d for d in manifest_days if d not in cached]
if missing:
    print(f"manifest-only (need S3 sync + rerun): {' '.join(missing)}")
if manifest_days and manifest_days[-1] < "2026-06-16":
    print(f"note: manifest ends at {manifest_days[-1]} — Jun 11+ needs manifest refresh")
PY
echo ""

IFS=',' read -ra VARS <<< "${VARIANT// /,}"
summary="$OUT/summary.tsv"
daily="$OUT/daily.tsv"
echo -e "variant\tn_trades\tnet_usd\thit_pct\tn_markets_run" > "$summary"
echo -e "date\tvariant\tn_trades\tnet_usd\thit_pct\tn_markets_run" > "$daily"

for var in "${VARS[@]}"; do
  for day in "${DATES[@]}"; do
    tag="${day}_${var}"
    log="$OUT/${tag}.log"
    json="$OUT/${tag}.json"
    echo "== $tag =="
    # shellcheck disable=SC2046
    "$BIN" alpha "${COMMON[@]}" $(gate_flags "$var") \
      --date-start "$day" --date-end "$day" \
      --out-json "$json" \
      > "$log" 2>&1
    python3 - "$json" "$day" "$var" "$daily" <<'PY'
import json, sys
path, day, var, tsv = sys.argv[1:5]
with open(path) as f:
    r = json.load(f)
cells = r.get("sweep") or [r]
rep = cells[0].get("report", {})
agg = rep.get("aggregate") or rep.get("cells", {}).get("BTC-300s", {})
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", agg.get("net_pnl_usd", 0)))
wins = int(agg.get("n_wins", 0))
mkts = int(r.get("n_markets_run", 0))
hit = (100.0 * wins / n) if n else 0.0
with open(tsv, "a") as f:
    f.write(f"{day}\t{var}\t{n}\t{net:.2f}\t{hit:.1f}\t{mkts}\n")
print(f"  {day} n={n} NET=${net:+,.0f} hit={hit:.1f}% mkts={mkts}")
PY
  done
done

python3 - "$daily" "$summary" <<'PY'
import sys
from collections import defaultdict
daily_path, summary_path = sys.argv[1:3]
rows = []
with open(daily_path) as f:
    next(f)
    for line in f:
        day, var, n, net, hit, mkts = line.rstrip().split("\t")
        rows.append((day, var, int(n), float(net), float(hit), int(mkts)))
by_var = defaultdict(lambda: {"n": 0, "net": 0.0, "wins": 0.0, "mkts": 0})
for day, var, n, net, hit, mkts in rows:
    by_var[var]["n"] += n
    by_var[var]["net"] += net
    by_var[var]["wins"] += n * hit / 100.0
    by_var[var]["mkts"] += mkts
with open(summary_path, "w") as f:
    f.write("variant\tn_trades\tnet_usd\thit_pct\tn_markets_run\n")
    for var in sorted(by_var):
        s = by_var[var]
        hit = 100.0 * s["wins"] / s["n"] if s["n"] else 0.0
        f.write(f"{var}\t{s['n']}\t{s['net']:.2f}\t{hit:.1f}\t{s['mkts']}\n")
        print(f"TOTAL {var}: n={s['n']} NET=${s['net']:+,.0f} hit={hit:.1f}% mkts={s['mkts']}")
PY

echo ""
echo "Wrote $summary"
echo "Wrote $daily"
echo ""
echo "=== Daily P&L ==="
column -t "$daily" 2>/dev/null || cat "$daily"
echo ""
echo "=== Totals ==="
column -t "$summary" 2>/dev/null || cat "$summary"