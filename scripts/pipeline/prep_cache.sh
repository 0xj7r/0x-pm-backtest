#!/usr/bin/env bash
# Mirror per-date S3 prefixes to local cache so the Rust backtester can read
# from a LocalFileSystem object_store backend instead of S3.
set -uo pipefail

CACHE="${CACHE:-$HOME/go/polymarket-backtest/data/cache}"
BUCKET="pm-research-data-prod"
REGION="${PM_TELONEX_REGION:-us-east-1}"
DAYS=("$@")
if [ ${#DAYS[@]} -eq 0 ]; then
    DAYS=(2026-05-12 2026-05-13 2026-05-14 2026-05-15 2026-05-16 2026-05-17 2026-05-18 2026-05-19 2026-05-20)
fi

mkdir -p "$CACHE"

sync_one() {
    local src="$1"
    local dst="$2"
    local profile_args=()
    if [ -n "${AWS_PROFILE:-}" ]; then
        profile_args=(--profile "$AWS_PROFILE")
    fi
    AWS_REGION="$REGION" aws s3 sync "$src" "$dst" "${profile_args[@]}" --quiet \
        --no-progress --cli-read-timeout 30 --cli-connect-timeout 10 \
        && echo "OK   $src" || echo "FAIL $src"
}
export -f sync_one
export REGION
export AWS_PROFILE

JOBS=()
for d in "${DAYS[@]}"; do
    for ch in book_snapshot_25 trades; do
        SRC="s3://${BUCKET}/raw/telonex/exchange=polymarket/channel=${ch}/date=${d}/"
        DST="${CACHE}/raw/telonex/exchange=polymarket/channel=${ch}/date=${d}/"
        mkdir -p "$DST"
        echo "queue: $SRC"
        JOBS+=("$SRC|$DST")
    done
    SRC="s3://${BUCKET}/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT/date=${d}/"
    DST="${CACHE}/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT/date=${d}/"
    mkdir -p "$DST"
    JOBS+=("$SRC|$DST")
done

echo "Total prefixes: ${#JOBS[@]}"
echo "${JOBS[@]}" | tr ' ' '\n' | xargs -P 8 -I {} bash -c '
    IFS="|" read -r src dst <<< "$0"
    sync_one "$src" "$dst"
' {}

echo
echo "==== final cache size ===="
du -sh "$CACHE"
find "$CACHE" -name "*.parquet" | wc -l
