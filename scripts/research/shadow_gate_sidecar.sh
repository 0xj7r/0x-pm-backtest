#!/usr/bin/env bash
# Start or status the shadow gate A/B sidecar on Dublin (no shadow-final restart).
set -euo pipefail

SSH_KEY="${SHADOW_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"
SSH_HOST="${SHADOW_SSH_HOST:-ubuntu@34.242.101.97}"
REPO="${SHADOW_REPO:-/home/ubuntu/pm-backtest}"
DATA="${SHADOW_DATA:-/home/ubuntu/data/pm-alpha}"
PID_FILE="${DATA}/shadow-gate-ab/sidecar.pid"
LOG_FILE="${DATA}/shadow-gate-ab/sidecar.log"

cmd="${1:-status}"

run_remote() {
  ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$SSH_HOST" "$@"
}

case "$cmd" in
  start)
    run_remote "mkdir -p ${DATA}/shadow-gate-ab"
    run_remote "scp -q -o StrictHostKeyChecking=no" \
      "$(dirname "$0")/shadow_gate_sidecar.py" \
      "$(dirname "$0")/score_shadow_gate_ab.py" \
      "${SSH_HOST}:${REPO}/scripts/" 2>/dev/null || true
    run_remote bash -s <<EOF
set -euo pipefail
if [[ -f ${PID_FILE} ]] && kill -0 "\$(cat ${PID_FILE})" 2>/dev/null; then
  echo "sidecar already running pid=\$(cat ${PID_FILE})"
  exit 0
fi
nohup python3 ${REPO}/scripts/shadow_gate_sidecar.py \\
  --shadow-dir ${DATA}/shadow-final \\
  --out ${DATA}/shadow-gate-ab/gate_ab.jsonl \\
  --state ${DATA}/shadow-gate-ab/tail_state.json \\
  --session-state ${DATA}/shadow-gate-ab/session_state.json \\
  >> ${LOG_FILE} 2>&1 &
echo \$! > ${PID_FILE}
echo "started pid=\$(cat ${PID_FILE})"
EOF
    ;;
  stop)
    run_remote "[[ -f ${PID_FILE} ]] && kill \$(cat ${PID_FILE}) 2>/dev/null; rm -f ${PID_FILE}; echo stopped"
    ;;
  status)
    run_remote "if [[ -f ${PID_FILE} ]] && kill -0 \$(cat ${PID_FILE}) 2>/dev/null; then echo RUNNING pid=\$(cat ${PID_FILE}); tail -3 ${LOG_FILE} 2>/dev/null; else echo STOPPED; fi"
    ;;
  score)
    SINCE="${2:-}"
    run_remote "python3 ${REPO}/scripts/score_shadow_gate_ab.py --gate-ab ${DATA}/shadow-gate-ab/gate_ab.jsonl --shadow-dir ${DATA}/shadow-final ${SINCE:+--since $SINCE}"
    ;;
  *)
    echo "Usage: $0 {start|stop|status|score [ISO_UTC]}"
    exit 1
    ;;
esac