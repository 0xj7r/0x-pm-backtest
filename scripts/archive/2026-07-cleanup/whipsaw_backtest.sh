#!/usr/bin/env bash
# Multi-window whipsaw gate backtest (TUNE / VERIFY / HOLDOUT / JUNE).
#
# Gates live in pm_alpha::decide (SSOT) — not post-hoc Python replay.
#
# Usage:
#   ./scripts/whipsaw_backtest.sh
#   VARIANT=combo ./scripts/whipsaw_backtest.sh
#   WINDOWS="VERIFY HOLDOUT" ./scripts/whipsaw_backtest.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/whipsaw}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"

WINDOWS="${WINDOWS:-TUNE VERIFY HOLDOUT JUNE}"
VARIANT="${VARIANT:-all}"

mkdir -p "$OUT"

window_dates() {
  case "$1" in
    TUNE)    echo "2026-02-12 2026-04-30" ;;
    VERIFY)  echo "2026-05-07 2026-05-18" ;;
    HOLDOUT) echo "2026-05-19 2026-05-28" ;;
    JUNE)    echo "2026-06-01 2026-06-07" ;;
    *) echo "unknown window $1" >&2; exit 1 ;;
  esac
}

# Shadow-final parity baseline knobs.
COMMON=(
  --markets "$MANIFEST"
  --local-cache-dir "$CACHE"
  --tick-cache-dir "${TICKS:-data/cache/ticks}"
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
    openfav)  echo --skip-open-fav-gap ;;
    loss2)    echo --pause-after-consec-losses 2 ;;
    loss3)    echo --pause-after-consec-losses 3 ;;
    rearm70)  echo --max-rearm-entry-ask 0.70 ;;
    combo)
      echo --skip-expanded-high-flip --skip-open-fav-gap \
           --pause-after-consec-losses 2 --max-rearm-entry-ask 0.70
      ;;
    *) echo "unknown variant $1" >&2; exit 1 ;;
  esac
}

variants_for() {
  case "$VARIANT" in
    all) echo "baseline flip openfav loss2 loss3 rearm70 combo" ;;
    *)   echo "$VARIANT" ;;
  esac
}

if [[ ! -x "$BIN" ]]; then
  echo "Building $BIN ..."
  cargo build -p pm-app --release
fi

summary_tsv="$OUT/summary.tsv"
echo -e "window\tvariant\tn_trades\tnet_usd\thit_pct" > "$summary_tsv"

for win in $WINDOWS; do
  read -r DS DE <<< "$(window_dates "$win")"
  for var in $(variants_for); do
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
with open(tsv, "a") as f:
    f.write(f"{win}\t{var}\t{n}\t{net:.2f}\t{hit:.1f}\n")
print(f"  n={n} NET=${net:+,.0f} hit={hit:.1f}%")
PY
  done
done

echo ""
echo "Wrote $summary_tsv"
column -t "$summary_tsv" 2>/dev/null || cat "$summary_tsv"