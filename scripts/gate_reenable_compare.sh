#!/usr/bin/env bash
# Compare gate packages for re-enable decision.
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/gate_reenable_compare}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"
TICKS="${TICKS:-data/cache/ticks}"

mkdir -p "$OUT"

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
    mom30)           echo --skip-spot-misalign-s 30 ;;
    mom30_ask045)    echo --skip-spot-misalign-s 30 --min-entry-ask 0.45 ;;
    mom30_ask055_of) echo --skip-spot-misalign-s 30 --min-entry-ask 0.55 --skip-open-fav-gap ;;
    *) echo "unknown variant $1" >&2; exit 1 ;;
  esac
}

WINDOWS="HOLDOUT JUN1017"
VARIANTS="mom30 mom30_ask045 mom30_ask055_of"

window_dates() {
  case "$1" in
    HOLDOUT) echo "2026-05-19 2026-05-28" ;;
    JUN1017) echo "2026-06-10 2026-06-17" ;;
    *) echo "unknown window $1" >&2; exit 1 ;;
  esac
}

summary_tsv="$OUT/summary.tsv"
echo -e "window\tvariant\tn_trades\tnet_usd\thit_pct\tper_trade" > "$summary_tsv"

for win in $WINDOWS; do
  read -r DS DE <<< "$(window_dates "$win")"
  for var in $VARIANTS; do
    tag="${win}_${var}"
    log="$OUT/${tag}.log"
    json="$OUT/${tag}.json"
    echo "== $tag ($DS .. $DE) =="
    # shellcheck disable=SC2046
    "$BIN" alpha "${COMMON[@]}" $(gate_flags "$var") \
      --date-start "$DS" --date-end "$DE" \
      --out-json "$json" \
      > "$log" 2>&1
    python3 - "$json" "$win" "$var" "$summary_tsv" <<'PY'
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
per = net / n if n else 0.0
with open(tsv, "a") as f:
    f.write(f"{win}\t{var}\t{n}\t{net:.2f}\t{hit:.1f}\t{per:.2f}\n")
print(f"  n={n} NET=${net:+,.0f} hit={hit:.1f}% ${per:+.2f}/tr")
PY
  done
done

echo ""
echo "Wrote $summary_tsv"
column -t "$summary_tsv" 2>/dev/null || cat "$summary_tsv"