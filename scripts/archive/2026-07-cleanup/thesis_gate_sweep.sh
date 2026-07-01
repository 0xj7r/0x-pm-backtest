#!/usr/bin/env bash
# Thesis A gate sweep — isolate A_lead bucket gates on shadow stack (btc5m).
#
# Validates thesis A entry filters from configs/thesis_a_btc5m.toml against the
# frozen shadow/hold baseline (θ=0.12, $50 flat, perp@0.75, hold).
# See docs/research/strategy-hunt/07-strategy-forward-plan.md §2 Class A.
#
# Usage:
#   ./scripts/thesis_gate_sweep.sh
#   WINDOWS=VERIFY ./scripts/thesis_gate_sweep.sh
#   BIN=./target/fast/pm-app ./scripts/thesis_gate_sweep.sh
#
# Outputs: data/runs/analysis/{WINDOW}_{variant}.json + *_summary.json
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/analysis}"
WINDOWS="${WINDOWS:-VERIFY,TUNE}"
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 5 ] || { log "DISK GUARD: ${free_gb}GB free — abort"; exit 1; }
}

window_dates() {
  case $1 in
    TUNE)    echo "--date-start 2026-02-12 --date-end 2026-04-30" ;;
    VERIFY)  echo "--date-start 2026-05-07 --date-end 2026-05-18" ;;
    *) log "unknown window $1"; return 1 ;;
  esac
}

write_summary() {
  local tag=$1
  local report="$OUT/${tag}.json"
  local summary="$OUT/${tag}_summary.json"
  python3 - "$tag" "$report" "$summary" <<'PY'
import json, sys
tag, report_path, summary_path = sys.argv[1:4]
window, _, variant = tag.partition("_")
with open(report_path) as f:
    r = json.load(f)
entry = r["sweep"][-1]
agg = entry["report"]["aggregate"]
hc = r["harness_cfg"]
out = {
    "tag": tag,
    "variant": variant,
    "window": window,
    "market": "btc5m",
    "date_start": r.get("date_start"),
    "date_end": r.get("date_end"),
    "latency_ms": entry["latency_ms"],
    "edge_threshold": entry["edge_threshold"],
    "notional_usdc": hc["notional_usdc"],
    "kelly_sizing": hc.get("kelly_sizing", False),
    "min_p_side": hc.get("min_p_side", 0.0),
    "min_entry_ask": hc.get("min_entry_ask", 0.0),
    "max_entry_ask": hc.get("max_entry_ask", 1.0),
    "perp_price_weight": r["model_cfg"].get("perp_price_weight", 0.0),
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
    f"trades={out['trades']} hit={out['hit']*100:.1f}% "
    f"kelly={out['kelly_sizing']}"
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
  local trades="$OUT/${tag}.trades.jsonl"
  "$BIN" alpha "$@" --out-json "$json" \
    --trades-out "$trades" > "$OUT/${tag}.log" 2>&1
  write_summary "$tag"
  # Summaries captured; drop bulky trade tapes to save disk.
  rm -f "$trades"
}

COMMON="--local-cache-dir data/cache \
  --down-assets data/manifests/canonical/down_all.jsonl \
  --tick-cache-dir data/cache/ticks \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
  --slug-prefix btc-updown-5m- \
  --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 \
  --fee-curve-rate 0.07 --notional-usdc 50"

# Shadow/hold stack — parity with shadow_baseline in thesis_backtest_sweep.sh
SHADOW="--perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
  --edge-thresholds 0.12 --exit-after-s 0"

# Full A_lead bucket gates (configs/thesis_a_btc5m.toml)
GATES_FULL="--min-p-side 0.25 --min-entry-ask 0.15 --max-entry-ask 0.85"

log "======== Thesis A gate sweep (btc5m) ========"
log "BIN=$BIN  OUT=$OUT  WINDOWS=$WINDOWS"

for WIN in ${WINDOWS//,/ }; do
  DATES=$(window_dates "$WIN") || exit 1
  log "-------- $WIN ($DATES) --------"

  # 1. Full gates, flat $50 (promotion candidate; kelly off per shadow parity)
  run_variant "${WIN}_thesis_a_gated" \
    $COMMON $DATES $SHADOW $GATES_FULL

  # 2. Full gates + Kelly (negative control — H7 showed −32% NET vs flat)
  run_variant "${WIN}_thesis_a_gated_kelly" \
    $COMMON $DATES $SHADOW $GATES_FULL --kelly-sizing

  # 3. p_side floor only — how much tail PnL is low-belief entries?
  run_variant "${WIN}_thesis_a_p_floor" \
    $COMMON $DATES $SHADOW --min-p-side 0.25

  # 4. ask floor only — how much PnL is sub-0.15 lottery tickets?
  run_variant "${WIN}_thesis_a_ask_floor" \
    $COMMON $DATES $SHADOW --min-entry-ask 0.15
done

log "======== comparison (thesis A gate sweep) ========"
python3 - "$OUT" <<'PY'
import glob, json, os, sys

out = sys.argv[1]
prefixes = ("VERIFY_thesis_a_", "TUNE_thesis_a_")
rows = []
for p in sorted(glob.glob(os.path.join(out, "*_summary.json"))):
    base = os.path.basename(p)
    tag = base.replace("_summary.json", "")
    if not tag.startswith(prefixes):
        continue
    with open(p) as f:
        rows.append(json.load(f))

rows.sort(key=lambda r: (r["window"], r["variant"]))

hdr = (
    f"{'window':<7} {'variant':<24} {'NET':>12} {'trades':>8} "
    f"{'hit%':>7} {'kelly':>6} {'p_min':>5} {'ask_lo':>6} {'ask_hi':>6}"
)
print(hdr)
print("-" * len(hdr))
for r in rows:
    print(
        f"{r['window']:<7} {r['variant']:<24} "
        f"${r['NET']:>10,.2f} {r['trades']:>8} {r['hit']*100:>6.1f}% "
        f"{str(r['kelly_sizing']):>6} {r['min_p_side']:>5.2f} "
        f"{r['min_entry_ask']:>6.2f} {r['max_entry_ask']:>6.2f}"
    )

# Reference shadow baseline on VERIFY if present (ungated control)
ref = os.path.join(out, "shadow_baseline_summary.json")
if os.path.isfile(ref):
    with open(ref) as f:
        b = json.load(f)
    print()
    print(
        f"reference VERIFY shadow_baseline (ungated): "
        f"NET=${b['NET']:,.2f} trades={b['trades']} hit={b['hit']*100:.1f}%"
    )

# TUNE→VERIFY consistency on gated flat variant
tune = next((r for r in rows if r["variant"] == "thesis_a_gated" and r["window"] == "TUNE"), None)
verify = next((r for r in rows if r["variant"] == "thesis_a_gated" and r["window"] == "VERIFY"), None)
if tune and verify:
    from datetime import date
    d_tune = (date.fromisoformat(tune["date_end"]) - date.fromisoformat(tune["date_start"])).days + 1
    d_verify = (date.fromisoformat(verify["date_end"]) - date.fromisoformat(verify["date_start"])).days + 1
    tune_pd = tune["NET"] / d_tune
    verify_pd = verify["NET"] / d_verify
    ratio = verify_pd / tune_pd if tune_pd else float("nan")
    target = 0.9 * tune_pd * (d_verify / d_tune)
    print()
    print(
        f"consistency thesis_a_gated: VERIFY ${verify_pd:,.0f}/d vs TUNE ${tune_pd:,.0f}/d "
        f"(ratio {ratio:.2f}x; gate wants VERIFY > ${target:,.0f} scaled)"
    )
PY

log "done — outputs in $OUT"
log "post-run: python3 scripts/fade_entry_decompose.py $OUT/VERIFY_thesis_a_gated.trades.jsonl"