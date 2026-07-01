#!/bin/bash
# Sync ETH book tapes for a (horizon, date-range) into the local cache, foreground.
# Standalone (no exported functions) so it survives xargs under any shell.
# Usage: scripts/pipeline/eth_sync_books.sh <keyfile>   where keyfile lines are "<date>\t<asset_id>"
set -uo pipefail
cd "$(dirname "$0")/../.."
KEYFILE=$1
CACHE=data/cache
BB=raw/telonex/exchange=polymarket/channel=book_snapshot_25
S3=s3://pm-research-data-prod

cat > /tmp/_get_one.sh <<'EOF'
#!/bin/bash
d=$1; a=$2
CACHE=data/cache
BB=raw/telonex/exchange=polymarket/channel=book_snapshot_25
S3=s3://pm-research-data-prod
dir="$CACHE/$BB/date=$d/asset_id=$a"
f="$dir/${a}_${d}_book_snapshot_25.parquet"
[ -s "$f" ] && exit 0
mkdir -p "$dir"
aws s3 cp "$S3/$BB/date=$d/asset_id=$a/${a}_${d}_book_snapshot_25.parquet" "$f" \
  --profile visumlabs --no-progress >/dev/null 2>&1 || rm -f "$f"
EOF
chmod +x /tmp/_get_one.sh
awk -F'[\t ]' '{print $1" "$2}' "$KEYFILE" | xargs -P 48 -n 2 /tmp/_get_one.sh
