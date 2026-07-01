#!/bin/bash
# Fast ETH cell, STRICT DISK: process ONE DATE at a time.
# Per date: aws s3 sync full book prefix (fast), sync ETHUSDT spot (day+prior),
# run alpha for that single day, append trades, DELETE that date's books.
# Never holds more than one date's books (~3GB). Protects W3 May7-18 cache.
# Merged trades scored at the end. Usage: scripts/eth_cell_fast.sh <5m|15m|4h> <W1|W2|W3>
set -uo pipefail
cd "$(dirname "$0")/.."
HZ=$1; W=$2
BIN=./target/fast/pm-app
PROFILE=visumlabs
S3=s3://pm-research-data-prod
BB=raw/telonex/exchange=polymarket/channel=book_snapshot_25
SB=raw/binance/exchange=binance/channel=agg_trades/symbol=ETHUSDT
CACHE=data/cache
OUT=data/runs/alpha/eth_complete
MANI=data/manifests/canonical/eth-updown-${HZ}_up.jsonl
DOWN=data/manifests/canonical/down_all.jsonl
mkdir -p "$OUT"
NAME="eth${HZ}_${W}"
log(){ echo "[$(date -u +%H:%M:%S)] $*"; }
disk_gb(){ df -g /System/Volumes/Data | tail -1 | awk '{print $4}'; }

case $W in
  W1) DS=2026-02-12; DE=2026-03-31 ;;
  W2) DS=2026-04-01; DE=2026-04-30 ;;
  W3) DS=2026-05-07; DE=2026-05-18 ;;
  *) echo "bad window"; exit 2 ;;
esac
# Optional explicit sub-range (args 3,4) to chunk a window across foreground calls.
# Trades append to the same NAME file; pass APPEND=1 to not truncate.
if [ -n "${3:-}" ]; then DS=$3; fi
if [ -n "${4:-}" ]; then DE=$4; fi
APPEND=${APPEND:-0}

python3 - "$MANI" "$DS" "$DE" /tmp/_dates_$NAME.txt <<'PY'
import json,sys
mani,ds,de,out=sys.argv[1:5]
dates=set()
for l in open(mani):
    r=json.loads(l)
    if ds<=r['date']<=de: dates.add(r['date'])
open(out,'w').write('\n'.join(sorted(dates)))
print('dates',len(dates))
PY

log "=== $NAME $DS..$DE : per-date sync+run+delete (append=$APPEND) ==="
if [ "$APPEND" != "1" ]; then : > "$OUT/${NAME}.trades.jsonl"; : > "$OUT/${NAME}.daylog"; fi
touch "$OUT/${NAME}.trades.jsonl" "$OUT/${NAME}.daylog"

while read -r d; do
  [ -z "$d" ] && continue
  is_w3=0; case "$d" in 2026-05-0[7-9]|2026-05-1[0-8]) is_w3=1 ;; esac
  bdir="$CACHE/$BB/date=$d"
  have=$(find "$bdir" -name "*_book_snapshot_25.parquet" 2>/dev/null | wc -l | tr -d ' ')
  if [ "$have" -lt 100 ]; then
    aws s3 sync "$S3/$BB/date=$d/" "$bdir/" --profile "$PROFILE" --no-progress >/dev/null 2>&1 || log "sync warn $d"
  fi
  pd=$(python3 -c "import datetime as t;print((t.date.fromisoformat('$d')-t.timedelta(days=1)).isoformat())")
  for sd in "$d" "$pd"; do
    sf="$CACHE/$SB/date=$sd/ETHUSDT-aggTrades-$sd.parquet"
    [ -s "$sf" ] && continue
    mkdir -p "$CACHE/$SB/date=$sd"
    aws s3 cp "$S3/$SB/date=$sd/ETHUSDT-aggTrades-$sd.parquet" "$sf" --profile "$PROFILE" --no-progress >/dev/null 2>&1 || rm -f "$sf"
  done

  $BIN alpha \
    --local-cache-dir "$CACHE" --down-assets "$DOWN" \
    --tick-cache-dir "$CACHE/ticks" --latency-ms 150 --vol-lookback-s 3600 \
    --stop-before-close-s 90 --fee-curve-rate 0.07 --vol-estimator ewma --ewma-halflife-s 600 \
    --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 --edge-thresholds 0.12 \
    --exit-after-s 0 --perp-price-weight 0 \
    --markets "$MANI" --slug-prefix "eth-updown-${HZ}-" \
    --date-start "$d" --date-end "$d" \
    --out-json "/tmp/_day_${NAME}.json" \
    --trades-out "/tmp/_day_${NAME}.trades.jsonl" > "/tmp/_day_${NAME}.log" 2>&1 || log "run warn $d"
  if [ -s "/tmp/_day_${NAME}.trades.jsonl" ]; then cat "/tmp/_day_${NAME}.trades.jsonl" >> "$OUT/${NAME}.trades.jsonl"; fi
  line=$(grep -E "150ms" "/tmp/_day_${NAME}.log" | tail -1)
  echo "$d | $line" >> "$OUT/${NAME}.daylog"
  log "$d done | ${line} | free=$(disk_gb)GB"

  if [ "$is_w3" -eq 0 ]; then
    find "$bdir" -mindepth 1 -delete 2>/dev/null
    find "$bdir" -maxdepth 0 -empty -delete 2>/dev/null
  fi
done < /tmp/_dates_$NAME.txt

log "all days done. scoring merged trades:"
python3 scripts/eth_complete_score.py "$OUT/${NAME}.trades.jsonl" 3.0 | tee "$OUT/${NAME}.score.txt"
log "=== $NAME complete, free=$(disk_gb)GB ==="
