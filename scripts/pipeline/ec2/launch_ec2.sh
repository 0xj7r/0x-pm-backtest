#!/usr/bin/env bash
# Launch a t3.xlarge in us-east-1 (same region as pm-research-data-prod),
# bootstrap pm-app, and run a walk-forward backtest. Results land in S3 at
# `s3://pm-research-backtest-prod/results/<run_id>/` and the instance
# auto-terminates on completion.
#
# Usage:
#   AWS_PROFILE=visumlabs ./scripts/launch_ec2.sh \
#       --markets-key path/in/s3.jsonl \
#       --strategies "late_big_bet" \
#       --portfolio
#
# Prereqs (one-time):
#   1. Source bundle in S3:  aws s3 sync . s3://pm-research-backtest-prod/source/
#   2. IAM role:  arn:aws:iam::442426854903:instance-profile/pm-backtest-runner
#      with read on pm-research-data-prod + write on pm-research-backtest-prod
#   3. KMS key access if the bucket uses CSE.
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
INSTANCE_TYPE="${INSTANCE_TYPE:-t3.xlarge}"
RESULTS_BUCKET="${RESULTS_BUCKET:-pm-research-backtest-prod}"
SOURCE_BUCKET="${SOURCE_BUCKET:-pm-research-backtest-prod}"
SOURCE_PREFIX="${SOURCE_PREFIX:-source/polymarket-backtest}"
INSTANCE_PROFILE="${INSTANCE_PROFILE:-pm-backtest-runner}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"

MARKETS_KEY=""
STRATEGIES="late_big_bet,reactive_directional,paired_mm"
STARTING_CASH="1000"
MAX_CLIP="5"
KELLY="0.5"
PORTFOLIO_MODE=""
CLIP_FRAC=""

while [ $# -gt 0 ]; do
    case "$1" in
        --markets-key)  MARKETS_KEY="$2"; shift 2 ;;
        --strategies)   STRATEGIES="$2"; shift 2 ;;
        --starting-cash) STARTING_CASH="$2"; shift 2 ;;
        --max-clip)     MAX_CLIP="$2"; shift 2 ;;
        --kelly)        KELLY="$2"; shift 2 ;;
        --portfolio)    PORTFOLIO_MODE="--portfolio-mode"; shift ;;
        --clip-fraction) CLIP_FRAC="--clip-fraction-of-equity $2"; shift 2 ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

[ -n "$MARKETS_KEY" ] || { echo "--markets-key required" >&2; exit 1; }

# Look up the latest Amazon Linux 2023 AMI for the region via SSM Parameter Store.
AMI=$(aws ssm get-parameter \
    --region "$REGION" \
    --name /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
    --query Parameter.Value --output text)
echo "AMI: $AMI"
echo "Run ID: $RUN_ID"

USER_DATA=$(cat <<EOF
#!/bin/bash
set -e
exec > >(tee -a /var/log/pm-bootstrap.log) 2>&1
echo "[\$(date)] starting pm-app bootstrap (run_id=$RUN_ID)"

dnf install -y git gcc openssl-devel cmake

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y --default-toolchain 1.95
source /root/.cargo/env

mkdir -p /opt/pm
cd /opt/pm
aws s3 sync s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/ /opt/pm/ \
    --exclude "target/*" --exclude "data/*"

aws s3 cp s3://${SOURCE_BUCKET}/markets/${MARKETS_KEY} /opt/pm/markets.jsonl

cd /opt/pm
cargo build --release -p pm-app

PM_TELONEX_REGION=${REGION} ./target/release/pm-app walk-forward \\
    --markets /opt/pm/markets.jsonl \\
    --strategies "${STRATEGIES}" \\
    --starting-cash ${STARTING_CASH} \\
    --max-clip-usdc ${MAX_CLIP} \\
    --kelly-fraction ${KELLY} \\
    --spot-symbol BTCUSDT \\
    --use-outcome-label \\
    --max-concurrent-fetches 32 \\
    ${PORTFOLIO_MODE} ${CLIP_FRAC} \\
    --out-markets /opt/pm/result-markets.jsonl \\
    --out-summary /opt/pm/result-summary.json \\
    2>&1 | tee /opt/pm/result.log || true

aws s3 cp /opt/pm/result-markets.jsonl  s3://${RESULTS_BUCKET}/results/${RUN_ID}/markets.jsonl
aws s3 cp /opt/pm/result-summary.json   s3://${RESULTS_BUCKET}/results/${RUN_ID}/summary.json
aws s3 cp /opt/pm/result.log            s3://${RESULTS_BUCKET}/results/${RUN_ID}/run.log
aws s3 cp /var/log/pm-bootstrap.log     s3://${RESULTS_BUCKET}/results/${RUN_ID}/bootstrap.log

echo "[\$(date)] backtest complete, terminating"
shutdown -h now
EOF
)

INSTANCE_ID=$(aws ec2 run-instances \
    --region "$REGION" \
    --image-id "$AMI" \
    --instance-type "$INSTANCE_TYPE" \
    --iam-instance-profile "Name=$INSTANCE_PROFILE" \
    --instance-initiated-shutdown-behavior terminate \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=pm-backtest-$RUN_ID},{Key=run_id,Value=$RUN_ID}]" \
    --user-data "$USER_DATA" \
    --query 'Instances[0].InstanceId' --output text)

echo "Launched: $INSTANCE_ID"
echo "Results will land at: s3://${RESULTS_BUCKET}/results/${RUN_ID}/"
echo
echo "Watch with:"
echo "  aws s3 ls s3://${RESULTS_BUCKET}/results/${RUN_ID}/ --recursive"
echo "  aws ec2 describe-instance-status --instance-ids $INSTANCE_ID --region $REGION"
