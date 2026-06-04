#!/usr/bin/env bash
# Launch one EC2 runner for the BackToExplore fixed-vs-policy replay.
#
# This is intentionally narrower than the portfolio-grid launchers: it runs the
# same BTE profile twice over an already-built held-out manifest, once fixed and
# once with --back-to-explore-policy-scales-jsonl.
set -euo pipefail

REGION="${AWS_REGION:-us-east-1}"
INSTANCE_TYPE="${INSTANCE_TYPE:-c7i.4xlarge}"
RESULTS_BUCKET="${RESULTS_BUCKET:-pm-research-backtest-prod}"
SOURCE_BUCKET="${SOURCE_BUCKET:-pm-research-backtest-prod}"
SOURCE_PREFIX="${SOURCE_PREFIX:-source/polymarket-backtest}"
INSTANCE_PROFILE="${INSTANCE_PROFILE:-instanceRole}"
KEY_NAME="${KEY_NAME:-whale-pair-use1}"
SECURITY_GROUP_ID="${SECURITY_GROUP_ID:-sg-0714c4165723a894a}"
SUBNET_ID="${SUBNET_ID:-subnet-0c16e9b7f39d97feb}"
ROOT_VOLUME_GB="${ROOT_VOLUME_GB:-250}"
USE_SPOT="${USE_SPOT:-1}"
SYNC_SOURCE="${SYNC_SOURCE:-1}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-bte-policy-replay-$$"

LOCAL_MARKETS="data/manifests/regime_clusters/bte_combined_no_boost_heldout.jsonl"
LOCAL_SCALES="data/runs/regime_clusters/bte_cluster_policy_combined_no_boost_scales.jsonl"
PROFILE_PATH="configs/back_to_explore_btc5m_conditional.toml"
STARTING_CASH="2700"
CLIP_FRACTION="0.0025"
MAX_CONCURRENT_FETCHES="64"
CHECKPOINT_EVERY="250"
ARM="both"

while [ $# -gt 0 ]; do
    case "$1" in
        --markets) LOCAL_MARKETS="$2"; shift 2 ;;
        --scales) LOCAL_SCALES="$2"; shift 2 ;;
        --profile) PROFILE_PATH="$2"; shift 2 ;;
        --starting-cash) STARTING_CASH="$2"; shift 2 ;;
        --clip-fraction) CLIP_FRACTION="$2"; shift 2 ;;
        --max-concurrent-fetches) MAX_CONCURRENT_FETCHES="$2"; shift 2 ;;
        --checkpoint-every) CHECKPOINT_EVERY="$2"; shift 2 ;;
        --instance-type) INSTANCE_TYPE="$2"; shift 2 ;;
        --arm) ARM="$2"; shift 2 ;;
        --on-demand) USE_SPOT="0"; shift ;;
        --no-source-sync) SYNC_SOURCE="0"; shift ;;
        *) echo "unknown arg: $1" >&2; exit 1 ;;
    esac
done

case "$ARM" in
    fixed|policy|both) ;;
    *) echo "--arm must be fixed, policy, or both" >&2; exit 1 ;;
esac
RUN_FIXED="0"
RUN_POLICY="0"
case "$ARM" in
    fixed) RUN_FIXED="1" ;;
    policy) RUN_POLICY="1" ;;
    both)
        RUN_FIXED="1"
        RUN_POLICY="1"
        ;;
esac

[ -f "$LOCAL_MARKETS" ] || { echo "missing markets file: $LOCAL_MARKETS" >&2; exit 1; }
[ -f "$LOCAL_SCALES" ] || { echo "missing scale file: $LOCAL_SCALES" >&2; exit 1; }
[ -f "$PROFILE_PATH" ] || { echo "missing profile: $PROFILE_PATH" >&2; exit 1; }

MARKETS_KEY="markets/regime_clusters/${RUN_ID}/markets.jsonl"
SCALES_KEY="artifacts/regime_clusters/${RUN_ID}/bte_policy_scales.jsonl"

if [ "$SYNC_SOURCE" = "1" ]; then
    echo "Syncing source to s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/"
    aws s3 rm "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/" --recursive --quiet
    for file in Cargo.toml Cargo.lock rust-toolchain.toml README.md .gitignore; do
        [ -f "$file" ] && aws s3 cp "$file" "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/${file}" --quiet
    done
    aws s3 sync crates "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/crates/" --delete --quiet
    aws s3 sync scripts "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/scripts/" --delete --quiet
    aws s3 sync configs "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/configs/" --delete --quiet
    [ -d docs ] && aws s3 sync docs "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/docs/" --delete --quiet
else
    echo "Skipping source sync; using existing s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/"
fi

aws s3 cp "$LOCAL_MARKETS" "s3://${SOURCE_BUCKET}/${MARKETS_KEY}" --quiet
aws s3 cp "$LOCAL_SCALES" "s3://${SOURCE_BUCKET}/${SCALES_KEY}" --quiet

SOURCE_GIT_SHA="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
AMI=$(aws ssm get-parameter \
    --region "$REGION" \
    --name /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
    --query Parameter.Value --output text)

echo "AMI: $AMI"
echo "Run ID: $RUN_ID"
echo "Source git SHA: $SOURCE_GIT_SHA"
echo "Markets: s3://${SOURCE_BUCKET}/${MARKETS_KEY}"
echo "Scales: s3://${SOURCE_BUCKET}/${SCALES_KEY}"
echo "Arm: $ARM"

INSTANCE_MARKET_OPTIONS_ARGS=()
if [ "$USE_SPOT" = "1" ]; then
    INSTANCE_MARKET_OPTIONS_ARGS=(
        --instance-market-options
        "MarketType=spot,SpotOptions={SpotInstanceType=one-time,InstanceInterruptionBehavior=terminate}"
    )
    echo "EC2 market: spot"
else
    echo "EC2 market: on-demand"
fi

USER_DATA=$(cat <<EOF
#!/bin/bash
set -euo pipefail
exec > >(tee -a /var/log/pm-bootstrap.log) 2>&1
echo "[\$(date -u)] starting BTE policy replay run_id=${RUN_ID}"

dnf install -y git gcc gcc-c++ make openssl-devel pkgconf-pkg-config cmake clang jq
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | bash -s -- -y --default-toolchain 1.95
export HOME=/root
source /root/.cargo/env

mkdir -p /opt/pm/artifacts /opt/pm/results
aws s3 sync "s3://${SOURCE_BUCKET}/${SOURCE_PREFIX}/" /opt/pm/ \\
    --exclude "target/*" --exclude "data/*" --exclude ".git/*"
cd /opt/pm
export PM_SOURCE_GIT_SHA="${SOURCE_GIT_SHA}"
cargo build --release -p pm-app

aws s3 cp "s3://${SOURCE_BUCKET}/${MARKETS_KEY}" /opt/pm/markets.jsonl
aws s3 cp "s3://${SOURCE_BUCKET}/${SCALES_KEY}" /opt/pm/bte_policy_scales.jsonl
aws s3 cp /opt/pm/markets.jsonl "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/markets.jsonl"
aws s3 cp /opt/pm/bte_policy_scales.jsonl "s3://${RESULTS_BUCKET}/results/${RUN_ID}/artifacts/bte_policy_scales.jsonl"

run_arm() {
  local label="\$1"
  shift
  local out_dir="/opt/pm/results/\${label}"
  mkdir -p "\${out_dir}"
  echo "[\$(date -u)] running \${label}"
  (
    while true; do
      sleep 180
      [ -f "\${out_dir}/markets.jsonl" ] && aws s3 cp "\${out_dir}/markets.jsonl" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${label}/markets.jsonl" || true
      [ -f "\${out_dir}/summary.json" ] && aws s3 cp "\${out_dir}/summary.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${label}/summary.json" || true
      [ -f "\${out_dir}/run_manifest.json" ] && aws s3 cp "\${out_dir}/run_manifest.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${label}/run_manifest.json" || true
    done
  ) &
  local uploader_pid="\$!"
  set +e
  PM_TELONEX_REGION="${REGION}" ./target/release/pm-app walk-forward \\
    --markets /opt/pm/markets.jsonl \\
    --profile "${PROFILE_PATH}" \\
    --strategies back_to_explore \\
    --starting-cash "${STARTING_CASH}" \\
    --clip-fraction-of-equity "${CLIP_FRACTION}" \\
    --portfolio-mode \\
    --use-outcome-label \\
    --spot-symbol BTCUSDT \\
    --max-concurrent-fetches "${MAX_CONCURRENT_FETCHES}" \\
    --portfolio-checkpoint-every-markets "${CHECKPOINT_EVERY}" \\
    "\$@" \\
    --out-markets "\${out_dir}/markets.jsonl" \\
    --out-summary "\${out_dir}/summary.json" \\
    2>&1 | tee "\${out_dir}/run.log"
  local status="\${PIPESTATUS[0]}"
  set -e
  kill "\${uploader_pid}" 2>/dev/null || true
  wait "\${uploader_pid}" 2>/dev/null || true
  aws s3 cp "\${out_dir}/markets.jsonl" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${label}/markets.jsonl" || true
  aws s3 cp "\${out_dir}/summary.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${label}/summary.json" || true
  aws s3 cp "\${out_dir}/run_manifest.json" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${label}/run_manifest.json" || true
  aws s3 cp "\${out_dir}/run.log" "s3://${RESULTS_BUCKET}/results/${RUN_ID}/\${label}/run.log" || true
  if [ "\${status}" != "0" ]; then
    echo "[\$(date -u)] arm \${label} failed with status \${status}"
    exit "\${status}"
  fi
}

if [ "${RUN_FIXED}" = "1" ]; then
  run_arm fixed
fi
if [ "${RUN_POLICY}" = "1" ]; then
  run_arm policy --back-to-explore-policy-scales-jsonl /opt/pm/bte_policy_scales.jsonl
fi

aws s3 cp /var/log/pm-bootstrap.log "s3://${RESULTS_BUCKET}/results/${RUN_ID}/bootstrap.log" || true
echo "[\$(date -u)] BTE policy replay complete"
shutdown -h now
EOF
)

INSTANCE_ID=$(aws ec2 run-instances \
    --region "$REGION" \
    --image-id "$AMI" \
    --instance-type "$INSTANCE_TYPE" \
    "${INSTANCE_MARKET_OPTIONS_ARGS[@]}" \
    --iam-instance-profile "Name=$INSTANCE_PROFILE" \
    --key-name "$KEY_NAME" \
    --security-group-ids "$SECURITY_GROUP_ID" \
    --subnet-id "$SUBNET_ID" \
    --block-device-mappings "DeviceName=/dev/xvda,Ebs={VolumeSize=${ROOT_VOLUME_GB},VolumeType=gp3,DeleteOnTermination=true}" \
    --instance-initiated-shutdown-behavior terminate \
    --tag-specifications "ResourceType=instance,Tags=[{Key=Name,Value=pm-bte-policy-${RUN_ID}},{Key=run_id,Value=${RUN_ID}},{Key=project,Value=polymarket-backtest}]" \
    --user-data "$USER_DATA" \
    --query 'Instances[0].InstanceId' --output text)

echo "Launched: $INSTANCE_ID"
echo "Results: s3://${RESULTS_BUCKET}/results/${RUN_ID}/"
echo "Watch:"
echo "  AWS_PROFILE=visumlabs aws s3 ls s3://${RESULTS_BUCKET}/results/${RUN_ID}/ --recursive"
echo "  AWS_PROFILE=visumlabs aws ec2 describe-instance-status --instance-ids ${INSTANCE_ID} --region ${REGION}"
