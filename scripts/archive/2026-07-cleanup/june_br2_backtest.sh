#!/usr/bin/env bash
# BonereaperV2 June OOS backtest — compare against gated fade daily runs.
#
# Two modes:
#   daily   — one walk-forward per calendar day (fast, no meta retrain; --disable-meta-calibration)
#   oos     — train meta on all pre-June markets, eval June 1–16 (slow; canonical favourite flags)
#
# Usage:
#   MODE=daily DATE_START=2026-06-01 DATE_END=2026-06-16 ./scripts/june_br2_backtest.sh
#   MODE=oos ./scripts/june_br2_backtest.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/june_br2_compare}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
MODE="${MODE:-daily}"
DATE_START="${DATE_START:-2026-06-01}"
DATE_END="${DATE_END:-2026-06-16}"
SKIP_EXISTING="${SKIP_EXISTING:-1}"
mkdir -p "$OUT"

if [[ ! -x "$BIN" ]]; then
  cargo build -p pm-app --release
fi

# Canonical favourite lane (from configs/bonereaper_v2_favourite_062901.command.txt).
# Profile bonereaper_v2_leader.toml supplies lane defaults; CLI overrides match the
# May-29 winning arm where they differ.
BR2_FLAGS=(
  --local-cache-dir data/cache
  --strategies bonereaper_v2
  --profile configs/bonereaper_v2_leader.toml
  --starting-cash 1000
  --portfolio-mode
  --clip-fraction-of-equity 0.015
  --max-clip-usdc 30
  --max-order-clip-multiplier 10
  --max-per-market-exposure-usdc 250
  --max-per-market-exposure-frac 0.12
  --kelly-fraction 0.5
  --use-outcome-label
  --spot-symbol BTCUSDT
  --replay-sample-ms 1000
  --taker-latency-ms 500
  --br2-participation-clip-frac 0.0
  --br2-late-clip-frac 1.0
  --br2-late-favourite-start-secs 180
  --br2-late-favourite-min-ask 0.70
  --br2-late-favourite-max-ask 0.97
  --br2-late-favourite-clip-frac 1.0
  --br2-late-favourite-max-clips 12
  --br2-late-favourite-min-model-confidence 0.68
  --br2-late-favourite-max-model-risk 0.72
  --br2-late-favourite-min-model-side-p 0.62
  --br2-late-favourite-min-model-edge 0.09
  --br2-late-favourite-min-realized-vol-180s-bps 1.25
  --model-gate-min-confidence 0.68
  --model-gate-max-risk 0.72
)

daily_tsv="$OUT/daily.tsv"
if [[ ! -s "$daily_tsv" ]]; then
  echo -e "date\tn_mkts\tnet_usd\tn_fills\thit_pct\tmode" > "$daily_tsv"
fi

append_daily() {
  local day=$1 summary=$2
  python3 - "$day" "$summary" "$daily_tsv" <<'PY'
import json, sys
from pathlib import Path

day, summary_path, tsv = sys.argv[1:4]
r = json.load(open(summary_path))
s = r.get("per_strategy", {}).get("bonereaper_v2", {})
net = float(s.get("total_pnl_usdc", 0))
mkts = int(s.get("markets_with_orders", 0))
fills = int(s.get("total_orders_filled", 0))
hit = 100.0 * float(s.get("hit_rate", 0))
mode = r.get("run_config", {}).get("mode", "daily")

lines = Path(tsv).read_text().splitlines()
header, body = lines[0], lines[1:]
body = [ln for ln in body if ln.strip() and not ln.startswith(f"{day}\t")]
body.append(f"{day}\t{mkts}\t{net:.2f}\t{fills}\t{hit:.1f}\t{mode}")
Path(tsv).write_text(header + "\n" + "\n".join(body) + "\n")
print(f"  {day}: mkts={mkts} fills={fills} NET=${net:+,.0f} hit={hit:.1f}%")
PY
}

market_offset() {
  python3 - "$MANIFEST" "$1" <<'PY'
import json, sys
manifest, day = sys.argv[1:3]
n = 0
for line in open(manifest):
    if not line.strip():
        continue
    if json.loads(line).get("date", "") < day:
        n += 1
print(n)
PY
}

markets_on_day() {
  python3 - "$MANIFEST" "$1" <<'PY'
import json, sys
manifest, day = sys.argv[1:3]
n = 0
for line in open(manifest):
    if not line.strip():
        continue
    if json.loads(line).get("date", "") == day:
        n += 1
print(n)
PY
}

run_daily() {
  local day=$1
  local tag_dir="$OUT/daily_${day}"
  local summary="$tag_dir/summary.json"
  if [[ "$SKIP_EXISTING" == "1" && -s "$summary" ]]; then
    echo "== skip $day (exists) =="
    append_daily "$day" "$summary"
    return 0
  fi
  local skip
  skip=$(market_offset "$day")
  local nmkts
  nmkts=$(markets_on_day "$day")
  mkdir -p "$tag_dir"
  echo "== br2 daily $day (skip=$skip n=$nmkts) =="
  # Per-day OOS: train meta on all markets before this day, eval this day only.
  local manifest_slice="$tag_dir/markets_slice.jsonl"
  python3 - "$MANIFEST" "$day" "$manifest_slice" <<'PY'
import json, sys
manifest, day, out = sys.argv[1:4]
rows = []
for line in open(manifest):
    if not line.strip():
        continue
    r = json.loads(line)
    if r.get("date", "") <= day:
        rows.append(line.strip())
open(out, "w").write("\n".join(rows) + ("\n" if rows else ""))
print(f"manifest slice: {len(rows)} markets through {day}")
PY
  "$BIN" walk-forward --markets "$manifest_slice" "${BR2_FLAGS[@]}" \
    --min-train-markets "$skip" \
    --meta-epochs 10 \
    --meta-learning-rate 0.04 \
    --meta-l2 0.001 \
    --meta-weight-clip 1.50 \
    --meta-max-fit-samples 120000 \
    --meta-max-validation-samples 60000 \
    --meta-max-samples-per-market 64 \
    --out-markets "$tag_dir/markets.jsonl" \
    --out-summary "$summary" > "$tag_dir/run.log" 2>&1
  python3 - "$summary" "$day" <<'PY'
import json, sys
p, day = sys.argv[1:3]
r = json.load(open(p))
r.setdefault("run_config", {})["mode"] = "daily_meta_oos"
r["eval_day"] = day
json.dump(r, open(p, "w"), indent=2)
PY
  append_daily "$day" "$summary"
}

run_oos() {
  local tag_dir="$OUT/oos_jun_pretrain"
  local summary="$tag_dir/summary.json"
  if [[ "$SKIP_EXISTING" == "1" && -s "$summary" ]]; then
    echo "== skip OOS (exists) =="
    return 0
  fi
  mkdir -p "$tag_dir"
  local manifest_slice="data/manifests/june_br2_may_june.jsonl"
  local train_mkts
  train_mkts=$(python3 - "$MANIFEST" "$manifest_slice" <<'PY'
import json, sys
from pathlib import Path

manifest, out = sys.argv[1:3]
spot_root = Path("data/cache/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT")

def has_spot(day: str) -> bool:
    d = spot_root / f"date={day}"
    return d.is_dir() and any(d.glob("*.parquet"))

rows, may = [], 0
for line in open(manifest):
    if not line.strip():
        continue
    d = json.loads(line).get("date", "")
    if not has_spot(d):
        continue
    if d.startswith("2026-05") or ("2026-06-01" <= d <= "2026-06-16"):
        rows.append(line.strip())
        if d.startswith("2026-05"):
            may += 1
open(out, "w").write("\n".join(rows) + ("\n" if rows else ""))
print(may)
PY
)
  echo "== br2 OOS: meta-train May ($train_mkts mkts), eval June 1–16 =="
  "$BIN" walk-forward --markets "$manifest_slice" "${BR2_FLAGS[@]}" \
    --min-train-markets "$train_mkts" \
    --meta-epochs 10 \
    --meta-learning-rate 0.04 \
    --meta-l2 0.001 \
    --meta-weight-clip 1.50 \
    --meta-max-fit-samples 120000 \
    --meta-max-validation-samples 60000 \
    --meta-max-samples-per-market 64 \
    --out-markets "$tag_dir/markets.jsonl" \
    --out-summary "$summary" \
    --meta-calibrator-snapshot-out "$tag_dir/meta_snapshot.json" \
    > "$tag_dir/run.log" 2>&1
  python3 - "$summary" "$tag_dir/markets.jsonl" <<'PY'
import json, sys
from collections import defaultdict
from pathlib import Path

summary_path, markets_path = sys.argv[1:3]
r = json.load(open(summary_path))
r.setdefault("run_config", {})["mode"] = "oos_pre_june_meta"
by_day = defaultdict(float)
mkts = defaultdict(int)
for line in Path(markets_path).read_text().splitlines():
    if not line.strip():
        continue
    row = json.loads(line)
    d = row.get("date") or row.get("market_date") or ""
    if not d.startswith("2026-06"):
        continue
    pnl = float(row.get("pnl_usdc", row.get("pnl", 0)) or 0)
    by_day[d] += pnl
    if pnl != 0 or row.get("orders_filled", 0):
        mkts[d] += 1
june_net = sum(by_day.values())
r["june_daily_pnl"] = dict(sorted(by_day.items()))
r["june_net_usdc"] = june_net
json.dump(r, open(summary_path, "w"), indent=2)
print(f"June NET (from markets.jsonl): ${june_net:+,.0f}")
for d, v in sorted(by_day.items()):
    print(f"  {d}: ${v:+,.0f} ({mkts[d]} mkts)")
PY
}

DATES=()
while IFS= read -r day; do
  DATES+=("$day")
done < <(python3 - "$MANIFEST" "$DATE_START" "$DATE_END" <<'PY'
import json, sys
from datetime import date, timedelta
manifest, start, end = sys.argv[1:4]
want = set()
cur = date.fromisoformat(start)
stop = date.fromisoformat(end)
while cur <= stop:
    want.add(cur.isoformat())
    cur += timedelta(days=1)
seen = set()
for line in open(manifest):
    if not line.strip():
        continue
    d = json.loads(line).get("date", "")
    if d in want:
        seen.add(d)
for d in sorted(seen):
    print(d)
PY
)

case "$MODE" in
  daily)
    echo "BR2 daily June: ${DATES[*]}"
    for day in "${DATES[@]}"; do
      run_daily "$day"
    done
    ;;
  oos)
    run_oos
    ;;
  both)
    for day in "${DATES[@]}"; do run_daily "$day"; done
    run_oos
    ;;
  *)
    echo "unknown MODE=$MODE (daily|oos|both)" >&2
    exit 1
    ;;
esac

echo ""
echo "=== BR2 daily summary ==="
column -t "$daily_tsv" 2>/dev/null || cat "$daily_tsv"

python3 - "$daily_tsv" "$OUT/../june_gated_daily/daily.tsv" <<'PY' 2>/dev/null || true
import sys
from pathlib import Path
from collections import defaultdict

br2_tsv, fade_tsv = sys.argv[1:3]
fade = {}
if Path(fade_tsv).is_file():
    for line in Path(fade_tsv).read_text().splitlines()[1:]:
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 6 and parts[1] == "base":
            fade[parts[0]] = float(parts[3])

br2 = {}
for line in Path(br2_tsv).read_text().splitlines()[1:]:
    if not line.strip():
        continue
    d, mkts, net, fills, hit, mode = line.split("\t")
    br2[d] = float(net)

days = sorted(set(fade) | set(br2))
print("\n=== Fade (gated base) vs BR2 daily ===")
print(f"{'date':<12} {'fade':>10} {'br2':>10} {'delta':>10}")
print("-" * 46)
ft = bt = 0
for d in days:
    f = fade.get(d, 0)
    b = br2.get(d, 0)
    ft += f
    bt += b
    print(f"{d:<12} ${f:>+8.0f} ${b:>+8.0f} ${b-f:>+8.0f}")
print("-" * 46)
print(f"{'TOTAL':<12} ${ft:>+8.0f} ${bt:>+8.0f} ${bt-ft:>+8.0f}")
PY