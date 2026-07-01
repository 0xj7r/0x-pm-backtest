#!/usr/bin/env bash
# Systematic strategy matrix — fresh validation, $1K bankroll.
# Usage: WINDOWS=VERIFY STRATEGIES=F1,F2 MARKETS=btc5m,eth5m ./scripts/research/strategy_hunt_matrix.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
BIN="${BIN:-./target/fast/pm-app}"
OUT="${OUT:-data/runs/strategy_hunt}"
mkdir -p "$OUT"
MAXJOBS="${MAXJOBS:-3}"
NOTIONAL="${NOTIONAL:-25}"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

disk_ok() {
  local free_gb
  free_gb=$(df -g /System/Volumes/Data | tail -1 | awk '{print $4}')
  [ "$free_gb" -ge 6 ] || { log "DISK GUARD: ${free_gb}GB free"; return 1; }
}
throttle() { while [ "$(jobs -rp | wc -l)" -ge "$MAXJOBS" ]; do sleep 20; done; }

COMMON="--local-cache-dir data/cache --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --vol-lookback-s 3600 --stop-before-close-s 90 --fee-curve-rate 0.07 --notional-usdc ${NOTIONAL}"

manifest_args() {
  case $1 in
    btc5m)  echo "--markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m-" ;;
    btc15m) echo "--markets data/manifests/canonical/btc-updown-15m_up.jsonl --slug-prefix btc-updown-15m-" ;;
    eth5m)  echo "--markets data/manifests/canonical/eth-updown-5m_up.jsonl --slug-prefix eth-updown-5m-" ;;
    eth15m) echo "--markets data/manifests/canonical/eth-updown-15m_up.jsonl --slug-prefix eth-updown-15m-" ;;
    sol5m)  echo "--markets data/manifests/canonical/sol-updown-5m_up.jsonl --slug-prefix sol-updown-5m-" ;;
    xrp5m)  echo "--markets data/manifests/canonical/xrp-updown-5m_up.jsonl --slug-prefix xrp-updown-5m-" ;;
    doge5m) echo "--markets data/manifests/canonical/doge-updown-5m_up.jsonl --slug-prefix doge-updown-5m-" ;;
    hype5m) echo "--markets data/manifests/canonical/hype-updown-5m_up.jsonl --slug-prefix hype-updown-5m-" ;;
    *) return 1 ;;
  esac
}

perp_args() {
  case $1 in
    btc5m|btc15m) echo "--perp-symbol BTCUSDT --perp-price-weight 0.75 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08" ;;
    eth5m|eth15m) echo "--perp-symbol ETHUSDT --perp-price-weight 0.75 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.08" ;;
    sol5m) echo "--perp-symbol SOLUSDT --perp-price-weight 0.75 --max-clips 1 --min-marginal-edge 0.08" ;;
    xrp5m) echo "--perp-symbol XRPUSDT --perp-price-weight 0.75 --max-clips 1 --min-marginal-edge 0.08" ;;
    *) echo "--max-clips 1 --min-marginal-edge 0.08" ;;
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

run_alpha() {
  local tag=$1; shift
  local json="$OUT/${tag}.json"
  [ -s "$json" ] && { log "skip $tag"; return; }
  disk_ok || exit 1
  throttle
  log "alpha $tag"
  "$BIN" alpha "$@" --out-json "$json" \
    --trades-out "$OUT/${tag}.trades.jsonl" > "$OUT/${tag}.log" 2>&1 &
}

run_wf() {
  local tag=$1; shift
  local dir="$OUT/${tag}"
  [ -s "$dir/summary.json" ] && { log "skip $tag"; return; }
  disk_ok || exit 1
  throttle
  mkdir -p "$dir"
  log "wf $tag"
  "$BIN" walk-forward "$@" \
    --out-markets "$dir/markets.jsonl" \
    --out-summary "$dir/summary.json" > "$dir/run.log" 2>&1 &
}

wants() { [[ "${STRATEGIES}" == "all" || "${STRATEGIES}" == *"$1"* ]]; }

WINDOWS="${WINDOWS:-VERIFY}"
STRATEGIES="${STRATEGIES:-all}"
MARKETS="${MARKETS:-btc5m,btc15m,eth5m,sol5m,xrp5m}"

for WIN in ${WINDOWS//,/ }; do
  DATES=$(window_dates "$WIN")
  log "======== $WIN ========"
  for MKT in ${MARKETS//,/ }; do
    M=$(manifest_args "$MKT") || { log "unknown market $MKT"; continue; }
    P=$(perp_args "$MKT")

    if wants F1; then
      run_alpha "${WIN}_F1_fade_${MKT}" $COMMON $M $DATES $P \
        --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60
    fi
    if wants F2; then
      run_alpha "${WIN}_F2_aligned_${MKT}" $COMMON $M $DATES $P \
        --aligned-mode --align-min-mid 0.55 --edge-thresholds 0.04,0.08 \
        --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60
    fi
    if wants F3; then
      run_alpha "${WIN}_F3_maker_${MKT}" $COMMON $M $DATES $P \
        --edge-thresholds 0.16 --exit-after-s 0 --maker-entry-offset 0.01
    fi
    if wants F4; then
      run_alpha "${WIN}_F4_late_${MKT}" $COMMON $M $DATES $P \
        --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 \
        --enter-within-close-s 120
    fi
    if wants F5; then
      run_alpha "${WIN}_F5_pair_${MKT}" $COMMON $M $DATES $P \
        --edge-thresholds 0.16 --exit-after-s 30 --pair-completion-margin 0.02
    fi
    if wants F6; then
      run_alpha "${WIN}_F6_expanded_${MKT}" $COMMON $M $DATES $P \
        --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 --skip-calm
      run_alpha "${WIN}_F6_calm_${MKT}" $COMMON $M $DATES $P \
        --edge-thresholds 0.16 --exit-after-s 30 --exit-at-mid --passive-exit-timeout-s 60 --only-calm
    fi
    if wants F7; then
      run_alpha "${WIN}_F7_hold_${MKT}" $COMMON $M $DATES $P \
        --edge-thresholds 0.16 --exit-after-s 0
    fi

    if [ "$MKT" = "btc5m" ]; then
      DS=$(echo $DATES | awk '{print $2}')
      DE=$(echo $DATES | awk '{print $4}')
      WF="--markets data/manifests/canonical/btc-updown-5m_up.jsonl --local-cache-dir data/cache --date-start $DS --date-end $DE --starting-cash 1000 --portfolio-mode --clip-fraction-of-equity 0.025 --max-clip-usdc 30 --use-outcome-label --spot-symbol BTCUSDT --replay-sample-ms 1000"
      if wants F1; then
        run_wf "${WIN}_F1_exo_${MKT}" $WF --strategies exo_fade
      fi
      if wants W1; then run_wf "${WIN}_W1_bte_${MKT}" $WF --strategies back_to_explore; fi
      if wants W2; then run_wf "${WIN}_W2_paired_${MKT}" $WF --strategies paired_mm; fi
      if wants W3; then run_wf "${WIN}_W3_br2_${MKT}" $WF --strategies bonereaper_v2; fi
    fi
  done
  wait
  log "======== $WIN done ========"
done
log "MATRIX_DONE $OUT"