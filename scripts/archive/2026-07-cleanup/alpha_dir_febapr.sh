#!/usr/bin/env bash
# Directional dataset generation over Feb-Apr (the trending cohort):
# 8 parallel S3-streaming shards, perp complex loaded, DirSamples emitted,
# tick caches written through (last expensive Feb-Apr pass).
# Each shard trains on its first N-1 days (samples come from the train
# pass) with the final day as the throwaway eval split.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=data/runs/alpha/dir
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

# Wait for the perp upload to be visible on S3.
# shellcheck disable=SC1091
source data/.aws_session.env
until aws s3 ls "s3://pm-research-data-prod/raw/binance/exchange=binance/channel=futures_agg_trades/symbol=BTCUSDT/date=2026-04-30/" 2>/dev/null | grep -q parquet; do
  log "waiting for perp S3 sync..."
  sleep 60
done
log "perp data visible on S3; launching shards"

mint() {
  ssh -i ~/.ssh/whale_pair_dublin_ed25519.pem -o ConnectTimeout=15 ubuntu@34.242.101.97 '
    TOK=$(curl -s -X PUT http://169.254.169.254/latest/api/token -H "X-aws-ec2-metadata-token-ttl-seconds: 300")
    curl -s -H "X-aws-ec2-metadata-token: $TOK" http://169.254.169.254/latest/meta-data/iam/security-credentials/instanceRole' \
  | python3 -c "
import json,sys
c=json.load(sys.stdin)
print('export AWS_ACCESS_KEY_ID='+c['AccessKeyId'])
print('export AWS_SECRET_ACCESS_KEY='+c['SecretAccessKey'])
print('export AWS_SESSION_TOKEN='+c['Token'])
print('export AWS_REGION=us-east-1')
" > data/.aws_session.env
  chmod 600 data/.aws_session.env
}

run_shard() {
  local name=$1 start=$2 end=$3 split=$4
  if [ -f "$OUT/dir_${name}.jsonl" ]; then log "shard $name exists"; return; fi
  # shellcheck disable=SC1091
  source data/.aws_session.env
  log "shard $name ($start..$end, train < $split)"
  "$BIN" alpha --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
    --slug-prefix btc-updown-5m- \
    --down-assets data/manifests/canonical/down_all.jsonl \
    --tick-cache-dir data/cache/ticks \
    --date-start "$start" --date-end "$end" \
    --calibrate-split "$split" \
    --latency-ms 150 --edge-thresholds 0.16 --vol-lookback-s 3600 \
    --exit-after-s 30 \
    --perp-symbol BTCUSDT \
    --dir-samples-out "$OUT/dir_${name}.jsonl" \
    > "$OUT/dir_${name}.log" 2>&1 || log "WARN shard $name failed"
  log "shard $name done"
}

mint || { log "mint failed"; exit 1; }
pids=()
run_shard feb1 2026-02-12 2026-02-22 2026-02-22 & pids+=($!)
run_shard feb2 2026-02-22 2026-03-01 2026-03-01 & pids+=($!)
run_shard mar1 2026-03-01 2026-03-11 2026-03-11 & pids+=($!)
run_shard mar2 2026-03-11 2026-03-21 2026-03-21 & pids+=($!)
run_shard mar3 2026-03-21 2026-04-01 2026-04-01 & pids+=($!)
run_shard apr1 2026-04-01 2026-04-11 2026-04-11 & pids+=($!)
run_shard apr2 2026-04-11 2026-04-21 2026-04-21 & pids+=($!)
run_shard apr3 2026-04-21 2026-05-01 2026-05-01 & pids+=($!)
for p in "${pids[@]}"; do wait "$p" || true; done
cat "$OUT"/dir_feb*.jsonl "$OUT"/dir_mar*.jsonl "$OUT"/dir_apr*.jsonl > "$OUT/dir_febapr_all.jsonl"
wc -l "$OUT/dir_febapr_all.jsonl"
log DIR_FEBAPR_DONE
