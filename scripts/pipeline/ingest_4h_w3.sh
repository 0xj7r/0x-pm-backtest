#!/bin/bash
# Targeted W3 (05-07..05-18) ingest of book_snapshot_25 + trades for the
# asset_ids in a manifest, partition by partition (small footprint). Used for
# 4h (and 15m XRP) cells whose books were never synced to the local cache.
# Usage: ingest_4h_w3.sh <manifest.jsonl>
set -uo pipefail
cd "$(dirname "$0")/../.."
MANI="${1:?usage: ingest_4h_w3.sh <manifest>}"
BUCKET="pm-research-data-prod"
CACHE="data/cache/raw/telonex/exchange=polymarket"
export AWS_PROFILE=visumlabs
W3="2026-05-07 2026-05-08 2026-05-09 2026-05-10 2026-05-11 2026-05-12 2026-05-13 2026-05-14 2026-05-15 2026-05-16 2026-05-17 2026-05-18"

python3 - "$MANI" "$W3" <<'PY' > /tmp/ingest_pairs.txt
import json,sys
mani=sys.argv[1]; w3=set(sys.argv[2].split())
for line in open(mani):
    line=line.strip()
    if not line: continue
    v=json.loads(line)
    if v.get("date") in w3:
        print(v["date"], v["asset_id"])
PY
N=$(wc -l < /tmp/ingest_pairs.txt)
echo "pairs to sync: $N"

sync_one() {
  local date=$1 aid=$2
  for ch in book_snapshot_25 trades; do
    local src="s3://${BUCKET}/raw/telonex/exchange=polymarket/channel=${ch}/date=${date}/asset_id=${aid}/"
    local dst="${CACHE}/channel=${ch}/date=${date}/asset_id=${aid}/"
    mkdir -p "$dst"
    aws s3 sync "$src" "$dst" --quiet --no-progress 2>/dev/null
  done
}
export -f sync_one
export BUCKET CACHE AWS_PROFILE

cat /tmp/ingest_pairs.txt | xargs -P 8 -n 2 bash -c 'sync_one "$@"' _
echo "ingest done for $MANI"
