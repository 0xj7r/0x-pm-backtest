#!/usr/bin/env bash
# Launch an in-region (us-east-1) spot EC2 that ingests Telonex data straight to
# the S3 mirror, then self-terminates. Pulls per (asset_id,date,channel) parquet
# via scripts/telonex_ingest.py and `aws s3 sync`s into
# s3://pm-research-data-prod/raw/telonex/ (read by --local-cache-dir / TelonexStore).
#
# Why in-region: the full multi-asset history is ~300GB / ~1M files — too big for
# a laptop. Inbound transfer to EC2 is free; EC2->S3 same-region is free; spot +
# self-terminate keeps it to a few dollars.
#
# Usage:
#   AWS_PROFILE=visumlabs ./scripts/launch_ec2_ingest.sh \
#       --manifest /tmp/all_cells.jsonl \
#       [--channels book_snapshot_25,trades] [--concurrency 48]
#
# Env knobs: INSTANCE_TYPE (c7i.4xlarge), ROOT_VOLUME_GB (400), USE_SPOT (1),
#            SPOT_MAX_PRICE (unset=on-demand cap), AWS_PROFILE (visumlabs).
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
INSTANCE_TYPE="${INSTANCE_TYPE:-c7i.4xlarge}"
ROOT_VOLUME_GB="${ROOT_VOLUME_GB:-400}"
USE_SPOT="${USE_SPOT:-1}"
PROFILE="${AWS_PROFILE:-visumlabs}"
INSTANCE_PROFILE="${INSTANCE_PROFILE:-instanceRole}"
SUBNET_ID="${SUBNET_ID:-subnet-0c16e9b7f39d97feb}"
SECURITY_GROUP_ID="${SECURITY_GROUP_ID:-sg-0714c4165723a894a}"
MIRROR="s3://pm-research-data-prod/raw/telonex"
STAGE_BUCKET="${STAGE_BUCKET:-pm-research-backtest-prod}"
SSM_KEY_PARAM="${SSM_KEY_PARAM:-/pm/telonex_api_key}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"

MANIFEST=""
CHANNELS="book_snapshot_25,trades"
CONCURRENCY="48"
while [ $# -gt 0 ]; do
    case "$1" in
        --manifest)    MANIFEST="$2"; shift 2 ;;
        --channels)    CHANNELS="$2"; shift 2 ;;
        --concurrency) CONCURRENCY="$2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done
[ -n "$MANIFEST" ] && [ -f "$MANIFEST" ] || { echo "--manifest <local jsonl> required" >&2; exit 1; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STAGE_PREFIX="ingest/${RUN_ID}"
echo "Staging manifest + downloader to s3://${STAGE_BUCKET}/${STAGE_PREFIX}/ ..."
aws s3 cp "$MANIFEST" "s3://${STAGE_BUCKET}/${STAGE_PREFIX}/manifest.jsonl" --profile "$PROFILE" --region "$REGION" --quiet
aws s3 cp "${SCRIPT_DIR}/telonex_ingest.py" "s3://${STAGE_BUCKET}/${STAGE_PREFIX}/telonex_ingest.py" --profile "$PROFILE" --region "$REGION" --quiet

AMI=$(aws ssm get-parameter --region "$REGION" \
    --name /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
    --query Parameter.Value --output text --profile "$PROFILE")
echo "AMI: $AMI  | instance: $INSTANCE_TYPE  | spot: $USE_SPOT  | run: $RUN_ID"

USER_DATA=$(cat <<EOF
#!/bin/bash
set -e
exec > >(tee -a /var/log/pm-ingest.log) 2>&1
echo "[\$(date)] telonex ingest bootstrap run_id=${RUN_ID}"
dnf install -y python3 python3-pip
pip3 install --quiet requests
mkdir -p /opt/ingest/cache && cd /opt/ingest
aws s3 cp s3://${STAGE_BUCKET}/${STAGE_PREFIX}/manifest.jsonl manifest.jsonl --region ${REGION}
aws s3 cp s3://${STAGE_BUCKET}/${STAGE_PREFIX}/telonex_ingest.py telonex_ingest.py --region ${REGION}
export TELONEX_API_KEY=\$(aws ssm get-parameter --name ${SSM_KEY_PARAM} --with-decryption --query Parameter.Value --output text --region ${REGION})

# Download to local cache, then mirror to S3. Run from a dir whose ./data/cache
# matches the telonex_ingest layout root.
mkdir -p /opt/ingest/data && ln -sfn /opt/ingest/cache /opt/ingest/data/cache 2>/dev/null || true
cd /opt/ingest
# Periodic background sync so progress is mirrored continuously over the long run
# (robust to a mid-run failure; on-demand so no spot interruption either).
( while sleep 300; do aws s3 sync /opt/ingest/data/cache/raw/telonex/ ${MIRROR}/ --size-only --region ${REGION} --quiet || true; done ) &
SYNC_PID=\$!
python3 telonex_ingest.py --manifest manifest.jsonl --channels "${CHANNELS}" --concurrency ${CONCURRENCY} || true
kill \$SYNC_PID 2>/dev/null || true

echo "[\$(date)] final sync to mirror ${MIRROR}"
aws s3 sync /opt/ingest/data/cache/raw/telonex/ ${MIRROR}/ --size-only --region ${REGION}

aws s3 cp /var/log/pm-ingest.log s3://${STAGE_BUCKET}/${STAGE_PREFIX}/ingest.log --region ${REGION} || true
echo "[\$(date)] ingest complete, terminating"
shutdown -h now
EOF
)

MARKET_OPTS=()
if [ "$USE_SPOT" = "1" ]; then
    MARKET_OPTS=(--instance-market-options "MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}")
    echo "EC2 market: spot"
fi

INSTANCE_ID=$(aws ec2 run-instances \
    --region "$REGION" --profile "$PROFILE" \
    --image-id "$AMI" \
    --instance-type "$INSTANCE_TYPE" \
    --iam-instance-profile "Name=$INSTANCE_PROFILE" \
    --subnet-id "$SUBNET_ID" \
    --security-group-ids "$SECURITY_GROUP_ID" \
    --instance-initiated-shutdown-behavior terminate \
    --block-device-mappings "DeviceName=/dev/xvda,Ebs={VolumeSize=${ROOT_VOLUME_GB},VolumeType=gp3}" \
    ${MARKET_OPTS[@]+"${MARKET_OPTS[@]}"} \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=pm-ingest-$RUN_ID},{Key=run_id,Value=$RUN_ID}]" \
    --user-data "$USER_DATA" \
    --query 'Instances[0].InstanceId' --output text)

echo "Launched: $INSTANCE_ID  (run_id=$RUN_ID)"
echo "Watch:"
echo "  aws ssm start-session --target $INSTANCE_ID --profile $PROFILE   # then tail /var/log/pm-ingest.log"
echo "  aws s3 cp s3://${STAGE_BUCKET}/${STAGE_PREFIX}/ingest.log - --profile $PROFILE   # after completion"
echo "  aws ec2 describe-instances --instance-ids $INSTANCE_ID --region $REGION --profile $PROFILE --query 'Reservations[].Instances[].State.Name' --output text"
