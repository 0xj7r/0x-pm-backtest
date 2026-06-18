#!/usr/bin/env bash
# First-principles strategy class screens — mechanistically distinct, not fade tuning.
# Prerequisite: python3 scripts/quant_signal_screen.py (t-stat on fee-adjusted edge > 2).
# Signal spec: docs/research/strategy-hunt/05-quant-signals.md
# Usage: WINDOWS=TUNE MARKETS=btc5m ./scripts/first_principles_scan.sh
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/fast/pm-app}"
OUT="${OUT:-data/runs/first_principles}"
mkdir -p "$OUT"
NOTIONAL="${NOTIONAL:-25}"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 5 ] || { log "DISK GUARD: ${free_gb}GB free — abort"; exit 1; }
}

manifest_args() {
  case $1 in
    btc5m)  echo "--markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m-" ;;
    btc15m) echo "--markets data/manifests/canonical/btc-updown-15m_up.jsonl --slug-prefix btc-updown-15m-" ;;
    eth5m)  echo "--markets data/manifests/canonical/eth-updown-5m_up.jsonl --slug-prefix eth-updown-5m-" ;;
    sol5m)  echo "--markets data/manifests/canonical/sol-updown-5m_up.jsonl --slug-prefix sol-updown-5m-" ;;
    *) return 1 ;;
  esac
}

perp_args() {
  case $1 in
    btc5m|btc15m) echo "--perp-symbol BTCUSDT --perp-price-weight 0.75" ;;
    eth5m) echo "--perp-symbol ETHUSDT --perp-price-weight 0.75" ;;
    sol5m) echo "--perp-symbol SOLUSDT --perp-price-weight 0.75" ;;
    *) echo "" ;;
  esac
}

window_dates() {
  case $1 in
    TUNE)    echo "--date-start 2026-02-12 --date-end 2026-04-30" ;;
    VERIFY)  echo "--date-start 2026-05-07 --date-end 2026-05-18" ;;
    HOLDOUT) echo "--date-start 2026-05-19 --date-end 2026-05-28" ;;
    *) echo "unknown window $1" >&2; return 1 ;;
  esac
}

COMMON="--local-cache-dir data/cache --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 --fee-curve-rate 0.07 --notional-usdc ${NOTIONAL}"

run_class() {
  local tag=$1; shift
  local json="$OUT/${tag}.json"
  disk_ok
  if [ -s "$json" ]; then
    log "skip $tag (exists)"
    return
  fi
  log "alpha $tag"
  "$BIN" alpha "$@" --out-json "$json" > "$OUT/${tag}.log" 2>&1
  rm -f "$OUT/${tag}.trades.jsonl" 2>/dev/null || true
}

WINDOWS="${WINDOWS:-TUNE}"
MARKETS="${MARKETS:-btc5m}"

for WIN in ${WINDOWS//,/ }; do
  DATES=$(window_dates "$WIN")
  log "======== $WIN first-principles screen ========"
  for MKT in ${MARKETS//,/ }; do
    M=$(manifest_args "$MKT") || { log "unknown market $MKT"; continue; }
    P=$(perp_args "$MKT")

    # C1 — stale-book fade (reference, not tuning)
    run_class "${WIN}_C1_fade_${MKT}" $COMMON $M $DATES $P \
      --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 \
      --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08

    # C2 — continuation / aligned (perp-led)
    run_class "${WIN}_C2_aligned_${MKT}" $COMMON $M $DATES $P \
      --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.08 \
      --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 \
      --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08

    # C3 — late-window certainty
    run_class "${WIN}_C3_late_fav_${MKT}" $COMMON $M $DATES $P \
      --aligned-mode --align-min-mid 0.85 --enter-within-close-s 120 --stop-before-close-s 5 \
      --edge-thresholds=-1 --exit-after-s 0

    # C4 — tail convexity hedge on fade entries
    run_class "${WIN}_C4_fade_tail_${MKT}" $COMMON $M $DATES $P \
      --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 \
      --tail-max-price 0.10 --tail-frac 0.25 \
      --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08

    # C4b — standalone cheap-side value (belief > ask on tails only)
    run_class "${WIN}_C4b_tail_value_${MKT}" $COMMON $M $DATES $P \
      --edge-thresholds 0.08 --exit-after-s 0 \
      --rearm-edge 0.08 --max-clips 1 --min-marginal-edge 0.04
  done
done

log "done — score with: python3 scripts/score_first_principles.py $OUT"