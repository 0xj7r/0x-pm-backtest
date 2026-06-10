#!/usr/bin/env bash
# Build Down-token manifests (local metadata, fast) and rerun the frozen
# May 25-28 btc-5m test + June finale with the REAL NO ladder, for direct
# comparison against the synthetic-NO results.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
MM=data/manifests/multimarket
OUT=data/runs/alpha/realno
mkdir -p "$OUT"

for D in 2026-05-21 2026-05-22 2026-05-23 2026-05-24 2026-05-25 2026-05-26 2026-05-27 2026-05-28 2026-06-01 2026-06-02 2026-06-03 2026-06-04 2026-06-05 2026-06-06 2026-06-07; do
  if [ ! -f "$MM/down_all_${D}.jsonl" ]; then
    "$BIN" discover-local-cache-book-metadata \
      --cache-dir data/cache --date "$D" --slug-prefix "" --token-outcome Down \
      --out "$MM/down_all_${D}.jsonl" >> "$OUT/discovery.log" 2>&1 || echo "WARN down discovery $D failed"
  fi
done
cat "$MM"/down_all_*.jsonl > "$MM/down_all.jsonl"
wc -l "$MM/down_all.jsonl"

# Frozen May 25-28 test, real NO (synthetic comparison: D_btc_updown_5m_test +$2,179)
"$BIN" alpha --local-cache-dir data/cache \
  --markets "$MM/meta_may21_28.jsonl" --slug-prefix btc-updown-5m- --infer-outcome \
  --down-assets "$MM/down_all.jsonl" \
  --date-start 2026-05-25 --date-end 2026-05-28 \
  --latency-ms 150 --edge-thresholds 0.16 --vol-lookback-s 3600 --exit-after-s 30 \
  --out-json "$OUT/may25_28_realno.json" --trades-out "$OUT/may25_28_realno.trades.jsonl" \
  > "$OUT/may25_28_realno.log" 2>&1 || echo "WARN may test failed"
grep -A6 "alpha run:" "$OUT/may25_28_realno.log" | head -8

# June finale, real NO (synthetic comparison: +$11,726)
"$BIN" alpha --local-cache-dir data/cache \
  --markets "$MM/meta_june.jsonl" --slug-prefix btc-updown-5m- --infer-outcome \
  --down-assets "$MM/down_all.jsonl" \
  --date-start 2026-06-01 --date-end 2026-06-07 \
  --latency-ms 150 --edge-thresholds 0.16 --vol-lookback-s 3600 --exit-after-s 30 \
  --out-json "$OUT/june_realno.json" --trades-out "$OUT/june_realno.trades.jsonl" \
  > "$OUT/june_realno.log" 2>&1 || echo "WARN june failed"
grep -A6 "alpha run:" "$OUT/june_realno.log" | head -8
echo REAL_NO_DONE
