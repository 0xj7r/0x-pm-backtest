#!/usr/bin/env bash
# Overnight directional-taker exploration across market regimes.
# Three exploration windows with different dynamics:
#   W1 Feb 12 - Mar 31  (trend / strong upside)
#   W2 Apr 1  - Apr 30  (mixed)
#   W3 May 7  - May 18  (whipsaw; the standing tune window)
# May 19-28 stays clean for the one-shot verification of any winner.
# Families: fade baseline, aligned momentum (mid x exit), aligned + convex
# tail hedge, momentum-augmented belief as fade and as aligned.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=data/runs/alpha/overnight2
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 15 ] || { log "DISK GUARD: ${free_gb}GB free, stopping"; return 1; }
}

run() { # name, then arg list
  local name=$1; shift
  if [ -f "$OUT/${name}.json" ]; then log "skip $name (exists)"; return; fi
  log "run $name"
  "$BIN" alpha "$@" --out-json "$OUT/${name}.json" \
    --trades-out "$OUT/${name}.trades.jsonl" > "$OUT/${name}.log" 2>&1 \
    || log "WARN $name failed"
}

BTC5="--markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m-"
ETH5="--markets data/manifests/canonical/eth-updown-5m_up.jsonl --slug-prefix eth-updown-5m-"
BTC15="--markets data/manifests/canonical/btc-updown-15m_up.jsonl --slug-prefix btc-updown-15m-"
COMMON="--local-cache-dir data/cache --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90"

window_args() {
  case $1 in
    W1) echo "--date-start 2026-02-12 --date-end 2026-03-31" ;;
    W2) echo "--date-start 2026-04-01 --date-end 2026-04-30" ;;
    W3) echo "--date-start 2026-05-07 --date-end 2026-05-18" ;;
  esac
}

for W in W3 W1 W2; do
  DATES=$(window_args $W)
  disk_ok || exit 0
  log "===== window $W ====="

  run "${W}_fade_base" $COMMON $BTC5 $DATES \
    --edge-thresholds 0.10,0.16,0.22 --exit-after-s 30

  for MID in 0.55 0.60; do
    run "${W}_aligned_m${MID}_hold" $COMMON $BTC5 $DATES \
      --aligned-mode --align-min-mid "$MID" --edge-thresholds 0.04,0.08,0.12 \
      --exit-after-s 0
  done
  run "${W}_aligned_m0.55_e30" $COMMON $BTC5 $DATES \
    --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08,0.12 \
    --exit-after-s 30

  run "${W}_aligned_tail" $COMMON $BTC5 $DATES \
    --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 \
    --exit-after-s 0 --tail-max-price 0.10 --tail-frac 0.25

  run "${W}_momfade_lb300" $COMMON $BTC5 $DATES \
    --momentum-lookback-s 300 --momentum-weight 1.0 \
    --edge-thresholds 0.10,0.16,0.22 --exit-after-s 30
  run "${W}_momfade_lb60" $COMMON $BTC5 $DATES \
    --momentum-lookback-s 60 --momentum-weight 1.0 \
    --edge-thresholds 0.10,0.16,0.22 --exit-after-s 30
  run "${W}_momaligned_lb300" $COMMON $BTC5 $DATES \
    --momentum-lookback-s 300 --momentum-weight 1.0 \
    --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 \
    --exit-after-s 0
done

disk_ok || exit 0
log "===== cross-market: ETH-5m and BTC-15m on W3 (may stream S3) ====="
run "X_eth5_fade" $COMMON $ETH5 $(window_args W3) \
  --edge-thresholds 0.10,0.16,0.22 --exit-after-s 30
run "X_eth5_aligned" $COMMON $ETH5 $(window_args W3) \
  --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 --exit-after-s 0
run "X_btc15_fade" $COMMON $BTC15 $(window_args W3) \
  --edge-thresholds 0.10,0.16,0.22 --exit-after-s 30
run "X_btc15_aligned" $COMMON $BTC15 $(window_args W3) \
  --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 --exit-after-s 0

log "OVERNIGHT_DONE"
