#!/usr/bin/env bash
# Build multi-market manifests (all updown families) for May 21-28 from the
# local book cache + Telonex availability API. Slow (API rate limits); run
# overnight. Waits for any in-flight May 21 discovery to finish first.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT=data/manifests/multimarket
mkdir -p "$OUT"

until [ -f "$OUT/markets_all_2026-05-21.jsonl" ]; do sleep 60; done

for D in 2026-05-22 2026-05-23 2026-05-24 2026-05-25 2026-05-26 2026-05-27 2026-05-28; do
  if [ ! -f "$OUT/markets_all_${D}.jsonl" ]; then
    ./target/fast/pm-app discover-local-cache-day \
      --cache-dir data/cache \
      --date "$D" \
      --slug-prefix "" \
      --availability-cache "$OUT/availability_cache.jsonl" \
      --out "$OUT/markets_all_${D}.jsonl"
  fi
  echo "done $D"
done

cat "$OUT"/markets_all_2026-05-*.jsonl > "$OUT/markets_all_may21_28.jsonl"
for FAM in eth-updown-5m btc-updown-15m eth-updown-15m; do
  grep "\"slug\": \"$FAM" "$OUT/markets_all_may21_28.jsonl" > "$OUT/markets_${FAM}.jsonl" || true
  wc -l "$OUT/markets_${FAM}.jsonl"
done
echo MULTIMARKET_MANIFESTS_DONE
