#!/usr/bin/env bash
# May/June-only strategy calibration — separate from pre-May exo_fade tuning.
#
# Splits (no Feb–Apr data):
#   TRAIN  2026-05-07 .. 2026-05-24  — calibrator fit + dir samples
#   VAL_MAY 2026-05-25 .. 2026-05-31 — May holdout
#   VAL_JUN 2026-06-01 .. 2026-06-17 — June holdout (live regime)
#
# Variants:
#   legacy_prod   — current live stack (mom30 + ask0.45), baseline comparison
#   mayjune       — book-aware mayjune_fade gates (no ML)
#   mayjune_cal   — mayjune gates + ExoCalibrator trained on TRAIN only
#   mayjune_cal_dir — mayjune_cal + DirModel from TRAIN dir samples
#   mayjune_aligned — aligned entry + dir model
#
# Usage:
#   ./scripts/mayjune_strategy_calibrate.sh
#   PHASE=eval ./scripts/mayjune_strategy_calibrate.sh   # skip train if snapshot exists
#   python3 scripts/score_mayjune_calibrate.py
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/mayjune_calibrate}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"
TICKS="${TICKS:-data/cache/ticks}"
PHASE="${PHASE:-all}"
SKIP_EXISTING="${SKIP_EXISTING:-1}"

TRAIN_START="2026-05-07"
TRAIN_END="2026-05-24"
VAL_MAY_START="2026-05-25"
VAL_MAY_END="2026-05-31"
VAL_JUN_START="2026-06-01"
VAL_JUN_END="2026-06-17"
CAL_SPLIT="2026-05-25"
CALIBRATOR="${OUT}/calibrator_may_train.json"
DIR_SAMPLES="${OUT}/dir_samples_may_train.jsonl"
DIR_MODEL="${OUT}/dir_model_may_train.json"

mkdir -p "$OUT"

if [[ ! -x "$BIN" ]]; then
  cargo build -p pm-app --release
fi

COMMON=(
  --markets "$MANIFEST"
  --local-cache-dir "$CACHE"
  --tick-cache-dir "$TICKS"
  --exit-after-s 0
  --perp-symbol BTCUSDT
  --perp-price-weight 0.75
  --vol-estimator realized
  --vol-lookback-s 3600
  --edge-thresholds 0.14
  --notional-usdc 50
  --latency-ms 250
  --max-clips 2
  --rearm-edge 0.08
  --clip-cooldown-ms 5000
  --min-entry-sigma-bps 3
  --skip-saturday
  --stop-before-close-s 90
  --min-marginal-edge 0.06
  --fee-curve-rate 0.07
)

# mayjune_fade decision gates (mirror ExoFadeConfig::mayjune_btc5m).
MAYJUNE_GATES=(
  --min-entry-ask 0.45
  --max-entry-ask 0.85
  --max-p-side 0.92
  --skip-open-fav-gap
  --open-fav-p-min 0.88
  --open-fav-ask-max 0.62
  --open-fav-secs 300
  --skip-spot-misalign-s 60
)

LEGACY_GATES=(
  --skip-spot-misalign-s 30
  --min-entry-ask 0.45
)

run_alpha() {
  local tag=$1
  local ds=$2
  local de=$3
  shift 3
  local json="$OUT/${tag}.json"
  local trades="$OUT/${tag}.trades.jsonl"
  local log="$OUT/${tag}.log"
  if [[ "$SKIP_EXISTING" == "1" && -s "$json" ]]; then
    echo "== skip $tag (exists) =="
    return 0
  fi
  echo "== $tag ($ds .. $de) =="
  "$BIN" alpha "${COMMON[@]}" "$@" \
    --date-start "$ds" --date-end "$de" \
    --out-json "$json" \
    --trades-out "$trades" \
    > "$log" 2>&1
}

append_summary() {
  python3 - "$OUT/summary.tsv" "$@" <<'PY'
import json, sys
from pathlib import Path

tsv = Path(sys.argv[1])
rows = sys.argv[2:]
path, window, variant = rows[0], rows[1], rows[2]
with open(path) as f:
    r = json.load(f)
cells = r.get("sweep") or [r]
agg = cells[0].get("report", {}).get("aggregate") or {}
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", agg.get("net_pnl_usd", 0)))
hit = float(agg.get("hit_rate", 0)) * 100
per = net / n if n else 0.0
if not tsv.is_file():
    tsv.write_text("window\tvariant\tn_trades\tnet_usd\thit_pct\tper_trade\n")
lines = tsv.read_text().splitlines()
header, body = lines[0], lines[1:]
body = [ln for ln in body if ln.strip() and not ln.startswith(f"{window}\t{variant}\t")]
body.append(f"{window}\t{variant}\t{n}\t{net:.2f}\t{hit:.1f}\t{per:.2f}")
tsv.write_text(header + "\n" + "\n".join(body) + "\n")
print(f"  {window}/{variant}: n={n} NET=${net:+,.0f} hit={hit:.1f}%")
PY
}

# --- Phase 1: train calibrator + dir samples on May TRAIN only ---
if [[ "$PHASE" == "all" || "$PHASE" == "train" ]]; then
  echo "######## Phase 1: TRAIN calibrator ($TRAIN_START .. $TRAIN_END) ########"
  if [[ "$SKIP_EXISTING" == "1" && -s "$CALIBRATOR" && -s "$DIR_SAMPLES" ]]; then
    echo "skip train (calibrator + dir samples exist)"
  else
    "$BIN" alpha "${COMMON[@]}" "${MAYJUNE_GATES[@]}" \
      --date-start "$TRAIN_START" --date-end "$VAL_JUN_END" \
      --calibrate-split "$CAL_SPLIT" \
      --calibrator-out "$CALIBRATOR" \
      --dir-samples-out "$DIR_SAMPLES" \
      --out-json "$OUT/train_calibrate.json" \
      > "$OUT/train_calibrate.log" 2>&1
    echo "calibrator -> $CALIBRATOR"
    echo "dir samples -> $DIR_SAMPLES"
  fi

  if [[ -s "$DIR_SAMPLES" && ! -s "$DIR_MODEL" ]]; then
    echo "training dir model on May TRAIN samples..."
    python3 scripts/mayjune_dir_train.py "$DIR_SAMPLES" "$DIR_MODEL" \
      > "$OUT/dir_train.log" 2>&1 || true
  fi
fi

# --- Phase 2: eval grid on VAL_MAY + VAL_JUN ---
eval_window() {
  local win=$1
  local ds=$2
  local de=$3

  run_alpha "${win}_legacy_prod" "$ds" "$de" "${LEGACY_GATES[@]}"
  append_summary "$OUT/${win}_legacy_prod.json" "$win" "legacy_prod"

  run_alpha "${win}_mayjune" "$ds" "$de" "${MAYJUNE_GATES[@]}"
  append_summary "$OUT/${win}_mayjune.json" "$win" "mayjune"

  if [[ -s "$CALIBRATOR" ]]; then
    run_alpha "${win}_mayjune_cal" "$ds" "$de" "${MAYJUNE_GATES[@]}" \
      --calibrator-in "$CALIBRATOR"
    append_summary "$OUT/${win}_mayjune_cal.json" "$win" "mayjune_cal"
  fi

  if [[ -s "$DIR_MODEL" ]]; then
    run_alpha "${win}_mayjune_cal_dir" "$ds" "$de" "${MAYJUNE_GATES[@]}" \
      --calibrator-in "$CALIBRATOR" \
      --dir-model "$DIR_MODEL"
    append_summary "$OUT/${win}_mayjune_cal_dir.json" "$win" "mayjune_cal_dir"

    run_alpha "${win}_mayjune_aligned" "$ds" "$de" "${MAYJUNE_GATES[@]}" \
      --aligned-mode --align-min-mid 0.55 \
      --calibrator-in "$CALIBRATOR" \
      --dir-model "$DIR_MODEL"
    append_summary "$OUT/${win}_mayjune_aligned.json" "$win" "mayjune_aligned"
  fi
}

if [[ "$PHASE" == "all" || "$PHASE" == "eval" ]]; then
  echo "######## Phase 2a: VAL_MAY ($VAL_MAY_START .. $VAL_MAY_END) ########"
  eval_window "VAL_MAY" "$VAL_MAY_START" "$VAL_MAY_END"
  echo "######## Phase 2b: VAL_JUN ($VAL_JUN_START .. $VAL_JUN_END) ########"
  eval_window "VAL_JUN" "$VAL_JUN_START" "$VAL_JUN_END"
  echo "######## Phase 2c: FULL May–Jun ($TRAIN_START .. $VAL_JUN_END) ########"
  eval_window "FULL" "$TRAIN_START" "$VAL_JUN_END"
fi

# --- Phase 3: walk-forward $1K portfolio compare ---
if [[ "$PHASE" == "all" || "$PHASE" == "wf" ]]; then
  echo "######## Phase 3: walk-forward \$1K ########"
  for strat in exo_fade mayjune_fade; do
    wf_dir="$OUT/wf_${strat}"
    if [[ "$SKIP_EXISTING" == "1" && -s "$wf_dir/summary.json" ]]; then
      echo "skip wf $strat"
      continue
    fi
    mkdir -p "$wf_dir"
    "$BIN" walk-forward \
      --markets "$MANIFEST" \
      --local-cache-dir "$CACHE" \
      --tick-cache-dir "$TICKS" \
      --strategies "$strat" \
      --date-start "$TRAIN_START" --date-end "$VAL_JUN_END" \
      --starting-cash 1000 \
      --portfolio-mode \
      --clip-fraction-of-equity 0.025 \
      --max-clip-usdc 50 \
      --use-outcome-label \
      --spot-symbol BTCUSDT \
      --out-markets "$wf_dir/markets.jsonl" \
      --out-summary "$wf_dir/summary.json" \
      > "$wf_dir/run.log" 2>&1
    echo "wf $strat -> $wf_dir/summary.json"
  done
fi

echo ""
echo "Done. Score with: python3 scripts/score_mayjune_calibrate.py"
column -t "$OUT/summary.tsv" 2>/dev/null || cat "$OUT/summary.tsv" 2>/dev/null || true