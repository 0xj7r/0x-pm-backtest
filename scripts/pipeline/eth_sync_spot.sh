#!/bin/bash
# Sync ETHUSDT agg_trades spot parquet for given dates (one per line) into cache.
# Usage: scripts/eth_sync_spot.sh <datesfile>
set -uo pipefail
cd "$(dirname "$0")/.."
DATESFILE=$1
CACHE=data/cache
SB=raw/binance/exchange=binance/channel=agg_trades/symbol=ETHUSDT
S3=s3://pm-research-data-prod
while read -r d; do
  [ -z "$d" ] && continue
  f="$CACHE/$SB/date=$d/ETHUSDT-aggTrades-$d.parquet"
  [ -s "$f" ] && continue
  mkdir -p "$CACHE/$SB/date=$d"
  aws s3 cp "$S3/$SB/date=$d/ETHUSDT-aggTrades-$d.parquet" "$f" \
    --profile visumlabs --no-progress >/dev/null 2>&1 || rm -f "$f"
done < "$DATESFILE"
echo "spot sync done"
