#!/bin/bash
# H6 follow-up: 15m hypothesis on sampled Feb-Apr days (backfilled tapes).
# Same flags as batch0612.sh h6 cells; stretches W1S (Feb 15-Mar 28 sample)
# and W2S (Apr 2-28 sample). Holdout 2026-05-19..06-30 never touched.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=data/runs/alpha/batch0612
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 20 ] || { log "DISK GUARD: ${free_gb}GB free, stopping"; return 1; }
}

BTC15="--markets data/manifests/canonical/btc-updown-15m_up.jsonl --slug-prefix btc-updown-15m-"
COMMON="--local-cache-dir data/cache --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 --fee-curve-rate 0.07"
COMBO="--perp-symbol BTCUSDT --perp-price-weight 0.5 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08"
PASSIVE="--exit-at-mid --passive-exit-timeout-s 60"

run() {
  local name=$1; shift
  if [ -s "$OUT/${name}.json" ]; then log "skip $name"; return; fi
  disk_ok || exit 1
  log "run $name"
  "$BIN" alpha "$@" --out-json "$OUT/${name}.json" \
    --trades-out "$OUT/${name}.trades.jsonl" > "$OUT/${name}.log" 2>&1 \
    || log "WARN $name failed"
}

NAME="${1:?usage: h6_followup_0612.sh W1S|W2S}"
case "$NAME" in
  W1S) DATES="--date-start 2026-02-15 --date-end 2026-03-28" ;;
  W2S) DATES="--date-start 2026-04-02 --date-end 2026-04-28" ;;
  *) echo "unknown stretch $NAME"; exit 1 ;;
esac

run "${NAME}_h6_btc15" $COMMON $BTC15 $DATES $COMBO $PASSIVE \
  --edge-thresholds 0.16 --exit-after-s 30
log "done $NAME"
