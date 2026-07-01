#!/usr/bin/env bash
# Vol + stress harness variants for p-improvement (VERIFY, shadow stack).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/fast/pm-app}"
OUT="${OUT:-data/runs/p-improvement}"
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

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
    "vol_lookback_s": mc.get("vol_lookback_s", 3600),
    "ewma_halflife_s": mc.get("ewma_halflife_s"),
    "depth_capture_frac": hc.get("depth_capture_frac", 1.0),
    "skip_touch_level": hc.get("skip_touch_level", False),
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

COMMON=(
  --local-cache-dir data/cache
  --down-assets data/manifests/canonical/down_all.jsonl
  --tick-cache-dir data/cache/ticks
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl
  --slug-prefix btc-updown-5m-
  --date-start 2026-05-07
  --date-end 2026-05-18
  --latency-ms 150
  --vol-lookback-s 3600
  --stop-before-close-s 90
  --fee-curve-rate 0.07
  --notional-usdc 50
)

SHADOW=(
  --perp-symbol BTCUSDT
  --perp-price-weight 0.75
  --rearm-edge 0.08
  --max-clips 2
  --min-marginal-edge 0.04
  --edge-thresholds 0.12
  --exit-after-s 0
)

log "======== vol + stress (VERIFY) ========"

run_variant vol_ewma600 "${COMMON[@]}" "${SHADOW[@]}" \
  --vol-estimator ewma --ewma-halflife-s 600

# vol_fast300: override default 3600s lookback (cannot pass --vol-lookback-s twice).
COMMON_FAST300=(
  --local-cache-dir data/cache
  --down-assets data/manifests/canonical/down_all.jsonl
  --tick-cache-dir data/cache/ticks
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl
  --slug-prefix btc-updown-5m-
  --date-start 2026-05-07
  --date-end 2026-05-18
  --latency-ms 150
  --vol-lookback-s 300
  --stop-before-close-s 90
  --fee-curve-rate 0.07
  --notional-usdc 50
)
run_variant vol_fast300 "${COMMON_FAST300[@]}" "${SHADOW[@]}" \
  --vol-estimator realized

run_variant stress_depth25 "${COMMON[@]}" "${SHADOW[@]}" \
  --depth-capture-frac 0.25

run_variant stress_skip_touch "${COMMON[@]}" "${SHADOW[@]}" \
  --skip-touch-level

run_variant stress_both "${COMMON[@]}" "${SHADOW[@]}" \
  --depth-capture-frac 0.25 --skip-touch-level

log "======== results ========"
python3 - "$OUT" <<'PY'
import json, glob, os, sys
out = sys.argv[1]
baseline_net = 26025.73
rows = []
for p in sorted(glob.glob(os.path.join(out, "*_summary.json"))):
    tag = os.path.basename(p).replace("_summary.json", "")
    if tag not in {
        "vol_ewma600", "vol_fast300",
        "stress_depth25", "stress_skip_touch", "stress_both",
    }:
        continue
    with open(p) as f:
        rows.append(json.load(f))
rows.sort(key=lambda r: r["variant"])
print(f"{'variant':<20} {'NET':>12} {'trades':>8} {'hit%':>7} {'%base':>7} {'stress':>8}")
print("-" * 68)
for r in rows:
    pct = 100.0 * r["NET"] / baseline_net if baseline_net else 0
    stress = ""
    if r["variant"].startswith("stress_"):
        ok = r["NET"] > 0 and r["NET"] > 0.5 * baseline_net
        stress = "PASS" if ok else "FAIL"
    print(f"{r['variant']:<20} ${r['NET']:>10,.2f} {r['trades']:>8} {r['hit']*100:>6.1f}% {pct:>6.1f}% {stress:>8}")
PY

log "done"