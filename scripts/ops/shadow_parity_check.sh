#!/usr/bin/env bash
# REF + LIVE shadow P&L and LIVE↔shadow-final parity on Dublin.
# PASS when orphans=0 and missed_ref=0 (UP and DOWN).
set -euo pipefail

SSH_KEY="${SHADOW_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"
SSH_HOST="${SHADOW_SSH_HOST:-}"
if [[ -z "$SSH_HOST" ]]; then
  echo "SHADOW_SSH_HOST is not set; set SHADOW_SSH_HOST=ubuntu@<current-ip>" >&2
  exit 1
fi
REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"

SHADOW_DIR="/home/ubuntu/data/pm-alpha/shadow-final"
LIVE_LOG="/home/ubuntu/data/pm-alpha/shadow_exec_tail.log"
DAY_UTC="$(date -u +%Y-%m-%d)"
SINCE_HOURS=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --since-hours)
      SINCE_HOURS="$2"
      shift 2
      ;;
    -h|--help)
      cat <<EOF
Usage: $0 [--since-hours N]

SSH to Dublin, print rolling REF+LIVE P&L (1h + 3h), then compare LIVE ENTER
lines against shadow-final would_enter events (UP and DOWN).

Checks:
  - ORPHAN LIVE: executor entered without REF signal
  - MISSED REF→LIVE: REF signaled but executor did not enter (side cutoff)

Exit 0 (PASS) when orphans=0 and missed_ref=0; exit 1 (FAIL) otherwise.

Env: SHADOW_SSH_KEY, SHADOW_SSH_HOST (same as shadow_pnl_hour.sh)
EOF
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      exit 1
      ;;
  esac
done

scp -q -i "$SSH_KEY" \
  "$REPO_ROOT/scripts/ops/shadow_pnl_hour.py" \
  "$REPO_ROOT/scripts/ops/compare_live_ref.py" \
  "$SSH_HOST:/tmp/"

ssh -i "$SSH_KEY" "$SSH_HOST" bash -s <<REMOTE
set -euo pipefail

SHADOW_DIR="$SHADOW_DIR"
LIVE_LOG="$LIVE_LOG"
DAY_UTC="$DAY_UTC"
SINCE_HOURS="$SINCE_HOURS"

echo "=== Shadow P&L — last 1h ==="
python3 /tmp/shadow_pnl_hour.py --hours 1

echo
echo "=== Shadow P&L — last 3h ==="
python3 /tmp/shadow_pnl_hour.py --hours 3

echo
echo "=== LIVE ↔ shadow-final parity ($DAY_UTC, last \${SINCE_HOURS}h) ==="
if [[ ! -f "\$LIVE_LOG" ]]; then
  echo "FAIL: live log not found: \$LIVE_LOG" >&2
  exit 1
fi

shopt -s nullglob
shadow_files=( "\$SHADOW_DIR"/shadow-*.jsonl )
if [[ \${#shadow_files[@]} -eq 0 ]]; then
  echo "FAIL: no shadow JSONL under \$SHADOW_DIR" >&2
  exit 1
fi

combined="/tmp/shadow-final-combined.jsonl"
cat "\${shadow_files[@]}" > "\$combined"
echo "shadow_files=\${#shadow_files[@]} combined=\$combined live_log=\$LIVE_LOG"

set +e
parity_out=\$(python3 /tmp/compare_live_ref.py \\
  --shadow "\$combined" \\
  --live-log "\$LIVE_LOG" \\
  --day "\$DAY_UTC" \\
  --since-hours "\$SINCE_HOURS" 2>&1)
parity_rc=\$?
set -e
printf '%s\n' "\$parity_out"

orphans=\$(printf '%s\n' "\$parity_out" | grep -Eo 'orphans=[0-9]+' | tail -1 | cut -d= -f2)
missed=\$(printf '%s\n' "\$parity_out" | grep -Eo 'missed_ref=[0-9]+' | tail -1 | cut -d= -f2)

if [[ "\$parity_rc" -eq 0 && "\$orphans" == "0" && "\$missed" == "0" ]]; then
  echo
  echo "PASS — orphans=0 missed_ref=0"
  exit 0
fi

echo
echo "FAIL — orphans=\${orphans:-unknown} missed_ref=\${missed:-unknown}"
exit 1
REMOTE