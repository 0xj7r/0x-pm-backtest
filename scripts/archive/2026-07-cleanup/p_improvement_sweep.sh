#!/usr/bin/env bash
# P-calculation improvement sweep — VERIFY window BTC-5m, shadow/hold baseline.
#
# Baseline to beat: shadow_baseline (~+$26k NET on VERIFY) in
# data/runs/analysis/shadow_baseline_summary.json — replicated here as perp_w75.
#
# Usage:
#   ./scripts/p_improvement_sweep.sh
#   BIN=./target/fast/pm-app OUT=data/runs/p-improvement ./scripts/p_improvement_sweep.sh
#
# Score:
#   python3 scripts/score_p_improvement.py
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/fast/pm-app}"
OUT="${OUT:-data/runs/p-improvement}"
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 5 ] || { log "DISK GUARD: ${free_gb}GB free — abort"; exit 1; }
}

write_summary() {
  local tag=$1
  local report="$OUT/${tag}.json"
  local summary="$OUT/${tag}_summary.json"
  python3 - "$tag" "$report" "$summary" <<'PY'
import json, sys
tag, report_path, summary_path = sys.argv[1:4]
with open(report_path) as f:
    r = json.load(f)
entry = r["sweep"][-1]
agg = entry["report"]["aggregate"]
hc = r["harness_cfg"]
mc = r["model_cfg"]
out = {
    "variant": tag,
    "window": "VERIFY",
    "market": "btc5m",
    "date_start": r.get("date_start"),
    "date_end": r.get("date_end"),
    "latency_ms": entry["latency_ms"],
    "edge_threshold": entry["edge_threshold"],
    "notional_usdc": hc["notional_usdc"],
    "perp_price_weight": mc.get("perp_price_weight", 0.0),
    "vol_estimator": mc.get("vol_estimator", "realized"),
    "fee_curve_rate": hc.get("fee_curve_rate", 0.07),
    "depth_capture_frac": hc.get("depth_capture_frac", 1.0),
    "skip_touch_level": hc.get("skip_touch_level", False),
    "min_entry_sigma_bps": hc.get("min_entry_sigma_bps", 0.0),
    "skip_saturday": hc.get("skip_saturday", False),
    "tail_max_price": hc.get("tail_max_price", 0.0),
    "tail_frac": hc.get("tail_frac", 0.0),
    "calibrated": bool(mc.get("calibrator")),
    "strikes": str(mc.get("strikes_path") or ""),
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
  disk_ok
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

VERIFY_DATES="--date-start 2026-05-07 --date-end 2026-05-18"
COMMON="$COMMON_BASE $VERIFY_DATES"

# Shadow/hold stack — parity with shadow_baseline in thesis_backtest_sweep.sh
SHADOW_EXTRA="--perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
  --edge-thresholds 0.12 --exit-after-s 0"

# Train calibrator on TUNE (Feb–Apr), evaluate VERIFY only.
CAL_DATES="--date-start 2026-02-12 --date-end 2026-05-18"

log "======== p-improvement VERIFY sweep (btc5m) ========"
log "BIN=$BIN  OUT=$OUT"

# --- A. Calibrator ---
run_variant p_cal_train_verify $COMMON_BASE $CAL_DATES \
  --calibrate-split 2026-05-07 \
  --calibrator-out "$OUT/calibrator_verify.json" \
  $SHADOW_EXTRA

# --- B. Perp weight ---
run_variant perp_w50 $COMMON $SHADOW_EXTRA --perp-price-weight 0.50
run_variant perp_w75 $COMMON $SHADOW_EXTRA
run_variant perp_w90 $COMMON $SHADOW_EXTRA --perp-price-weight 0.90

# --- C. Vol ---
run_variant vol_ewma600 $COMMON $SHADOW_EXTRA \
  --vol-estimator ewma --ewma-halflife-s 600

run_variant vol_blend $COMMON $SHADOW_EXTRA \
  --vol-estimator blend

run_variant sigma_floor_only $COMMON $SHADOW_EXTRA \
  --min-entry-sigma-bps 3.0 --skip-saturday

# --- D. Fee proxy (decide has no fee gate; threshold + fee_curve on P&L) ---
run_variant edge_014 $COMMON \
  --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
  --edge-thresholds 0.14 --exit-after-s 0

run_variant edge_016 $COMMON \
  --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
  --edge-thresholds 0.16 --exit-after-s 0

# --- E. Stress harness ---
run_variant stress_depth25 $COMMON $SHADOW_EXTRA \
  --depth-capture-frac 0.25

run_variant stress_skip_touch $COMMON $SHADOW_EXTRA \
  --skip-touch-level

run_variant stress_both $COMMON $SHADOW_EXTRA \
  --depth-capture-frac 0.25 --skip-touch-level

# --- F. Class C tail sleeve ---
# tail_010: convexity hedge sleeve on the shadow stack.
run_variant tail_010 $COMMON $SHADOW_EXTRA \
  --tail-max-price 0.10 --tail-frac 0.25

# tail_010_standalone: cheap-side entries only (ask <= 0.10).
# Full F11 late-window 1–2c harness is separate; this is the alpha-screen proxy.
run_variant tail_010_standalone $COMMON \
  --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 1 --min-marginal-edge 0.02 \
  --edge-thresholds 0.04 --exit-after-s 0 \
  --max-entry-ask 0.10

# --- G. Official strikes (skip if backfill missing) ---
STRIKES="data/manifests/canonical/strikes_btc5m.jsonl"
if [ -f "$STRIKES" ]; then
  run_variant strikes_official $COMMON $SHADOW_EXTRA --strikes "$STRIKES"
else
  log "skip strikes_official — no strikes file at $STRIKES"
fi

log "======== quick comparison ========"
python3 scripts/score_p_improvement.py "$OUT" || true

log "done — outputs in $OUT"