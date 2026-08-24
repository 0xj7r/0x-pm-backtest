#!/usr/bin/env bash
# Data-integrity Week 1 harness: fail-fast audit before backtest claims.
#
# Runs in sequence:
#   1. data_gate_june.sh (cache coverage)
#   2. pm-alpha decide:: unit tests
#   3. decide_construction_parity (backtest-vs-live DecisionInputs parity)
#   4. optional 1-day alpha smoke (if BIN exists): reports n_skipped_load_error
#
# Usage:
#   ./scripts/pipeline/harness_data_audit.sh
#   DATE_START=2026-06-10 DATE_END=2026-06-16 ./scripts/pipeline/harness_data_audit.sh
#   SKIP_PARITY=1 ./scripts/pipeline/harness_data_audit.sh        # data gate + tests only
#   SKIP_SMOKE=1 ./scripts/pipeline/harness_data_audit.sh
set -euo pipefail
cd "$(dirname "$0")/../.."

DATE_START="${DATE_START:-2026-06-10}"
DATE_END="${DATE_END:-2026-06-16}"
SKIP_PARITY="${SKIP_PARITY:-${SKIP_EQUIVALENCE:-0}}"
SKIP_SMOKE="${SKIP_SMOKE:-0}"
BIN="${BIN:-./target/release/pm-app}"
SMOKE_DAY="${SMOKE_DAY:-$DATE_END}"
SMOKE_OUT="${SMOKE_OUT:-/tmp/harness_data_audit_smoke.json}"

fail=0
declare -a RESULTS=()

record() {
  local name=$1
  local status=$2
  RESULTS+=("$name:$status")
  if [[ "$status" == "FAIL" ]]; then
    fail=1
  fi
}

echo "=== harness data audit ==="
echo "window: $DATE_START .. $DATE_END"
echo ""

echo "=== 1. data gate (June cache) ==="
if ./scripts/pipeline/data_gate_june.sh "$DATE_START" "$DATE_END"; then
  record "data_gate" "PASS"
else
  record "data_gate" "FAIL"
fi
echo ""

echo "=== 2. decide:: unit tests ==="
if cargo test -p pm-alpha decide:: --quiet; then
  record "decide_tests" "PASS"
else
  record "decide_tests" "FAIL"
fi
echo ""

if [[ "$SKIP_PARITY" == "1" ]]; then
  record "decide_construction_parity" "SKIP"
  echo "=== 3. decide_construction_parity: SKIPPED (SKIP_PARITY=1) ==="
else
  echo "=== 3. decide_construction_parity ==="
  if cargo run -p pm-app --bin decide_construction_parity --quiet; then
    record "decide_construction_parity" "PASS"
  else
    record "decide_construction_parity" "FAIL"
  fi
fi
echo ""

if [[ "$SKIP_SMOKE" == "1" ]]; then
  record "alpha_smoke" "SKIP"
  echo "=== 4. alpha smoke: SKIPPED (SKIP_SMOKE=1) ==="
elif [[ ! -x "$BIN" ]]; then
  record "alpha_smoke" "SKIP"
  echo "=== 4. alpha smoke: SKIPPED (BIN not executable: $BIN) ==="
else
  echo "=== 4. alpha smoke (1 day: $SMOKE_DAY) ==="
  rm -f "$SMOKE_OUT"
  if "$BIN" alpha \
    --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
    --slug-prefix btc-updown-5m- \
    --down-assets data/manifests/canonical/down_all.jsonl \
    --local-cache-dir data/cache \
    --tick-cache-dir data/cache/ticks \
    --date-start "$SMOKE_DAY" --date-end "$SMOKE_DAY" \
    --exit-after-s 0 \
    --perp-symbol BTCUSDT \
    --perp-price-weight 0.75 \
    --vol-estimator realized \
    --vol-lookback-s 3600 \
    --edge-thresholds 0.12 \
    --notional-usdc 50 \
    --latency-ms 150 \
    --max-clips 2 \
    --rearm-edge 0.08 \
    --clip-cooldown-ms 5000 \
    --min-entry-sigma-bps 3 \
    --skip-saturday \
    --stop-before-close-s 90 \
    --min-marginal-edge 0.04 \
    --fee-curve-rate 0.07 \
    --skip-spot-misalign-s 30 \
    --min-entry-ask 0.45 \
    --out-json "$SMOKE_OUT" > /dev/null 2>&1; then
    smoke_status=$(python3 - "$SMOKE_OUT" <<'PY'
import json, sys
from pathlib import Path

path = Path(sys.argv[1])
if not path.is_file():
    print("FAIL missing_json")
    raise SystemExit(0)
r = json.load(open(path))
n_load = r.get("n_skipped_load_error")
if n_load is None:
    cell = (r.get("sweep") or [r])[0]
    n_load = cell.get("n_skipped_load_error")
if n_load is None:
    print("FAIL missing_n_skipped_load_error")
else:
    n_run = int(r.get("n_markets_run", 0))
    print(f"PASS n_skipped_load_error={n_load} n_markets_run={n_run}")
PY
)
    if [[ "$smoke_status" == FAIL* ]]; then
      echo "  $smoke_status"
      record "alpha_smoke" "FAIL"
    else
      echo "  $smoke_status"
      record "alpha_smoke" "PASS"
    fi
  else
    echo "  alpha smoke command failed"
    record "alpha_smoke" "FAIL"
  fi
fi
echo ""

echo "=== harness data audit summary ==="
for row in "${RESULTS[@]}"; do
  name="${row%%:*}"
  status="${row#*:}"
  printf "  %-24s %s\n" "$name" "$status"
done
echo ""

if [[ "$fail" -eq 0 ]]; then
  echo "OVERALL: PASS"
  exit 0
else
  echo "OVERALL: FAIL"
  exit 1
fi