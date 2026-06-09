#!/usr/bin/env bash
# pm-alpha overnight program (2026-06-09): risk-config tune -> verify ->
# multi-market manifests (metadata) -> per-family hunts -> June finale.
# Protocol: tune windows only for selection; May 19-28 verified once per
# decision; June 1-7 reserved for the single final frozen config.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
MAY=data/manifests/may2026_focused/markets_btc.jsonl
MM=data/manifests/multimarket
NIGHT=data/runs/alpha/overnight
mkdir -p "$NIGHT" "$MM"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

run_alpha() { # name, then extra args
  local name=$1; shift
  "$BIN" alpha --local-cache-dir data/cache \
    --out-json "$NIGHT/$name.json" --trades-out "$NIGHT/$name.trades.jsonl" \
    "$@" > "$NIGHT/$name.log" 2>&1
}

# Phase B: risk-config grid on TUNE window (May 7-18), frozen signal config.
log "phase B: risk grid on tune window"
pids=()
for EXIT_S in 0 30 60 120; do
  for SKIP in 0 1; do
    NAME="riskB_exit${EXIT_S}_skip${SKIP}"
    EXTRA=()
    [ "$SKIP" = 1 ] && EXTRA+=(--skip-calm)
    run_alpha "$NAME" --markets "$MAY" \
      --date-start 2026-05-07 --date-end 2026-05-18 \
      --latency-ms 150 --edge-thresholds 0.12 --vol-lookback-s 3600 \
      --exit-after-s "$EXIT_S" "${EXTRA[@]}" &
    pids+=($!)
  done
done
for p in "${pids[@]}"; do wait "$p" || log "WARN riskB job failed"; done

python3 scripts/alpha_overnight_pick.py risk "$NIGHT" > "$NIGHT/riskB_choice.env" || { log "risk pick failed"; exit 1; }
. "$NIGHT/riskB_choice.env"   # sets CHOSEN_EXIT_S, CHOSEN_SKIP
log "phase B chosen: exit=$CHOSEN_EXIT_S skip_calm=$CHOSEN_SKIP"
SKIPFLAG=()
[ "$CHOSEN_SKIP" = 1 ] && SKIPFLAG+=(--skip-calm)

# Phase B2: single verification on TEST window (May 19-28).
log "phase B2: verify risk config on test window"
run_alpha "riskB2_verify_test" --markets "$MAY" \
  --date-start 2026-05-19 --date-end 2026-05-28 \
  --latency-ms 150 --edge-thresholds 0.12 --vol-lookback-s 3600 \
  --exit-after-s "$CHOSEN_EXIT_S" "${SKIPFLAG[@]}" || log "WARN B2 failed"

# Phase C: metadata manifests for all families (local, no API).
log "phase C: metadata manifests"
ALL_DAYS="2026-05-21 2026-05-22 2026-05-23 2026-05-24 2026-05-25 2026-05-26 2026-05-27 2026-05-28 2026-06-01 2026-06-02 2026-06-03 2026-06-04 2026-06-05 2026-06-06 2026-06-07"
for D in $ALL_DAYS; do
  if [ ! -f "$MM/meta_all_${D}.jsonl" ]; then
    "$BIN" discover-local-cache-book-metadata \
      --cache-dir data/cache --date "$D" --slug-prefix "" \
      --out "$MM/meta_all_${D}.jsonl" >> "$NIGHT/phaseC.log" 2>&1 || log "WARN meta discovery $D failed"
  fi
done
cat "$MM"/meta_all_2026-05-2*.jsonl > "$MM/meta_may21_28.jsonl" 2>/dev/null || true
cat "$MM"/meta_all_2026-06-0*.jsonl > "$MM/meta_june.jsonl" 2>/dev/null || true
python3 scripts/alpha_overnight_pick.py families "$MM" > "$NIGHT/families.txt" || true
log "families found: $(cat "$NIGHT/families.txt" | tr '\n' ' ')"
# Label validation: inferred-vs-API where the availability cache has answers.
python3 scripts/alpha_overnight_pick.py labelcheck "$MM" > "$NIGHT/labelcheck.txt" 2>&1 || true
log "labelcheck: $(tail -1 "$NIGHT/labelcheck.txt" 2>/dev/null)"

# Phase D: per-family tune (May 21-24) and frozen test (May 25-28).
log "phase D: per-family hunts"
while read -r FAM; do
  [ -z "$FAM" ] && continue
  SAFE=${FAM//-/_}
  run_alpha "D_${SAFE}_tune" --markets "$MM/meta_may21_28.jsonl" \
    --slug-prefix "${FAM}-" --infer-outcome \
    --date-start 2026-05-21 --date-end 2026-05-24 \
    --latency-ms 150 --edge-thresholds 0.08,0.12,0.16 \
    --vol-lookback-s 3600 \
    --exit-after-s "$CHOSEN_EXIT_S" "${SKIPFLAG[@]}" || { log "WARN tune $FAM failed"; continue; }
  THR=$(python3 scripts/alpha_overnight_pick.py best-thr "$NIGHT/D_${SAFE}_tune.json") || continue
  log "family $FAM chosen thr=$THR"
  run_alpha "D_${SAFE}_test" --markets "$MM/meta_may21_28.jsonl" \
    --slug-prefix "${FAM}-" --infer-outcome \
    --date-start 2026-05-25 --date-end 2026-05-28 \
    --latency-ms 150 --edge-thresholds "$THR" --vol-lookback-s 3600 \
    --exit-after-s "$CHOSEN_EXIT_S" "${SKIPFLAG[@]}" || log "WARN test $FAM failed"
done < "$NIGHT/families.txt"

# Phase E: June finale — frozen config only, every family with June data.
log "phase E: June holdout finale (frozen)"
while read -r FAM; do
  [ -z "$FAM" ] && continue
  SAFE=${FAM//-/_}
  THR=0.12
  [ -f "$NIGHT/D_${SAFE}_tune.json" ] && THR=$(python3 scripts/alpha_overnight_pick.py best-thr "$NIGHT/D_${SAFE}_tune.json" 2>/dev/null || echo 0.12)
  run_alpha "JUNE_${SAFE}" --markets "$MM/meta_june.jsonl" \
    --slug-prefix "${FAM}-" --infer-outcome \
    --date-start 2026-06-01 --date-end 2026-06-07 \
    --latency-ms 150 --edge-thresholds "$THR" --vol-lookback-s 3600 \
    --exit-after-s "$CHOSEN_EXIT_S" "${SKIPFLAG[@]}" || log "WARN june $FAM failed"
done < "$NIGHT/families.txt"

python3 scripts/alpha_overnight_pick.py summary "$NIGHT" > "$NIGHT/SUMMARY.txt" 2>&1 || true
log "OVERNIGHT_DONE"
