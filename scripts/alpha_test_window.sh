#!/usr/bin/env bash
# pm-alpha hunt: run ONE frozen config on the May 19-28 test window.
# Raw base:    VOL=1800 THR=0.05 OUT=test_base bash scripts/alpha_test_window.sh
# Calibrated:  CAL=1 VOL=1800 THR=0.05 OUT=test_cal bash scripts/alpha_test_window.sh
#   (trains the calibrator on May 7-18, evaluates May 19-28, one command)
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p data/runs/alpha/test

ARGS=(
  alpha
  --markets data/manifests/may2026_focused/markets_btc.jsonl
  --local-cache-dir data/cache
  --latency-sweep --edge-thresholds "${THR:-0.05}"
  --vol-lookback-s "${VOL:-1800}"
  --momentum-lookback-s "${MOM:-0}" --momentum-weight "${MW:-1.0}"
  --out-json "data/runs/alpha/test/${OUT:-test}.json"
)
if [ "${CAL:-0}" = "1" ]; then
  ARGS+=(
    --date-start 2026-05-07 --date-end 2026-05-28
    --calibrate-split 2026-05-19
    --calibrator-out "data/runs/alpha/test/${OUT:-test}_calibrator.json"
  )
else
  ARGS+=(--date-start 2026-05-19 --date-end 2026-05-28)
fi

./target/fast/pm-app "${ARGS[@]}" 2>&1 | tail -40
echo TEST_WINDOW_DONE
