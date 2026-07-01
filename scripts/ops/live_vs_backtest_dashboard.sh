#!/usr/bin/env bash
# Fetch rolling live vs backtest dashboard from Dublin (with local replay TSVs).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SSH_KEY="${SHADOW_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"
SSH_HOST="${SHADOW_SSH_HOST:-ubuntu@34.242.101.97}"
REMOTE="/tmp/live_vs_backtest_dashboard.py"
BASELINE_TSV="${BASELINE_TSV:-$REPO_ROOT/data/runs/june_baseline_daily/daily.tsv}"
GATED_TSV="${GATED_TSV:-$REPO_ROOT/data/runs/june_gated_daily/daily.tsv}"

JSON=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --json) JSON=1; shift ;;
    -h|--help)
      echo "Usage: $0 [--json]"
      exit 0
      ;;
    *) echo "Unknown: $1" >&2; exit 1 ;;
  esac
done

scp -q -i "$SSH_KEY" "$REPO_ROOT/scripts/live_vs_backtest_dashboard.py" "$SSH_HOST:$REMOTE"
scp -q -i "$SSH_KEY" "$BASELINE_TSV" "$SSH_HOST:/tmp/june_baseline_daily.tsv" 2>/dev/null || true
scp -q -i "$SSH_KEY" "$GATED_TSV" "$SSH_HOST:/tmp/june_gated_daily.tsv" 2>/dev/null || true

ARGS=(
  python3 "$REMOTE"
  --backtest-baseline-tsv /tmp/june_baseline_daily.tsv
  --backtest-gated-tsv /tmp/june_gated_daily.tsv
)
[[ "$JSON" == "1" ]] && ARGS+=(--json)

ssh -i "$SSH_KEY" "$SSH_HOST" "${ARGS[*]}"