#!/usr/bin/env bash
# Hardened Feb-Apr rerun: canonical true labels + real NO ladders + fixed
# strike + tick cache (written through for future instant reruns).
# Waits for hunt003b to finish to avoid S3 contention.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=data/runs/alpha/febapr_hardened
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

until [ -f data/runs/alpha/hunt003b/SUMMARY.txt ] || ! pgrep -f alpha_short_horizons > /dev/null; do
  sleep 120
done
log "hunt003b finished; starting hardened feb-apr"

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
  local name=$1 start=$2 end=$3
  if [ -f "$OUT/$name.json" ]; then log "shard $name exists"; return; fi
  # shellcheck disable=SC1091
  source data/.aws_session.env
  log "shard $name starting"
  "$BIN" alpha --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
    --slug-prefix btc-updown-5m- \
    --down-assets data/manifests/canonical/down_all.jsonl \
    --tick-cache-dir data/cache/ticks \
    --date-start "$start" --date-end "$end" \
    --latency-ms 150 --edge-thresholds 0.12,0.16 --vol-lookback-s 3600 \
    --exit-after-s 30 \
    --out-json "$OUT/$name.json" --trades-out "$OUT/$name.trades.jsonl" \
    > "$OUT/$name.log" 2>&1 || log "WARN shard $name failed"
  log "shard $name done"
}

mint || { log "mint failed"; exit 1; }
pids=()
for SPEC in \
  "feb1 2026-02-12 2026-02-21" "feb2 2026-02-22 2026-02-28" \
  "mar1 2026-03-01 2026-03-10" "mar2 2026-03-11 2026-03-20" \
  "mar3 2026-03-21 2026-03-31" "apr1 2026-04-01 2026-04-10" \
  "apr2 2026-04-11 2026-04-20" "apr3 2026-04-21 2026-04-30"; do
  set -- $SPEC
  run_shard "$1" "$2" "$3" &
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p" || true; done
python3 scripts/alpha_overnight_pick.py summary "$OUT" > "$OUT/SUMMARY.txt" 2>&1 || true
log FEBAPR_HARDENED_DONE
