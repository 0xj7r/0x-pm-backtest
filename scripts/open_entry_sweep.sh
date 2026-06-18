#!/usr/bin/env bash
# Backtest open-window entry hypotheses on VERIFY (+ optional TUNE confirm).
#
# Tests whether instant mid entries (p~0.96, touch~0.50 at :00) help or hurt
# vs baseline shadow stack. Uses causal harness gates:
#   --min-secs-from-open N   wait N seconds after window open
#   --max-p-side X           cap saturated beliefs (live p=1.0 rows)
#   --max-entry-ask X        block fav/mid entries
#
# Usage:
#   ./scripts/open_entry_sweep.sh
#   WINDOW=TUNE ./scripts/open_entry_sweep.sh
#
# Score:
#   python3 scripts/score_open_entry.py
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/fast/pm-app}"
OUT="${OUT:-data/runs/open-entry}"
WINDOW="${WINDOW:-VERIFY}"
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

dates_for_window() {
  case "$WINDOW" in
    VERIFY) echo "--date-start 2026-05-07 --date-end 2026-05-18" ;;
    TUNE)   echo "--date-start 2026-02-12 --date-end 2026-04-30" ;;
    *)      echo "--date-start 2026-05-07 --date-end 2026-05-18" ;;
  esac
}

write_summary() {
  local tag=$1
  local report="$OUT/${tag}.json"
  local summary="$OUT/${tag}_summary.json"
  python3 - "$tag" "$report" "$summary" "$WINDOW" <<'PY'
import json, sys
tag, report_path, summary_path, window = sys.argv[1:5]
with open(report_path) as f:
    r = json.load(f)
entry = r["sweep"][-1]
agg = entry["report"]["aggregate"]
hc = r["harness_cfg"]
mc = r["model_cfg"]
out = {
    "variant": tag,
    "window": window,
    "market": "btc5m",
    "date_start": r.get("date_start"),
    "date_end": r.get("date_end"),
    "latency_ms": entry["latency_ms"],
    "edge_threshold": entry["edge_threshold"],
    "notional_usdc": hc["notional_usdc"],
    "perp_price_weight": mc.get("perp_price_weight", 0.0),
    "min_secs_from_open": hc.get("min_secs_from_open", 0),
    "max_p_side": hc.get("max_p_side", 1.0),
    "max_entry_ask": hc.get("max_entry_ask", 1.0),
    "min_entry_sigma_bps": hc.get("min_entry_sigma_bps", 0.0),
    "NET": round(agg["total_pnl"], 2),
    "trades": agg["n_trades"],
    "hit": round(agg["hit_rate"], 4),
    "n_markets": agg["n_markets"],
}
with open(summary_path, "w") as f:
    json.dump(out, f, indent=2)
    f.write("\n")
print(
    f"summary {summary_path}: NET=${out['NET']:,.2f} "
    f"trades={out['trades']} hit={out['hit']*100:.1f}%"
)
PY
}

run_variant() {
  local tag=$1
  shift
  local json="$OUT/${tag}.json"
  if [ -s "$json" ]; then
    log "skip $tag (exists)"
    write_summary "$tag"
    return
  fi
  log "alpha $tag"
  "$BIN" alpha "$@" --out-json "$json" \
    --trades-out "$OUT/${tag}.trades.jsonl" > "$OUT/${tag}.log" 2>&1
  write_summary "$tag"
}

COMMON_BASE="--local-cache-dir data/cache \
  --down-assets data/manifests/canonical/down_all.jsonl \
  --tick-cache-dir data/cache/ticks \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
  --slug-prefix btc-updown-5m- \
  --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 \
  --fee-curve-rate 0.07 --notional-usdc 50"

DATES=$(dates_for_window)
COMMON="$COMMON_BASE $DATES"

# Shadow/hold stack (live parity except sigma/saturday applied in harness when set)
SHADOW="--perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
  --edge-thresholds 0.12 --exit-after-s 0 \
  --min-entry-sigma-bps 3.0 --skip-saturday"

log "======== open-entry sweep ($WINDOW) ========"
log "BIN=$BIN  OUT=$OUT"

run_variant baseline $COMMON $SHADOW

run_variant open_delay_5s $COMMON $SHADOW --min-secs-from-open 5
run_variant open_delay_15s $COMMON $SHADOW --min-secs-from-open 15
run_variant open_delay_30s $COMMON $SHADOW --min-secs-from-open 30

run_variant max_p_085 $COMMON $SHADOW --max-p-side 0.85
run_variant max_p_090 $COMMON $SHADOW --max-p-side 0.90

run_variant max_ask_055 $COMMON $SHADOW --max-entry-ask 0.55
run_variant max_ask_060 $COMMON $SHADOW --max-entry-ask 0.60

run_variant open15_max_p090 $COMMON $SHADOW --min-secs-from-open 15 --max-p-side 0.90
run_variant open15_max_ask055 $COMMON $SHADOW --min-secs-from-open 15 --max-entry-ask 0.55

log "======== results ========"
python3 scripts/score_open_entry.py "$OUT" || true
log "done — outputs in $OUT"