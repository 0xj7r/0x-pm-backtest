#!/bin/bash
# SOL/XRP-5m W3 (6-day core 05-09..05-14) book-tape validation, SPOT-ONLY.
# Decontaminated: NO --perp-symbol, --perp-price-weight 0 (BTC perp would swamp
# the tiny non-BTC signal). Fade-candidate + lane, mirroring batch0612 base args.
set -uo pipefail
cd "$(dirname "$0")/../.."
BIN=./target/fast/pm-app
OUT=data/runs/alpha/solxrp
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 20 ] || { log "DISK GUARD: ${free_gb}GB free, stopping"; return 1; }
}

DATES="--date-start 2026-05-09 --date-end 2026-05-14"
COMMON="--local-cache-dir data/cache --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --fee-curve-rate 0.07 --vol-estimator ewma --ewma-halflife-s 600 --perp-price-weight 0"

# FADE-CANDIDATE (real NO ladders via down_all): edge 0.12, 2 clips, rearm.
fade() { # tag manifest prefix
  local tag=$1 mani=$2 pref=$3
  if [ -s "$OUT/${tag}_fade.json" ]; then log "skip ${tag}_fade"; return; fi
  disk_ok || exit 1
  log "run ${tag}_fade"
  "$BIN" alpha $COMMON $DATES --stop-before-close-s 90 \
    --down-assets data/manifests/canonical/down_all.jsonl \
    --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 \
    --edge-thresholds 0.12 --exit-after-s 0 \
    --markets "$mani" --slug-prefix "$pref" \
    --out-json "$OUT/${tag}_fade.json" --trades-out "$OUT/${tag}_fade.trades.jsonl" \
    > "$OUT/${tag}_fade.log" 2>&1 || log "WARN ${tag}_fade failed"
}

# LANE (aligned mode, no down-assets): align>=0.85, edge 0.02, enter-late.
lane() { # tag manifest prefix
  local tag=$1 mani=$2 pref=$3
  if [ -s "$OUT/${tag}_lane.json" ]; then log "skip ${tag}_lane"; return; fi
  disk_ok || exit 1
  log "run ${tag}_lane"
  "$BIN" alpha $COMMON $DATES \
    --aligned-mode --align-min-mid 0.85 --edge-thresholds 0.02 \
    --enter-within-close-s 120 --stop-before-close-s 5 --exit-after-s 0 \
    --markets "$mani" --slug-prefix "$pref" \
    --out-json "$OUT/${tag}_lane.json" --trades-out "$OUT/${tag}_lane.trades.jsonl" \
    > "$OUT/${tag}_lane.log" 2>&1 || log "WARN ${tag}_lane failed"
}

fade sol5 data/manifests/canonical/sol-updown-5m_up.jsonl sol-updown-5m-
fade xrp5 data/manifests/canonical/xrp-updown-5m_up.jsonl xrp-updown-5m-
lane sol5 data/manifests/canonical/sol-updown-5m_up.jsonl sol-updown-5m-
lane xrp5 data/manifests/canonical/xrp-updown-5m_up.jsonl xrp-updown-5m-
log ALL_DONE
