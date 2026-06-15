#!/bin/bash
# Run ONE (horizon, window) ETH cell end-to-end with strict disk discipline:
# sync ETH-only up+down book tapes + ETHUSDT spot for the window, run the
# fade-candidate (spot-only, perp 0), score, then DELETE the synced tapes.
# Usage: scripts/eth_complete_cell.sh <5m|15m|1h|4h> <W1|W2|W3> [keep]
set -uo pipefail
cd "$(dirname "$0")/.."

HZ=$1; W=$2; KEEP=${3:-}
BIN=./target/fast/pm-app
PROFILE=visumlabs
S3=s3://pm-research-data-prod
BOOKBASE=raw/telonex/exchange=polymarket/channel=book_snapshot_25
SPOTBASE=raw/binance/exchange=binance/channel=agg_trades/symbol=ETHUSDT
CACHE=data/cache
OUT=data/runs/alpha/eth_complete
MANI=data/manifests/canonical/eth-updown-${HZ}_up.jsonl
DOWN=data/manifests/canonical/down_all.jsonl
mkdir -p "$OUT"
NAME="eth${HZ}_${W}"
log(){ echo "[$(date -u +%H:%M:%S)] $*"; }

case $W in
  W1) DS=2026-02-12; DE=2026-03-31 ;;
  W2) DS=2026-04-01; DE=2026-04-30 ;;
  W3) DS=2026-05-07; DE=2026-05-18 ;;
  *) echo "bad window $W"; exit 2 ;;
esac
case $HZ in
  5m) WSEC=300 ;; 15m) WSEC=900 ;; 1h) WSEC=3600 ;; 4h) WSEC=14400 ;;
  *) echo "bad horizon $HZ"; exit 2 ;;
esac

disk_gb(){ df -g /System/Volumes/Data | tail -1 | awk '{print $4}'; }
disk_ok(){ local g; g=$(disk_gb); [ "$g" -ge 20 ] || { log "DISK GUARD ${g}GB<20, abort"; exit 1; }; }

log "=== cell $NAME  $DS..$DE  wsec=$WSEC ==="
disk_ok

# 1. Build (date, asset_id) list: up (manifest) + down (down_all, eth-updown-HZ-)
TMP=$(mktemp -d)
python3 - "$MANI" "$DOWN" "$HZ" "$DS" "$DE" "$TMP/keys.txt" "$TMP/spotdates.txt" <<'PY'
import json,sys
mani,down,hz,ds,de,outk,outs=sys.argv[1:8]
keys=set(); dates=set()
for l in open(mani):
    r=json.loads(l)
    if ds<=r['date']<=de:
        keys.add((r['date'],r['asset_id'])); dates.add(r['date'])
pref=f"eth-updown-{hz}-"
for l in open(down):
    r=json.loads(l)
    if r['slug'].startswith(pref) and ds<=r['date']<=de:
        keys.add((r['date'],r['asset_id'])); dates.add(r['date'])
with open(outk,'w') as f:
    for d,a in sorted(keys): f.write(f"{d}\t{a}\n")
with open(outs,'w') as f:
    for d in sorted(dates): f.write(d+"\n")
print(f"keys={len(keys)} dates={len(dates)}")
PY
NKEYS=$(wc -l < "$TMP/keys.txt")
log "to sync: $NKEYS book tapes, $(wc -l < "$TMP/spotdates.txt") spot days"

# 2. Download book tapes in parallel (skip if already local).
sync_one(){
  local d=$1 a=$2
  local dir="$CACHE/$BOOKBASE/date=$d/asset_id=$a"
  local f="$dir/${a}_${d}_book_snapshot_25.parquet"
  [ -s "$f" ] && return 0
  mkdir -p "$dir"
  aws s3 cp "$S3/$BOOKBASE/date=$d/asset_id=$a/${a}_${d}_book_snapshot_25.parquet" "$f" \
    --profile "$PROFILE" --no-progress >/dev/null 2>&1 || rm -f "$f"
}
export -f sync_one; export CACHE BOOKBASE S3 PROFILE
log "downloading books..."
awk -F'\t' '{print $1" "$2}' "$TMP/keys.txt" | xargs -P 48 -n 2 bash -c 'sync_one "$0" "$1"'

# 3. Download ETHUSDT spot days (skip if local).
while read -r d; do
  sf="$CACHE/$SPOTBASE/date=$d/ETHUSDT-aggTrades-$d.parquet"
  [ -s "$sf" ] && continue
  mkdir -p "$CACHE/$SPOTBASE/date=$d"
  aws s3 cp "$S3/$SPOTBASE/date=$d/ETHUSDT-aggTrades-$d.parquet" "$sf" \
    --profile "$PROFILE" --no-progress >/dev/null 2>&1 || rm -f "$sf"
  # spot needs prior day for vol lookback warmup; fetch d-1 too
  pd=$(python3 -c "import datetime as t;print((t.date.fromisoformat('$d')-t.timedelta(days=1)).isoformat())")
  pf="$CACHE/$SPOTBASE/date=$pd/ETHUSDT-aggTrades-$pd.parquet"
  if [ ! -s "$pf" ]; then mkdir -p "$CACHE/$SPOTBASE/date=$pd"
    aws s3 cp "$S3/$SPOTBASE/date=$pd/ETHUSDT-aggTrades-$pd.parquet" "$pf" \
      --profile "$PROFILE" --no-progress >/dev/null 2>&1 || rm -f "$pf"; fi
done < "$TMP/spotdates.txt"

NB=$(find "$CACHE/$BOOKBASE" -path "*date=20*" -name "*_book_snapshot_25.parquet" 2>/dev/null | wc -l)
log "books on disk now (all dates): $NB ; free=$(disk_gb)GB"
disk_ok

# 4. Run.
log "running alpha..."
$BIN alpha \
  --local-cache-dir "$CACHE" --down-assets "$DOWN" \
  --tick-cache-dir "$CACHE/ticks" --latency-ms 150 --vol-lookback-s 3600 \
  --stop-before-close-s 90 --fee-curve-rate 0.07 --vol-estimator ewma --ewma-halflife-s 600 \
  --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 --edge-thresholds 0.12 \
  --exit-after-s 0 --perp-price-weight 0 \
  --markets "$MANI" --slug-prefix "eth-updown-${HZ}-" \
  --date-start "$DS" --date-end "$DE" \
  --out-json "$OUT/${NAME}.json" \
  --trades-out "$OUT/${NAME}.trades.jsonl" > "$OUT/${NAME}.log" 2>&1
log "run done. tail:"
grep -E "markets run|latency thresh|150ms|real-NO" "$OUT/${NAME}.log" | tail -6

# 5. Score.
python3 scripts/eth_complete_score.py "$OUT/${NAME}.trades.jsonl" 3.0 | tee "$OUT/${NAME}.score.txt"

# 6. Delete synced book tapes for the window dates (unless KEEP).
#    Only delete dates NOT in the W3-local set (protect existing full cache).
if [ "$KEEP" != "keep" ]; then
  log "deleting synced book tapes for $W dates (protecting W3 May7-18)..."
  while read -r d; do
    case "$d" in 2026-05-0[7-9]|2026-05-1[0-8]) continue ;; esac
    find "$CACHE/$BOOKBASE/date=$d" -mindepth 1 -delete 2>/dev/null
    find "$CACHE/$BOOKBASE/date=$d" -maxdepth 0 -empty -delete 2>/dev/null
  done < "$TMP/spotdates.txt"
  log "free after cleanup=$(disk_gb)GB"
fi
find "$TMP" -mindepth 1 -delete 2>/dev/null; find "$TMP" -maxdepth 0 -delete 2>/dev/null
log "=== cell $NAME complete ==="
