#!/usr/bin/env bash
# Build the June holdout manifest (slug + outcome per cached asset) via the
# Telonex availability API. Metadata only — no backtest results are produced.
set -euo pipefail
cd "$(dirname "$0")/.."
mkdir -p data/manifests/june2026_holdout
for D in 2026-06-01 2026-06-02 2026-06-03 2026-06-04 2026-06-05 2026-06-06 2026-06-07; do
  ./target/fast/pm-app discover-local-cache-day \
    --cache-dir data/cache \
    --date "$D" \
    --slug-prefix btc-updown-5m- \
    --availability-cache data/manifests/june2026_holdout/availability_cache.jsonl \
    --out "data/manifests/june2026_holdout/markets_btc_${D}.jsonl"
done
cat data/manifests/june2026_holdout/markets_btc_2026-06-0*.jsonl > data/manifests/june2026_holdout/markets_btc_june.jsonl
wc -l data/manifests/june2026_holdout/markets_btc_june.jsonl
echo JUNE_MANIFEST_DONE
