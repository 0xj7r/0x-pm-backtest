#!/usr/bin/env bash
# Standing engine-validation suite. Run after ANY change to pm-alpha/pm-app.
# Layers:
#   1. unit + invariant tests (leakage, latency monotonicity, costs,
#      determinism, exit/fill semantics — 130+ tests)
#   2. bit-determinism: identical run twice -> identical JSON
#   3. golden regression: the frozen June holdout must reproduce the
#      committed result EXACTLY (any drift = unexplained engine change)
# Exit nonzero on any failure.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
GOLDEN=data/golden/june_holdout.json
fail=0

echo "=== 1. workspace tests ==="
TEST_OUT=$(cargo test --workspace 2>&1)
N_RESULTS=$(echo "$TEST_OUT" | grep -c "test result:")
N_OK=$(echo "$TEST_OUT" | grep -c "test result: ok")
if [ "$N_RESULTS" -eq 0 ] || [ "$N_OK" -ne "$N_RESULTS" ]; then
  echo "$TEST_OUT" | grep -E "FAILED|panicked|test result:" | head
  fail=1
else
  TOTAL=$(echo "$TEST_OUT" | grep -oE "[0-9]+ passed" | awk '{s+=$1} END {print s+0}')
  echo "ALL TEST SUITES OK ($TOTAL tests)"
fi

run_june() {
  "$BIN" alpha --local-cache-dir data/cache \
    --markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m- \
    --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks \
    --date-start 2026-06-01 --date-end 2026-06-07 \
    --latency-ms 150 --edge-thresholds 0.16 --vol-lookback-s 3600 --exit-after-s 30 \
    --stop-before-close-s 90 \
    --out-json "$1" > /dev/null 2>&1
}

echo "=== 2. determinism (June x2) ==="
run_june /tmp/val_a.json
run_june /tmp/val_b.json
if cmp -s /tmp/val_a.json /tmp/val_b.json; then
  echo "DETERMINISM OK"
else
  echo "DETERMINISM FAILED: identical inputs produced different outputs"
  fail=1
fi

echo "=== 3. golden June holdout ==="
if [ ! -f "$GOLDEN" ]; then
  mkdir -p data/golden
  cp /tmp/val_a.json "$GOLDEN"
  echo "GOLDEN CREATED (first run): $GOLDEN — commit it"
elif cmp -s /tmp/val_a.json "$GOLDEN"; then
  echo "GOLDEN OK: June holdout reproduces exactly"
else
  echo "GOLDEN DRIFT: June holdout no longer reproduces the committed result."
  echo "If an engine change INTENDED to alter results, re-baseline deliberately"
  echo "and record the delta in docs; otherwise this is a regression."
  python3 - "$GOLDEN" /tmp/val_a.json <<'PYEOF'
import json, sys
a = json.load(open(sys.argv[1])); b = json.load(open(sys.argv[2]))
ga = a["sweep"][0]["report"]["aggregate"]; gb = b["sweep"][0]["report"]["aggregate"]
for k in ("n_markets","n_trades","total_pnl","hit_rate","log_loss_exo"):
    if ga.get(k) != gb.get(k):
        print(f"  {k}: golden={ga.get(k)} now={gb.get(k)}")
PYEOF
  fail=1
fi

[ "$fail" = 0 ] && echo "=== VALIDATION PASSED ===" || echo "=== VALIDATION FAILED ==="
exit $fail
