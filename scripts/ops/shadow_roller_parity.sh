#!/usr/bin/env bash
# Shadow roller ↔ reference wallet parity on Dublin.
set -euo pipefail

SSH_KEY="${SHADOW_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"
SSH_HOST="${SHADOW_SSH_HOST:-}"
if [[ -z "$SSH_HOST" ]]; then
  echo "SHADOW_SSH_HOST is not set; set SHADOW_SSH_HOST=ubuntu@<current-ip>" >&2
  exit 1
fi
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
WALLET="${ROLLER_REF_WALLET:-0x4d64518a17816c43719e4337294b61107611e544}"
SINCE_HOURS=6

while [[ $# -gt 0 ]]; do
  case "$1" in
    --since-hours)
      SINCE_HOURS="$2"
      shift 2
      ;;
    --wallet)
      WALLET="$2"
      shift 2
      ;;
    -h|--help)
      cat <<EOF
Usage: $0 [--since-hours N] [--wallet ADDR]

Pull fresh reference-wallet activity, SCP parity script + cache to Dublin,
compare shadow-roller JSONL timing vs wallet SPLIT / penny-SELL / REDEEM.

Exit 0 (PASS) when missed_shadow=0 and missed_wallet=0; else 1.
EOF
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      exit 1
      ;;
  esac
done

echo "=== Pull reference wallet (last ${SINCE_HOURS}h window, incremental) ==="
PULL_DAYS="$(python3 -c "print(round($SINCE_HOURS/24 + 2, 1))")"
python3 "$REPO_ROOT/scripts/archive/2026-07-cleanup/whale_pull.py" "$WALLET" "$PULL_DAYS"

WALLET_LC="$(printf '%s' "$WALLET" | tr '[:upper:]' '[:lower:]')"
WALLET_JSONL="$REPO_ROOT/data/runs/whales/${WALLET_LC}.jsonl"
if [[ ! -f "$WALLET_JSONL" ]]; then
  echo "FAIL: wallet cache missing: $WALLET_JSONL" >&2
  exit 1
fi

scp -q -i "$SSH_KEY" \
  "$REPO_ROOT/scripts/ops/shadow_roller_parity.py" \
  "$WALLET_JSONL" \
  "$SSH_HOST:/tmp/"

ssh -i "$SSH_KEY" "$SSH_HOST" bash -s <<REMOTE
set -euo pipefail
python3 /tmp/shadow_roller_parity.py \\
  --shadow-dir /home/ubuntu/data/pm-alpha/shadow-roller \\
  --wallet "$WALLET" \\
  --wallet-jsonl /tmp/$(basename "$WALLET_JSONL") \\
  --since-hours "$SINCE_HOURS" \\
  --live
REMOTE