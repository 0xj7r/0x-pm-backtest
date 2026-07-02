#!/usr/bin/env bash
# Canonical replay of recent days: ingest Telonex/Binance data and run the
# frozen-config harness (fee-net, latency 250) with per-trade dumps. Output
# feeds the realization-ratio comparison (docs/PROD.md re-arm gate b).
# Runs on the Mac (needs pyarrow/pandas; Dublin's system python has neither).
# CATCH-UP: with no DAY set, replays every missing day in the last
# CATCHUP_DAYS, so an asleep Mac backfills on next run.
set -euo pipefail
cd "$(dirname "$0")/../.."

OUT="${OUT:-data/runs/daily_replay}"
LOG_DIR="$OUT/logs"
mkdir -p "$OUT" "$LOG_DIR"
CATCHUP_DAYS="${CATCHUP_DAYS:-7}"

days_to_run() {
  if [[ -n "${DAY:-}" ]]; then
    echo "$DAY"
    return
  fi
  python3 - "$OUT" "$CATCHUP_DAYS" <<'PY'
import sys
from datetime import date, timedelta
from pathlib import Path
out, n = Path(sys.argv[1]), int(sys.argv[2])
today = date.today()
for i in range(n, 0, -1):
    d = (today - timedelta(days=i)).isoformat()
    if not (out / f"{d}.json").exists():
        print(d)
PY
}

BIN="${BIN:-./target/release/pm-app}"

FAILED=0
for DAY in $(days_to_run); do

echo "== ingest $DAY =="
if ! START="$DAY" END="$DAY" bash scripts/pipeline/ingest_june_live.sh > "$LOG_DIR/${DAY}_ingest.log" 2>&1; then
  echo "$DAY ingest FAILED (see $LOG_DIR/${DAY}_ingest.log); continuing with next day"
  FAILED=1
  continue
fi

echo "== replay $DAY (frozen config, canonical accounting) =="
"$BIN" alpha \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
  --local-cache-dir data/cache \
  --tick-cache-dir data/cache/ticks \
  --exit-after-s 0 \
  --perp-symbol BTCUSDT \
  --perp-price-weight 0.75 \
  --vol-estimator realized \
  --vol-lookback-s 3600 \
  --edge-thresholds 0.12 \
  --notional-usdc 50 \
  --latency-ms 250 \
  --max-clips 2 \
  --rearm-edge 0.08 \
  --clip-cooldown-ms 5000 \
  --min-entry-sigma-bps 3 \
  --skip-saturday \
  --stop-before-close-s 90 \
  --min-marginal-edge 0.04 \
  --fee-curve-rate 0.07 \
  --date-start "$DAY" --date-end "$DAY" \
  --out-json "$OUT/${DAY}.json" \
  --trades-out "$OUT/${DAY}_trades.jsonl" > "$LOG_DIR/${DAY}_replay.log" 2>&1 || {
  echo "$DAY replay FAILED (see $LOG_DIR/${DAY}_replay.log); continuing with next day"
  rm -f "$OUT/${DAY}.json"
  FAILED=1
  continue
}

python3 - "$OUT/${DAY}.json" "$DAY" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
agg = (r.get("sweep") or [r])[0].get("report", {}).get("aggregate", {})
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", 0))
wins = int(agg.get("n_wins", 0))
print(f"{sys.argv[2]} replay: n={n} NET=${net:+,.0f} hit={100*wins/n if n else 0:.1f}%")
PY

done
exit "$FAILED"
