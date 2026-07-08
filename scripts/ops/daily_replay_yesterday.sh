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
mkdir -p "$OUT" "$OUT/15m" "$LOG_DIR"
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

# The manifest builder reads the markets parquet; a stale one silently yields
# zero manifest rows for recent days. Refresh when older than 20h.
PQ=data/cache/telonex_markets.parquet
if [[ ! -f "$PQ" ]] || [[ -n "$(find "$PQ" -mmin +1200 2>/dev/null)" ]]; then
  echo "refreshing markets parquet"
  curl -sL -o "$PQ.tmp" "https://api.telonex.io/v1/datasets/polymarket/markets" \
    && mv "$PQ.tmp" "$PQ" || echo "parquet refresh FAILED; using existing"
fi

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

python3 - "$OUT/${DAY}.json" "$DAY" <<'PY' || { rm -f "$OUT/${DAY}.json" "$OUT/${DAY}_trades.jsonl"; echo "$DAY produced 0 markets (source data not yet published?); will retry on next catch-up"; FAILED=1; continue; }
import json, sys
r = json.load(open(sys.argv[1]))
if int(r.get("n_markets_run") or 0) == 0:
    raise SystemExit(1)
agg = (r.get("sweep") or [r])[0].get("report", {}).get("aggregate", {})
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", 0))
wins = int(agg.get("n_wins", 0))
print(f"{sys.argv[2]} replay: n={n} NET=${net:+,.0f} hit={100*wins/n if n else 0:.1f}%")
PY

# Latency-matched replay (1250ms = the live TIMER engine's measured effective
# latency). Realization must compare live against edge ACHIEVABLE at our real
# latency; the 250ms replay above is the fantasy-latency reference only. A
# realization ratio vs the 250ms replay conflates the known latency tax with
# genuine execution slippage and reads artificially catastrophic.
mkdir -p "$OUT/lat1250"
"$BIN" alpha \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl \
  --slug-prefix btc-updown-5m- \
  --local-cache-dir data/cache \
  --tick-cache-dir data/cache/ticks \
  --exit-after-s 0 --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --vol-estimator realized --vol-lookback-s 3600 \
  --edge-thresholds 0.12 --notional-usdc 50 --latency-ms 1250 \
  --max-clips 2 --rearm-edge 0.08 --clip-cooldown-ms 5000 \
  --min-entry-sigma-bps 3 --skip-saturday --stop-before-close-s 90 \
  --min-marginal-edge 0.04 --fee-curve-rate 0.07 \
  --date-start "$DAY" --date-end "$DAY" \
  --out-json "$OUT/lat1250/${DAY}.json" \
  --trades-out "$OUT/lat1250/${DAY}_trades.jsonl" > "$LOG_DIR/${DAY}_replay1250.log" 2>&1 \
  || echo "$DAY 1250ms replay FAILED (non-blocking)"

# 15m book replay for the same day (separate output; used by the 15m
# realization loop). Failure here does not block the 5m result.
"$BIN" alpha \
  --markets data/manifests/canonical/btc-updown-15m_up.jsonl \
  --slug-prefix btc-updown-15m- \
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
  --out-json "$OUT/15m/${DAY}.json" \
  --trades-out "$OUT/15m/${DAY}_trades.jsonl" > "$LOG_DIR/${DAY}_replay15m.log" 2>&1 \
  || echo "$DAY 15m replay FAILED (non-blocking)"

done
exit "$FAILED"
