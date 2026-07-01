#!/usr/bin/env bash
# Run live_trade_ledger.py on Dublin via SSH (ingest / daily-report / query).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SSH_KEY="${SHADOW_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"
SSH_HOST="${SHADOW_SSH_HOST:-}"
if [[ -z "$SSH_HOST" ]]; then
  echo "SHADOW_SSH_HOST is not set; set SHADOW_SSH_HOST=ubuntu@<current-ip>" >&2
  exit 1
fi
REMOTE="/tmp/live_trade_ledger.py"

if [[ $# -eq 0 ]]; then
  echo "Usage: $0 ingest|daily-report|query --date YYYY-MM-DD" >&2
  exit 1
fi

scp -q -i "$SSH_KEY" "$REPO_ROOT/scripts/ops/live_trade_ledger.py" "$SSH_HOST:$REMOTE"
ssh -i "$SSH_KEY" "$SSH_HOST" python3 "$REMOTE" "$@"