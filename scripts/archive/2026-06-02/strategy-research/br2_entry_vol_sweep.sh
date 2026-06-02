#!/usr/bin/env bash
# br2 entry-timing x vol-floor sweep (frozen-snapshot replay over calm May).
# Tests whether a LATER entry rescues the lifted vol-floor case that is
# catastrophic at the normal 180s entry.
set -uo pipefail

ROOT="/Users/jackreid/go/polymarket-backtest"
cd "$ROOT"

BIN="target/release/pm-app"
MARKETS="${SWEEP_MARKETS:-data/runs/volgate/markets-may-labeled.jsonl}"
CACHE="data/cache"
SNAP="data/snap062901.json"
OUTROOT="${SWEEP_OUTROOT:-data/runs/br2_entry_sweep_labeled}"
mkdir -p "$OUTROOT"

# Frozen-replay base flags: the canonical br2 command MINUS training/path/out
# flags, PLUS replay invocation + LIVE deltas. start-secs and the three
# vol-floor flags are appended per-cell, so their canonical values are dropped
# from this base set.
BASE_FLAGS=(
  --strategies bonereaper_v2
  --starting-cash 1000
  --max-clip-usdc 30
  --max-order-clip-multiplier 10
  --max-per-market-exposure-usdc 250
  --max-per-market-exposure-frac 0.12
  --kelly-fraction 0.5
  --spot-symbol BTCUSDT
  --portfolio-mode
  --clip-fraction-of-equity 0.015
  --clip-drawdown-soft-pct 0.2
  --clip-drawdown-hard-pct 0.4
  --clip-drawdown-min-multiplier 0.1
  --br2-participation-clip-frac 0.0
  --br2-participation-max-pair-cost 0.99
  --br2-participation-max-orders-per-leg 500
  --br2-participation-max-inventory-delta-shares 25.0
  --br2-participation-repair-inventory-delta-shares 5.0
  --br2-participation-refresh-secs 0.50
  --br2-participation-stop-secs-before-close 20.0
  --br2-min-composite-direction 0.10
  --br2-early-clip-frac 0.00
  --br2-mid-clip-frac 0.00
  --br2-late-clip-frac 1.0
  --br2-late-max-fires 3
  --br2-late-confirm-min-model-confidence 0.58
  --br2-late-confirm-max-model-risk 0.80
  --br2-late-confirm-min-model-side-p 0.58
  --br2-late-confirm-min-model-edge 0.02
  --br2-late-confirm-min-book-skew 0.06
  --br2-late-confirm-max-whipsaw-score 0.85
  --br2-late-confirm-max-observed-range 0.50
  --br2-recent-regime-gate-min-edge 0.08
  --br2-high-skew-clip-frac 0.60
  --br2-high-skew-max-clips 5
  --br2-high-skew-max-whipsaw-score 0.75
  --br2-late-favourite-threshold 0.22
  --br2-late-favourite-max-ask 0.97
  --br2-late-favourite-clip-frac 1.00
  --br2-late-favourite-high-cert-clip-frac 1.00
  --br2-late-favourite-high-cert-full-clip-edge 0.09
  --br2-late-favourite-max-clips 12
  --br2-late-favourite-min-sustain-secs 0.0
  --br2-late-favourite-sweep-depth 7
  --br2-late-favourite-min-model-confidence 0.68
  --br2-late-favourite-min-model-direction-abs 0.0
  --br2-late-favourite-max-model-risk 0.72
  --br2-late-favourite-min-model-side-p 0.62
  --br2-late-favourite-high-cert-min-model-edge 0.06
  --br2-late-favourite-max-whipsaw-score 0.75
  --br2-late-favourite-max-reversal-pressure 0.85
  --br2-late-favourite-min-path-efficiency 0.0
  --br2-late-favourite-max-observed-range 0.70
  --br2-late-favourite-range-soft-throttle 0.55
  --br2-late-favourite-range-hard-throttle 0.70
  --br2-late-favourite-range-extra-edge 0.08
  --br2-late-favourite-range-extra-confidence 0.12
  --br2-late-favourite-max-adverse-fast-momentum 1.0
  --br2-late-favourite-max-adverse-broad-momentum 1.0
  --br2-late-favourite-max-entry-pullback 1.0
  --br2-late-favourite-max-avg-entry-drawdown 1.0
  --br2-tail-clip-frac 0.10
  --br2-tail-max-clips 6
  --br2-tail-sweep-depth 3
  --br2-tail-min-ask 0.01
  --br2-tail-max-ask 0.08
  --br2-tail-min-seconds-to-close 10.0
  --br2-tail-min-favourite-unrealized-edge 0.0
  --br2-tail-min-observed-range 0.0
  --br2-tail-target-favourite-loss-coverage-frac 0.50
  --br2-tail-reversal-coverage-frac 0.00
  --br2-tail-reversal-min-seconds-to-close 10.0
  --br2-tail-reversal-max-seconds-to-close 35.0
  --br2-tail-reversal-min-favourite-ask 0.85
  --br2-tail-extreme-threshold 0.30
  --br2-tail-min-skew-step 0.02
  --br2-tail-budget-favourite-spend-frac 0.20
  --br2-tail-budget-favourite-upside-frac 0.25
  --br2-tail-regime-boost-coverage-frac 0.0
  --br2-tail-regime-boost-budget-spend-frac 0.0
  --br2-tail-regime-boost-budget-upside-frac 0.0
  --br2-tail-regime-boost-min-whipsaw-score 1.0
  --br2-tail-regime-boost-min-reversal-pressure 1.0
  --br2-tail-regime-boost-min-realized-vol-180s-bps 1000000000.0
  --br2-tail-regime-boost-max-path-efficiency 0.0
  --model-gate-min-confidence 0.68
  --model-gate-max-risk 0.72
  --model-gate-min-edge 0.00
  --replay-sample-ms 1000
  --taker-latency-ms 500
  --portfolio-checkpoint-every-markets 250
  # replay invocation
  --markets "$MARKETS"
  --local-cache-dir "$CACHE"
  --use-outcome-label
  --meta-calibrator-snapshot-in "$SNAP"
  --disable-meta-calibration
  # LIVE deltas
  --br2-late-favourite-min-ask 0.60
  --br2-late-favourite-min-model-edge 0.06
)

STARTS=(180 120 90 60 45 30)
VOLS=(1.25 0.0)

for S in "${STARTS[@]}"; do
  for V in "${VOLS[@]}"; do
    CELL="$OUTROOT/start${S}_vol${V}"
    mkdir -p "$CELL"
    echo "=== $(date +%H:%M:%S) START cell start=${S} vol=${V} -> $CELL ==="
    "$BIN" walk-forward "${BASE_FLAGS[@]}" \
      --br2-late-favourite-start-secs "$S" \
      --br2-late-confirm-min-realized-vol-180s-bps "$V" \
      --br2-high-skew-min-realized-vol-180s-bps "$V" \
      --br2-late-favourite-min-realized-vol-180s-bps "$V" \
      --out-markets "$CELL/markets.jsonl" \
      --out-summary "$CELL/summary.json"
    rc=$?
    echo "=== $(date +%H:%M:%S) DONE cell start=${S} vol=${V} rc=${rc} ==="
    if [ $rc -ne 0 ]; then
      echo "!!! CELL FAILED start=${S} vol=${V} rc=${rc}"
    fi
  done
done

echo "all cells attempted" > "$OUTROOT/DONE"
echo "=== $(date +%H:%M:%S) SWEEP COMPLETE ==="
