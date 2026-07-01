#!/bin/bash
# Strategy hunt @ $1K bankroll — systematic multi-family backtest launcher.
# Protocol: W3 fail-fast triage, then W2, then W1. NEVER touch 2026-05-19+.
# Selection on W1/W2/W3 only; May 19-28 test and June are sealed.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN="${BIN:-./target/fast/pm-app}"
OUT="${OUT:-data/runs/strategy_hunt_1k}"
mkdir -p "$OUT"
MAXJOBS="${MAXJOBS:-4}"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 8 ] || { log "DISK GUARD: ${free_gb}GB free, stopping"; return 1; }
}

throttle() { while [ "$(jobs -rp | wc -l)" -ge "$MAXJOBS" ]; do sleep 15; done; }

# $1K bankroll: $25 clips = 2.5% per market (alpha-hunt-002 deployment frame)
NOTIONAL=25
COMMON="--local-cache-dir data/cache --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 --fee-curve-rate 0.07 --notional-usdc ${NOTIONAL}"
CHAMP="--perp-symbol BTCUSDT --perp-price-weight 0.75 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08 --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60"

BTC5="--markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m-"
BTC15="--markets data/manifests/canonical/btc-updown-15m_up.jsonl --slug-prefix btc-updown-15m-"
ETH5="--markets data/manifests/canonical/eth-updown-5m_up.jsonl --slug-prefix eth-updown-5m-"
SOL5="--markets data/manifests/canonical/sol-updown-5m_up.jsonl --slug-prefix sol-updown-5m-"
XRP5="--markets data/manifests/canonical/xrp-updown-5m_up.jsonl --slug-prefix xrp-updown-5m-"
DOGE5="--markets data/manifests/canonical/doge-updown-5m_up.jsonl --slug-prefix doge-updown-5m-"
HYPE5="--markets data/manifests/canonical/hype-updown-5m_up.jsonl --slug-prefix hype-updown-5m-"

window_args() {
  case $1 in
    W1) echo "--date-start 2026-02-12 --date-end 2026-03-31" ;;
    W2) echo "--date-start 2026-04-01 --date-end 2026-04-30" ;;
    W3) echo "--date-start 2026-05-07 --date-end 2026-05-18" ;;
  esac
}

run_alpha() {
  local name=$1; shift
  if [ -s "$OUT/${name}.json" ]; then log "skip $name"; return; fi
  disk_ok || exit 1
  throttle
  log "run $name"
  "$BIN" alpha "$@" --out-json "$OUT/${name}.json" \
    --trades-out "$OUT/${name}.trades.jsonl" > "$OUT/${name}.log" 2>&1 \
    || log "WARN $name failed" &
}

run_wf() {
  local name=$1; shift
  if [ -s "$OUT/${name}/summary.json" ]; then log "skip $name"; return; fi
  disk_ok || exit 1
  throttle
  log "run $name"
  mkdir -p "$OUT/${name}"
  "$BIN" walk-forward "$@" \
    --out-markets "$OUT/${name}/markets.jsonl" \
    --out-summary "$OUT/${name}/summary.json" > "$OUT/${name}.log" 2>&1 \
    || log "WARN $name failed" &
}

# Families to hunt (pass FAMILIES env to subset, e.g. FAMILIES=champion,btc15)
FAMILIES="${FAMILIES:-champion,btc15,alts,bonereaper,paired}"

for W in W3 W2 W1; do
  DATES=$(window_args $W)
  log "===== window $W ====="

  if [[ "$FAMILIES" == *champion* ]]; then
    run_alpha "${W}_champion_1k" $COMMON $BTC5 $DATES $CHAMP
    # F3b: basis momentum on finalized pw0.75
    run_alpha "${W}_f3b_basis" $COMMON $BTC5 $DATES $CHAMP \
      --basis-mom-agree 1.25 --basis-mom-disagree 0.75
    # Dual-trigger: late secondary threshold
    run_alpha "${W}_dual_trigger" $COMMON $BTC5 $DATES $CHAMP \
      --edge-thresholds 0.16,0.12 --min-marginal-edge 0.06
  fi

  if [[ "$FAMILIES" == *btc15* ]]; then
    run_alpha "${W}_btc15_1k" $COMMON $BTC15 $DATES $CHAMP
  fi

  if [[ "$FAMILIES" == *alts* ]]; then
    # Perp-led belief on alts (feed leadership hypothesis)
    run_alpha "${W}_sol5_perp" $COMMON $SOL5 $DATES \
      --perp-symbol SOLUSDT --perp-price-weight 0.75 --edge-thresholds 0.16 \
      --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 --max-clips 1
    run_alpha "${W}_xrp5_perp" $COMMON $XRP5 $DATES \
      --perp-symbol XRPUSDT --perp-price-weight 0.75 --edge-thresholds 0.16 \
      --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 --max-clips 1
    # Inverse: aligned follow on alts
    run_alpha "${W}_sol5_aligned" $COMMON $SOL5 $DATES \
      --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 \
      --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60
    run_alpha "${W}_doge5_fade" $COMMON $DOGE5 $DATES $CHAMP
    run_alpha "${W}_hype5_fade" $COMMON $HYPE5 $DATES $CHAMP
  fi

  if [[ "$FAMILIES" == *bonereaper* ]]; then
    run_wf "${W}_br2_1k" \
      --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
      --local-cache-dir data/cache \
      --date-start $(echo $DATES | awk '{print $2}') \
      --date-end $(echo $DATES | awk '{print $4}') \
      --strategies bonereaper_v2 \
      --starting-cash 1000 \
      --portfolio-mode \
      --clip-fraction-of-equity 0.025 \
      --max-clip-usdc 30 \
      --max-per-market-exposure-frac 0.25 \
      --use-outcome-label \
      --spot-symbol BTCUSDT \
      --replay-sample-ms 1000
  fi

  if [[ "$FAMILIES" == *paired* ]]; then
    run_wf "${W}_paired_mm_1k" \
      --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
      --local-cache-dir data/cache \
      --date-start $(echo $DATES | awk '{print $2}') \
      --date-end $(echo $DATES | awk '{print $4}') \
      --strategies paired_mm \
      --starting-cash 1000 \
      --portfolio-mode \
      --clip-fraction-of-equity 0.025 \
      --max-clip-usdc 30 \
      --use-outcome-label \
      --spot-symbol BTCUSDT \
      --replay-sample-ms 1000
  fi

  wait
  log "===== window $W complete ====="
done

log "ALL_DONE — results in $OUT"