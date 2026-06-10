#!/usr/bin/env bash
# Hunt 003: short-horizon expansion across btc/eth/sol/xrp x 5m/15m.
# Per family: tune thr {0.08,0.12,0.16} on May 21-24 -> frozen test May 25-28
# -> June 1-7 check (fresh data for these families). Real NO via down map,
# exit 30s, 150ms, inferred outcomes. btc/eth use the local cache; sol/xrp
# stream from S3 (spot + book synced there).
# Waits for the Feb-Apr shards to finish before starting (CPU + bandwidth).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
MAN=data/manifests/ingest0608
OUT=data/runs/alpha/hunt003
mkdir -p "$OUT"
log() { echo "[$(date -u +%H:%M:%S)] $*"; }

until [ "$(ls data/runs/alpha/febapr/*.json 2>/dev/null | wc -l | tr -d ' ')" -ge 8 ] \
   || ! pgrep -f "date-start 2026-0[234]" > /dev/null; do
  sleep 120
done
log "feb-apr shards finished; starting hunt 003"

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

run_family() {
  local fam=$1 local_ok=$2
  local safe=${fam//-/_}
  local STORE=()
  if [ "$local_ok" = 1 ]; then
    STORE=(--local-cache-dir data/cache)
  else
    mint || { log "mint failed for $fam"; return 1; }
    # shellcheck disable=SC1091
    source data/.aws_session.env
  fi
  log "family $fam tune"
  "$BIN" alpha "${STORE[@]+"${STORE[@]}"}" \
    --markets "$MAN/${fam}_up.jsonl" --slug-prefix "${fam}-" --infer-outcome \
    --down-assets "$MAN/down_all.jsonl" --tick-cache-dir data/cache/ticks \
    --date-start 2026-05-21 --date-end 2026-05-24 \
    --latency-ms 150 --edge-thresholds 0.08,0.12,0.16 --vol-lookback-s 3600 \
    --exit-after-s 30 \
    --out-json "$OUT/${safe}_tune.json" --trades-out "$OUT/${safe}_tune.trades.jsonl" \
    > "$OUT/${safe}_tune.log" 2>&1 || { log "WARN tune $fam failed"; return 1; }
  local THR
  THR=$(python3 scripts/alpha_overnight_pick.py best-thr "$OUT/${safe}_tune.json") || return 1
  log "family $fam thr=$THR test"
  "$BIN" alpha "${STORE[@]+"${STORE[@]}"}" \
    --markets "$MAN/${fam}_up.jsonl" --slug-prefix "${fam}-" --infer-outcome \
    --down-assets "$MAN/down_all.jsonl" --tick-cache-dir data/cache/ticks \
    --date-start 2026-05-25 --date-end 2026-05-28 \
    --latency-ms 150 --edge-thresholds "$THR" --vol-lookback-s 3600 \
    --exit-after-s 30 \
    --out-json "$OUT/${safe}_test.json" --trades-out "$OUT/${safe}_test.trades.jsonl" \
    > "$OUT/${safe}_test.log" 2>&1 || log "WARN test $fam failed"
  log "family $fam june"
  "$BIN" alpha "${STORE[@]+"${STORE[@]}"}" \
    --markets "$MAN/${fam}_up.jsonl" --slug-prefix "${fam}-" --infer-outcome \
    --down-assets "$MAN/down_all.jsonl" --tick-cache-dir data/cache/ticks \
    --date-start 2026-06-01 --date-end 2026-06-07 \
    --latency-ms 150 --edge-thresholds "$THR" --vol-lookback-s 3600 \
    --exit-after-s 30 \
    --out-json "$OUT/${safe}_june.json" --trades-out "$OUT/${safe}_june.trades.jsonl" \
    > "$OUT/${safe}_june.log" 2>&1 || log "WARN june $fam failed"
}

# Local-cache families in parallel (book on disk), then S3 families serially
# per credential freshness.
run_family btc-updown-15m 1 &
P1=$!
run_family eth-updown-15m 1 &
P2=$!
wait $P1 || true
wait $P2 || true
run_family sol-updown-5m 0 || true
run_family xrp-updown-5m 0 || true
run_family sol-updown-15m 0 || true
run_family xrp-updown-15m 0 || true

python3 scripts/alpha_overnight_pick.py summary "$OUT" > "$OUT/SUMMARY.txt" 2>&1 || true
log HUNT003_DONE
