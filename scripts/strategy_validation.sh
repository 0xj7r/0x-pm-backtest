#!/usr/bin/env bash
# Full strategy robustness validation (live-parity config).
#
# 1. VERIFY + HOLDOUT backtest grid (gates in pm_alpha::decide SSOT)
# 2. Python counterfactual gate replay on VERIFY trade tape
# 3. June diagnostic (cached tick days)
#
# Usage:
#   ./scripts/strategy_validation.sh
#   WINDOWS=HOLDOUT VARIANT=baseline,rearm70 ./scripts/strategy_validation.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/strategy_validation}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"
TICKS="${TICKS:-data/cache/ticks}"
WINDOWS="${WINDOWS:-VERIFY HOLDOUT}"
# loss2/loss3: use Python counterfactual on VERIFY trades (Rust serial gate uses daily reset).
VARIANT="${VARIANT:-baseline,rearm70,flip}"

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
    baseline) ;;
    flip)     echo --skip-expanded-high-flip ;;
    openfav)  echo --skip-open-fav-gap ;;
    loss2)    echo --pause-after-consec-losses 2 ;;
    loss3)    echo --pause-after-consec-losses 3 ;;
    rearm70)  echo --max-rearm-entry-ask 0.70 ;;
    combo)
      echo --skip-expanded-high-flip --pause-after-consec-losses 2 --max-rearm-entry-ask 0.70
      ;;
    *) echo "unknown variant $1" >&2; exit 1 ;;
  esac
}

window_dates() {
  case "$1" in
    VERIFY)  echo "2026-05-07 2026-05-18" ;;
    HOLDOUT) echo "2026-05-19 2026-05-28" ;;
    JUNE)    echo "2026-06-01 2026-06-07" ;;
    *) echo "unknown window $1" >&2; exit 1 ;;
  esac
}

if [[ ! -x "$BIN" ]]; then
  echo "Building $BIN ..."
  cargo build -p pm-app --release
fi

IFS=',' read -ra VARS <<< "${VARIANT// /,}"
summary="$OUT/summary.tsv"
echo -e "window\tvariant\tn_trades\tnet_usd\thit_pct\tn_markets_run" > "$summary"

for win in $WINDOWS; do
  read -r DS DE <<< "$(window_dates "$win")"
  for var in "${VARS[@]}"; do
    tag="${win}_${var}"
    log="$OUT/${tag}.log"
    json="$OUT/${tag}.json"
    echo "== $tag ($DS .. $DE) =="
    trades_arg=()
    if [[ "$var" == "baseline" && ( "$win" == "VERIFY" || "$win" == "HOLDOUT" ) ]]; then
      trades_arg=(--trades-out "$OUT/${win}_baseline.trades.jsonl")
    fi
    # shellcheck disable=SC2046
    if ((${#trades_arg[@]})); then
      "$BIN" alpha "${COMMON[@]}" $(gate_flags "$var") \
        --date-start "$DS" --date-end "$DE" \
        --out-json "$json" \
        "${trades_arg[@]}" \
        > "$log" 2>&1
    else
      "$BIN" alpha "${COMMON[@]}" $(gate_flags "$var") \
        --date-start "$DS" --date-end "$DE" \
        --out-json "$json" \
        > "$log" 2>&1
    fi
    python3 - "$json" "$win" "$var" "$summary" <<'PY'
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
print(f"  n={n} NET=${net:+,.0f} hit={hit:.1f}% mkts={mkts}")
PY
  done
done

for tape in VERIFY HOLDOUT; do
  f="$OUT/${tape}_baseline.trades.jsonl"
  if [[ -s "$f" ]]; then
    echo ""
    echo "== Python counterfactual gates (${tape} baseline tape) =="
    python3 scripts/whipsaw_gate_validate.py "$f" \
      | tee "$OUT/whipsaw_gate_validate_${tape}.log"
    python3 scripts/loss_cluster_analyze.py "$f" \
      | tee "$OUT/loss_cluster_${tape}.log"
  fi
done

echo ""
echo "== June diagnostic (cached tick days) =="
JUNE_OUT="$OUT/june"
mkdir -p "$JUNE_OUT"
OUT="$JUNE_OUT" VARIANT=baseline,rearm70 ./scripts/june_live_backtest.sh 2>&1 | tee "$JUNE_OUT/run.log" || true
if [[ -f "$JUNE_OUT/daily.tsv" ]]; then
  cp "$JUNE_OUT/daily.tsv" "$OUT/june_daily.tsv"
  cp "$JUNE_OUT/summary.tsv" "$OUT/june_summary.tsv"
fi

echo ""
echo "Wrote $summary"
column -t "$summary" 2>/dev/null || cat "$summary"