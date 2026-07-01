#!/usr/bin/env bash
# Rolling REF + LIVE shadow P&L from Dublin. Run from repo root or anywhere.
set -euo pipefail

HOURS=1
SSH_KEY="${SHADOW_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"
SSH_HOST="${SHADOW_SSH_HOST:-}"
if [[ -z "$SSH_HOST" ]]; then
  echo "SHADOW_SSH_HOST is not set; set SHADOW_SSH_HOST=ubuntu@<current-ip>" >&2
  exit 1
fi
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --hours)
      HOURS="$2"
      shift 2
      ;;
    -h|--help)
      echo "Usage: $0 [--hours N]"
      echo "  SSH to Dublin and print shadow-final REF + live executor P&L."
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      exit 1
      ;;
  esac
done

scp -q -i "$SSH_KEY" "$REPO_ROOT/scripts/ops/shadow_pnl_hour.py" \
  "$SSH_HOST:/tmp/shadow_pnl_hour.py"

ssh -i "$SSH_KEY" "$SSH_HOST" \
  "python3 /tmp/shadow_pnl_hour.py --hours $HOURS"