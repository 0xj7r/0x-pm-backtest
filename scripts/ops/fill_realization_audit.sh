#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SSH_KEY="${SHADOW_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"
SSH_HOST="${SHADOW_SSH_HOST:-}"
if [[ -z "$SSH_HOST" ]]; then
  echo "SHADOW_SSH_HOST is not set; set SHADOW_SSH_HOST=ubuntu@<current-ip>" >&2
  exit 1
fi
SINCE_HOURS="${SINCE_HOURS:-0}"

JSON=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --json) JSON=1; shift ;;
    --since-hours) SINCE_HOURS="$2"; shift 2 ;;
    -h|--help)
      echo "Usage: $0 [--since-hours N] [--json]"
      exit 0
      ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
done

scp -q -i "$SSH_KEY" "$REPO_ROOT/scripts/ops/fill_realization_audit.py" "$SSH_HOST:/tmp/fill_realization_audit.py"
ARGS=(python3 /tmp/fill_realization_audit.py --since-hours "$SINCE_HOURS")
[[ "$JSON" == "1" ]] && ARGS+=(--json)
ssh -i "$SSH_KEY" "$SSH_HOST" "${ARGS[*]}"