#!/usr/bin/env bash
# Thesis backtest sweep — VERIFY window BTC-5m, three mechanistic variants.
# Usage: ./scripts/thesis_backtest_sweep.sh
# Reproduces analysis in data/runs/analysis/ (shadow_baseline, spot_only, champion).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/fast/pm-app}"
OUT="${OUT:-data/runs/analysis}"
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
out = {
    "variant": tag,
    "window": "VERIFY",
    "market": "btc5m",
    "date_start": r.get("date_start"),
    "date_end": r.get("date_end"),
    "latency_ms": entry["latency_ms"],
    "edge_threshold": entry["edge_threshold"],
    "notional_usdc": r["harness_cfg"]["notional_usdc"],
    "perp_price_weight": r["model_cfg"].get("perp_price_weight", 0.0),
    "NET": round(agg["total_pnl"], 2),
    "trades": agg["n_trades"],
    "hit": round(agg["hit_rate"], 4),
    "n_markets": agg["n_markets"],
}
with open(summary_path, "w") as f:
    json.dump(out, f, indent=2)
    f.write("\n")
print(f"summary {summary_path}: NET=${out['NET']:,.2f} trades={out['trades']} hit={out['hit']*100:.1f}%")
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

COMMON="--local-cache-dir data/cache \
  --down-assets data/manifests/canonical/down_all.jsonl \
  --tick-cache-dir data/cache/ticks \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
  --slug-prefix btc-updown-5m- \
  --date-start 2026-05-07 --date-end 2026-05-18 \
  --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 \
  --fee-curve-rate 0.07"

SHADOW_EXTRA="--perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
  --edge-thresholds 0.12 --exit-after-s 0"

log "======== VERIFY thesis sweep (btc5m) ========"

# 1. shadow_baseline — hold@0.12, $50, perp-led (alias: shadow_match)
run_variant shadow_baseline $COMMON --notional-usdc 50 $SHADOW_EXTRA

# 2. spot_only — same stack, spot-only belief (perp weight 0)
run_variant spot_only $COMMON --notional-usdc 50 \
  --perp-symbol BTCUSDT --perp-price-weight 0 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
  --edge-thresholds 0.12 --exit-after-s 0

# 3. shadow_parity — in-harness sigma floor + Saturday (live shadow gates)
run_variant shadow_parity $COMMON --notional-usdc 50 $SHADOW_EXTRA \
  --min-entry-sigma-bps 3.0 --skip-saturday

# 4. champion — timed exit F1 baseline
run_variant champion $COMMON --notional-usdc 25 \
  --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08 \
  --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60

# Legacy alias for downstream scripts expecting shadow_match.*
if [ -s "$OUT/shadow_baseline.json" ] && [ ! -e "$OUT/shadow_match.json" ]; then
  ln -sf shadow_baseline.json "$OUT/shadow_match.json"
  ln -sf shadow_baseline.trades.jsonl "$OUT/shadow_match.trades.jsonl"
  ln -sf shadow_baseline_summary.json "$OUT/shadow_match_summary.json"
fi
if [ -s "$OUT/champion.json" ] && [ ! -e "$OUT/champion_f1.json" ]; then
  ln -sf champion.json "$OUT/champion_f1.json"
  ln -sf champion.trades.jsonl "$OUT/champion_f1.trades.jsonl"
  ln -sf champion_summary.json "$OUT/champion_f1_summary.json"
fi

log "======== comparison ========"
python3 - "$OUT" <<'PY'
import json, glob, os, sys
out = sys.argv[1]
rows = []
for p in sorted(glob.glob(os.path.join(out, "*_summary.json"))):
    with open(p) as f:
        rows.append(json.load(f))
rows.sort(key=lambda r: r["variant"])
print(f"{'variant':<18} {'NET':>12} {'trades':>8} {'hit%':>7}")
print("-" * 48)
for r in rows:
    print(f"{r['variant']:<18} ${r['NET']:>10,.2f} {r['trades']:>8} {r['hit']*100:>6.1f}%")
PY

log "done — outputs in $OUT"