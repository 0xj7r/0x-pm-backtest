#!/bin/bash
# btclong_xrp W3 (2026-05-07..05-18). BTC longer horizon = 4h (no 1h market exists
# on Polymarket; master parquet horizons are 5m/15m/4h only). XRP full: 5m/15m/4h.
# BTC uses BTC perp (correct), XRP is SPOT-ONLY (perp-price-weight 0).
# One cell at a time; W3 tick cache already present so no ingestion. NEVER 05-19..06-30.
set -uo pipefail
cd "$(dirname "$0")/../.."
BIN=./target/fast/pm-app
OUT=data/runs/alpha/btclong_xrp
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 20 ] || { log "DISK GUARD: ${free_gb}GB free, stopping"; return 1; }
}

DATES="--date-start 2026-05-07 --date-end 2026-05-18"
COMMON="--local-cache-dir data/cache --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --fee-curve-rate 0.07 --vol-estimator ewma --ewma-halflife-s 600"

# fade tag manifest prefix perp_args
fade() {
  local tag=$1 mani=$2 pref=$3 perp=$4
  if [ -s "$OUT/${tag}_fade.json" ]; then log "skip ${tag}_fade"; return; fi
  disk_ok || exit 1
  log "run ${tag}_fade"
  "$BIN" alpha $COMMON $DATES $perp --stop-before-close-s 90 \
    --down-assets data/manifests/canonical/down_all.jsonl \
    --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
    --edge-thresholds 0.12 --exit-after-s 0 \
    --markets "$mani" --slug-prefix "$pref" \
    --out-json "$OUT/${tag}_fade.json" --trades-out "$OUT/${tag}_fade.trades.jsonl" \
    > "$OUT/${tag}_fade.log" 2>&1 || log "WARN ${tag}_fade failed"
}

# lane tag manifest prefix perp_args
lane() {
  local tag=$1 mani=$2 pref=$3 perp=$4
  if [ -s "$OUT/${tag}_lane.json" ]; then log "skip ${tag}_lane"; return; fi
  disk_ok || exit 1
  log "run ${tag}_lane"
  "$BIN" alpha $COMMON $DATES $perp \
    --aligned-mode --align-min-mid 0.85 --edge-thresholds 0.02 \
    --enter-within-close-s 120 --stop-before-close-s 5 --exit-after-s 0 \
    --markets "$mani" --slug-prefix "$pref" \
    --out-json "$OUT/${tag}_lane.json" --trades-out "$OUT/${tag}_lane.trades.jsonl" \
    > "$OUT/${tag}_lane.log" 2>&1 || log "WARN ${tag}_lane failed"
}

BTCPERP="--perp-symbol BTCUSDT --perp-price-weight 0.75"
SPOT="--perp-price-weight 0"
MAN=data/manifests/canonical

# GOAL A: BTC longer horizon (4h primary, 15m intermediate). BTC perp.
fade btc4h  $MAN/btc-updown-4h_up.jsonl  btc-updown-4h-  "$BTCPERP"
fade btc15m $MAN/btc-updown-15m_up.jsonl btc-updown-15m- "$BTCPERP"

# GOAL B: XRP full. SPOT ONLY.
fade xrp5   $MAN/xrp-updown-5m_up.jsonl  xrp-updown-5m-  "$SPOT"
fade xrp15  $MAN/xrp-updown-15m_up.jsonl xrp-updown-15m- "$SPOT"
fade xrp4h  $MAN/xrp-updown-4h_up.jsonl  xrp-updown-4h-  "$SPOT"

lane xrp5   $MAN/xrp-updown-5m_up.jsonl  xrp-updown-5m-  "$SPOT"
lane xrp15  $MAN/xrp-updown-15m_up.jsonl xrp-updown-15m- "$SPOT"
lane xrp4h  $MAN/xrp-updown-4h_up.jsonl  xrp-updown-4h-  "$SPOT"

log ALL_DONE
