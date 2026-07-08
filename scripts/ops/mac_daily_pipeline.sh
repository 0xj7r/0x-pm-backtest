#!/usr/bin/env bash
# Mac-side daily pipeline (launchd: com.polymarket.daily-replay):
#   1. catch-up canonical replay of missing days (needs pyarrow, hence Mac)
#   2. sync shadow-final JSONLs down from Dublin
#   3. realization report per replayed day (re-arm gate b evidence)
# Requires SHADOW_SSH_HOST (or DUBLIN_SSH_KEY + auto-discovered IP via aws).
set -uo pipefail
cd "$(dirname "$0")/../.."

LOG_DIR="data/runs/daily_replay/logs"
SYNC_DIR="${SHADOW_SYNC_DIR:-data/runs/shadow_final_sync}"
mkdir -p "$LOG_DIR" "$SYNC_DIR"

# Resolve the Dublin host: env wins; otherwise ask AWS for the instance IP.
# launchd runs with the bare system PATH: no homebrew python3 (pyarrow) and
# no aws CLI. Prepend homebrew so scheduled runs match manual ones.
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

HOST="${SHADOW_SSH_HOST:-}"
if [[ -z "$HOST" ]]; then
  IP=$(AWS_PROFILE=visumlabs aws ec2 describe-instances --region eu-west-1 \
    --instance-ids i-0e1d441131c50103c \
    --query "Reservations[0].Instances[0].PublicIpAddress" --output text 2>/dev/null)
  if [[ -n "$IP" && "$IP" != "None" ]]; then
    HOST="ubuntu@$IP"
  fi
fi
SSH_KEY="${DUBLIN_SSH_KEY:-$HOME/.ssh/whale_pair_dublin_ed25519.pem}"

echo "== $(date -u +%FT%TZ) daily pipeline start (host=${HOST:-none}) =="

bash scripts/ops/daily_replay_yesterday.sh || echo "replay: some days failed (see $LOG_DIR)"

if [[ -n "$HOST" ]]; then
  scp -i "$SSH_KEY" -o ConnectTimeout=20 -o StrictHostKeyChecking=no -q \
    "$HOST:~/data/pm-alpha/shadow-final/shadow-*.jsonl" "$SYNC_DIR/" \
    && echo "shadow sync ok" || echo "shadow sync FAILED (box down or IP changed)"
  mkdir -p data/runs/shadow_fast_sync data/runs/shadow_15m_sync
  scp -i "$SSH_KEY" -o ConnectTimeout=20 -o StrictHostKeyChecking=no -q \
    "$HOST:~/data/pm-alpha/shadow-fast/shadow-*.jsonl" data/runs/shadow_fast_sync/ 2>/dev/null \
    && echo "fast sync ok" || echo "fast sync skipped"
  scp -i "$SSH_KEY" -o ConnectTimeout=20 -o StrictHostKeyChecking=no -q \
    "$HOST:~/data/pm-alpha/shadow-15m/shadow-*.jsonl" data/runs/shadow_15m_sync/ 2>/dev/null \
    && echo "15m sync ok" || echo "15m sync skipped"
else
  echo "no Dublin host resolved; skipping shadow sync + realization"
fi

for tj in data/runs/daily_replay/*_trades.jsonl; do
  [[ -e "$tj" ]] || continue
  day=$(basename "$tj" _trades.jsonl)
  python3 scripts/ops/soak_realization_report.py --date "$day" \
    --shadow-dir "$SYNC_DIR" --replay-dir data/runs/daily_replay \
    --out data/runs/daily_replay/realization.jsonl > /dev/null 2>&1 || true
done
# Dedupe the report log (last write per day wins).
python3 - <<'PY'
import json
from pathlib import Path
p = Path("data/runs/daily_replay/realization.jsonl")
if p.exists():
    rows = {}
    for line in p.read_text().splitlines():
        if line.strip():
            r = json.loads(line)
            rows[r["day"]] = r
    p.write_text("".join(json.dumps(rows[d]) + "\n" for d in sorted(rows)))
    for d in sorted(rows)[-3:]:
        r = rows[d]
        print(f"{d}: ratio={r.get('realization_ratio')} agree={r.get('side_agree_pct')}% "
              f"live<15s={r.get('live_pnl_lt15s')} live>=15s={r.get('live_pnl_ge15s')}")
PY
# Dwell-split evidence (final + fast streams) and 15m realization, last 3 days.
for tj in $(ls data/runs/daily_replay/*_trades.jsonl 2>/dev/null | tail -3); do
  day=$(basename "$tj" _trades.jsonl)
  python3 scripts/ops/dwell_split_report.py --date "$day" \
    --out data/runs/daily_replay/dwell_split.jsonl > /dev/null 2>&1 || true
done
for d in $(python3 -c "from datetime import date,timedelta; [print((date.today()-timedelta(days=i)).isoformat()) for i in (1,2)]"); do
  python3 scripts/ops/fast_vs_timer_report.py --date "$d" \
    --out data/runs/daily_replay/fast_vs_timer.jsonl > /dev/null 2>&1 || true
done
for tj in $(ls data/runs/daily_replay/15m/*_trades.jsonl 2>/dev/null | tail -3); do
  day=$(basename "$tj" _trades.jsonl)
  python3 scripts/ops/soak_realization_report.py --date "$day" \
    --shadow-dir data/runs/shadow_15m_sync --replay-dir data/runs/daily_replay/15m \
    --out data/runs/daily_replay/realization_15m.jsonl > /dev/null 2>&1 || true
done
echo "== $(date -u +%FT%TZ) daily pipeline done =="
