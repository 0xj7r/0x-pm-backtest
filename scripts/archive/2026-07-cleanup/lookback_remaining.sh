#!/usr/bin/env bash
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/release/pm-app
OUT=/tmp/lookback_cmp
mkdir -p "$OUT"
cp -f "$OUT/smoke_w3_3600.json" "$OUT/w3_lb3600.json"

COMMON="--local-cache-dir data/cache \
  --markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m- \
  --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks \
  --latency-ms 150 --stop-before-close-s 90 --fee-curve-rate 0.07 \
  --perp-symbol BTCUSDT --perp-price-weight 0.75 \
  --edge-thresholds 0.12 --exit-after-s 0 --rearm-edge 0.08 --max-clips 2 \
  --min-marginal-edge 0.04 --vol-estimator realized"

run() { # label start end lookback
  echo "[$(date -u +%H:%M:%S)] start $1 (lb=$4)"
  $BIN alpha $COMMON --vol-lookback-s "$4" --date-start "$2" --date-end "$3" \
    --out-json "$OUT/$1.json" > "$OUT/$1.log" 2>&1
  echo "[$(date -u +%H:%M:%S)] done  $1"
}

run w3_lb1800 2026-05-07 2026-05-18 1800
run w2_lb1800 2026-04-01 2026-04-30 1800
run w2_lb3600 2026-04-01 2026-04-30 3600
run w1_lb1800 2026-02-12 2026-03-31 1800
run w1_lb3600 2026-02-12 2026-03-31 3600

echo "=== SUMMARY (realized vol, frozen fade config, lookback 1800 vs 3600) ==="
python3 - <<'PY'
import json,glob,os
OUT="/tmp/lookback_cmp"
def agg(f):
    try:
        a=json.load(open(f))["sweep"][0]["report"]["aggregate"]
        return a.get("n_trades"),a.get("total_pnl"),a.get("hit_rate")
    except Exception as e:
        return None
rows=[]
for w in ("w1","w2","w3"):
    a18=agg(f"{OUT}/{w}_lb1800.json"); a36=agg(f"{OUT}/{w}_lb3600.json")
    rows.append((w,a18,a36))
print(f"{'win':>3} | {'lb1800 NET':>12} {'trades':>7} {'hit':>6} | {'lb3600 NET':>12} {'trades':>7} {'hit':>6} | {'1800-3600':>10}")
tot18=tot36=0.0
for w,a18,a36 in rows:
    if not a18 or not a36:
        print(f"{w:>3} | MISSING {a18} {a36}"); continue
    n18,p18,h18=a18; n36,p36,h36=a36
    tot18+=p18; tot36+=p36
    print(f"{w:>3} | {p18:12.0f} {n18:7d} {h18*100:5.1f}% | {p36:12.0f} {n36:7d} {h36*100:5.1f}% | {p18-p36:+10.0f}")
print(f"{'TOT':>3} | {tot18:12.0f} {'':7} {'':6} | {tot36:12.0f} {'':7} {'':6} | {tot18-tot36:+10.0f}")
print(f"1800 vs 3600 overall: {(tot18/tot36-1)*100:+.1f}%" if tot36 else "")
PY
echo "ALL_DONE"
