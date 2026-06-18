#!/usr/bin/env bash
# Resume strategy_validation after VERIFY baseline completed.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${OUT:-data/runs/strategy_validation}"
BIN="${BIN:-./target/release/pm-app}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"
TICKS="${TICKS:-data/cache/ticks}"
summary="$OUT/summary.tsv"

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
    flip)     echo --skip-expanded-high-flip ;;
    rearm70)  echo --max-rearm-entry-ask 0.70 ;;
    *) exit 1 ;;
  esac
}

run_one() {
  local win=$1 var=$2 DS=$3 DE=$4
  local tag="${win}_${var}"
  echo "== $tag =="
  # shellcheck disable=SC2046
  "$BIN" alpha "${COMMON[@]}" $(gate_flags "$var") \
    --date-start "$DS" --date-end "$DE" \
    --out-json "$OUT/${tag}.json" \
    > "$OUT/${tag}.log" 2>&1
  python3 - "$OUT/${tag}.json" "$win" "$var" "$summary" <<'PY'
import json, sys
path, win, var, tsv = sys.argv[1:5]
with open(path) as f:
    r = json.load(f)
cells = r.get("sweep") or [r]
rep = cells[0].get("report", {})
agg = rep.get("aggregate") or rep.get("cells", {}).get("BTC-300s", {})
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", agg.get("net_pnl_usd", 0)))
hit = float(agg.get("hit_rate", 0)) * 100
mkts = int(r.get("n_markets_run", 0))
with open(tsv, "a") as f:
    f.write(f"{win}\t{var}\t{n}\t{net:.2f}\t{hit:.1f}\t{mkts}\n")
print(f"  n={n} NET=${net:+,.0f} hit={hit:.1f}%")
PY
}

[[ -f "$summary" ]] || echo -e "window\tvariant\tn_trades\tnet_usd\thit_pct\tn_markets_run" > "$summary"

run_one VERIFY rearm70 2026-05-07 2026-05-18
run_one VERIFY flip 2026-05-07 2026-05-18
run_one HOLDOUT baseline 2026-05-19 2026-05-28
run_one HOLDOUT rearm70 2026-05-19 2026-05-28
run_one HOLDOUT flip 2026-05-19 2026-05-28

echo "Done. Summary:"
column -t "$summary" 2>/dev/null || cat "$summary"