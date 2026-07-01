#!/bin/bash
# Full hypothesis batch: every harness-runnable backlog item across the three
# exploration windows (W1 trend, W2 mixed, W3 whipsaw). Selection requires
# cross-regime consistency; May 19+ and June are never touched.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=data/runs/alpha/batch0612
mkdir -p "$OUT"
MAXJOBS=6
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 20 ] || { log "DISK GUARD: ${free_gb}GB free, stopping"; return 1; }
}

throttle() { while [ "$(jobs -rp | wc -l)" -ge "$MAXJOBS" ]; do sleep 10; done; }

BTC5="--markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m-"
ETH5="--markets data/manifests/canonical/eth-updown-5m_up.jsonl --slug-prefix eth-updown-5m-"
BTC15="--markets data/manifests/canonical/btc-updown-15m_up.jsonl --slug-prefix btc-updown-15m-"
COMMON="--local-cache-dir data/cache --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 --fee-curve-rate 0.07"
COMBO="--perp-symbol BTCUSDT --perp-price-weight 0.5 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08"
PASSIVE="--exit-at-mid --passive-exit-timeout-s 60"

window_args() {
  case $1 in
    W1) echo "--date-start 2026-02-12 --date-end 2026-03-31" ;;
    W2) echo "--date-start 2026-04-01 --date-end 2026-04-30" ;;
    W3) echo "--date-start 2026-05-07 --date-end 2026-05-18" ;;
  esac
}

run() { # name, then arg list
  local name=$1; shift
  if [ -s "$OUT/${name}.json" ]; then log "skip $name"; return; fi
  disk_ok || exit 1
  throttle
  log "run $name"
  "$BIN" alpha "$@" --out-json "$OUT/${name}.json" \
    --trades-out "$OUT/${name}.trades.jsonl" > "$OUT/${name}.log" 2>&1 \
    || log "WARN $name failed" &
}

# W3 first (fail-fast triage), then W2, then W1.
for W in W3 W2 W1; do
  DATES=$(window_args $W)
  log "===== window $W ====="

  # H0 threshold refit under passive exit (0.16 already done in feemin)
  for THR in 0.10 0.12 0.14; do
    run "${W}_h0_thr${THR/./}" $COMMON $BTC5 $DATES $COMBO $PASSIVE \
      --edge-thresholds "$THR" --exit-after-s 30
  done

  # H1 passive timeout sweep
  for TO in 30 90 120; do
    run "${W}_h1_to${TO}" $COMMON $BTC5 $DATES $COMBO \
      --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s "$TO"
  done

  # H2 exit horizon under passive exit
  for EX in 45 60 90; do
    run "${W}_h2_ex${EX}" $COMMON $BTC5 $DATES $COMBO $PASSIVE \
      --edge-thresholds 0.16 --exit-after-s "$EX"
  done

  # H4 rearm off
  run "${W}_h4_noclip2" $COMMON $BTC5 $DATES $PASSIVE \
    --perp-symbol BTCUSDT --perp-price-weight 0.5 --min-marginal-edge 0.08 \
    --edge-thresholds 0.16 --exit-after-s 30 --max-clips 1

  # H5 perp weight sweep
  for PW in 0.25 0.75 1.0; do
    run "${W}_h5_pw${PW/./}" $COMMON $BTC5 $DATES $PASSIVE \
      --perp-symbol BTCUSDT --perp-price-weight "$PW" --rearm-edge 0.08 --max-clips 2 \
      --min-marginal-edge 0.08 --edge-thresholds 0.16 --exit-after-s 30
  done

  # H6 15m horizon
  run "${W}_h6_btc15" $COMMON $BTC15 $DATES $COMBO $PASSIVE \
    --edge-thresholds 0.16 --exit-after-s 30

  # H7 kelly / momentum overlay / ewma vol
  run "${W}_h7_kelly" $COMMON $BTC5 $DATES $COMBO $PASSIVE \
    --edge-thresholds 0.16 --exit-after-s 30 --kelly-sizing
  for MW in 0.1 0.2; do
    run "${W}_h7_mom${MW/./}" $COMMON $BTC5 $DATES $COMBO $PASSIVE \
      --edge-thresholds 0.16 --exit-after-s 30 --momentum-lookback-s 300 --momentum-weight "$MW"
  done
  for HL in 600 1800; do
    run "${W}_h7_ewma${HL}" $COMMON $BTC5 $DATES $COMBO $PASSIVE \
      --edge-thresholds 0.16 --exit-after-s 30 --vol-estimator ewma --ewma-halflife-s "$HL"
  done

  # F1 window-open momentum as its own engine (aligned expression)
  run "${W}_f1_aligned_e30" $COMMON $BTC5 $DATES $PASSIVE \
    --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 --exit-after-s 30
  run "${W}_f1_aligned_hold" $COMMON $BTC5 $DATES \
    --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 --exit-after-s 0

  # ETH-5m one-shot recheck under passive exit
  run "${W}_eth5_passive" $COMMON $ETH5 $DATES $COMBO $PASSIVE \
    --edge-thresholds 0.16 --exit-after-s 30

  wait
  log "===== window $W complete ====="
done
log ALL_DONE
