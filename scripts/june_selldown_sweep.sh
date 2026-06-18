#!/usr/bin/env bash
# Selldown-stop sweep on June gated config (mom30_ask045, hold-to-expiry base).
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/june_selldown_sweep}"
mkdir -p "$OUT"

COMMON=(
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl
  --local-cache-dir data/cache
  --tick-cache-dir data/cache/ticks
  --exit-after-s 0
  --perp-symbol BTCUSDT
  --perp-price-weight 0.75
  --vol-estimator realized
  --vol-lookback-s 3600
  --edge-thresholds 0.12
  --notional-usdc 50
  --latency-ms 250
  --max-clips 2
  --rearm-edge 0.08
  --clip-cooldown-ms 5000
  --min-entry-sigma-bps 3
  --skip-saturday
  --stop-before-close-s 90
  --min-marginal-edge 0.04
  --fee-curve-rate 0.07
  --skip-spot-misalign-s 30
  --min-entry-ask 0.45
  --date-start 2026-06-01
  --date-end 2026-06-16
)

run_one() {
  local label=$1 eps=$2
  if [[ -s "$OUT/${label}.trades.jsonl" && -s "$OUT/${label}.json" ]]; then
    echo "[skip] $label (exists)"
    return 0
  fi
  echo "[start] $label eps=$eps"
  "$BIN" alpha "${COMMON[@]}" \
    --selldown-stop-eps "$eps" \
    --trades-out "$OUT/${label}.trades.jsonl" \
    --out-json "$OUT/${label}.json" > "$OUT/${label}.log" 2>&1
  echo "[done] $label"
}

for spec in "flat:-1.0" "e10:0.10" "e15:0.15" "e20:0.20" "e25:0.25"; do
  label=${spec%%:*}
  eps=${spec##*:}
  run_one "$label" "$eps"
done

python3 - "$OUT" <<'PY'
import json
import sys
from pathlib import Path

out = Path(sys.argv[1])

def stat(label):
    path = out / f"{label}.trades.jsonl"
    if not path.is_file():
        return None
    pnls = []
    n = ns = 0
    real = hold = 0.0
    cut_win = cut_loss = 0
    for line in path.open():
        t = json.loads(line)
        pnl = float(t.get("pnl", 0))
        pnls.append((t.get("decision_ts_ns", 0), pnl))
        n += 1
        if t.get("stopped"):
            ns += 1
            real += pnl
            h = float(t.get("stop_hold_pnl") or 0)
            hold += h
            if h > 0:
                cut_win += 1
            else:
                cut_loss += 1
    pnls.sort()
    eq = peak = mdd = 0.0
    for _, p in pnls:
        eq += p
        peak = max(peak, eq)
        mdd = max(mdd, peak - eq)
    net = sum(p for _, p in pnls)
    wins = sum(1 for _, p in pnls if p > 0)
    return dict(
        net=net,
        mdd=mdd,
        n=n,
        hit=100 * wins / n if n else 0,
        ns=ns,
        stop_real=real,
        stop_hold=hold,
        stop_gain=real - hold,
        cut_win=cut_win,
        cut_loss=cut_loss,
    )

print("June gated mom30_ask045 | Jun 1-16 | $50 clip | selldown stop on ask <= fill-eps")
print(
    "%-6s %10s %8s %6s %7s %10s %7s %7s"
    % ("arm", "NET", "maxDD", "hit%", "n_stop", "stop_gain", "cut_win", "cut_loss")
)
for label in ("flat", "e10", "e15", "e20", "e25"):
    r = stat(label)
    if not r:
        print(f"{label} MISSING")
        continue
    print(
        "%-6s %10.0f %8.0f %5.1f%% %7d %+10.0f %7d %7d"
        % (label, r["net"], r["mdd"], r["hit"], r["ns"], r["stop_gain"], r["cut_win"], r["cut_loss"])
    )
print()
print("stop_gain > 0 => salvage beat hold on stopped trades")
print("cut_win = stopped trades that would have won if held")
PY