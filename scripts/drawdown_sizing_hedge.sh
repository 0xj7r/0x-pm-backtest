#!/usr/bin/env bash
# Drawdown impact of vol-sizing and the convex tail hedge, at the $15 pilot clip,
# W3 whipsaw window (worst-case). Equity-curve max-drawdown from per-trade pnl.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=/tmp/ddcmp
mkdir -p "$OUT"
COMMON="--local-cache-dir data/cache --markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m- --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --stop-before-close-s 90 --fee-curve-rate 0.07 --perp-symbol BTCUSDT --perp-price-weight 0.75 --edge-thresholds 0.12 --exit-after-s 0 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 --vol-estimator realized --vol-lookback-s 3600 --notional-usdc 15"
W3="--date-start 2026-05-07 --date-end 2026-05-18"

run(){ echo "[$(date -u +%H:%M:%S)] $1"; $BIN alpha $COMMON $2 $W3 --trades-out "$OUT/$1.trades.jsonl" --out-json "$OUT/$1.json" > "$OUT/$1.log" 2>&1; }

run flat    ""
run vol     "--vol-sizing-ref-bps 9.58 --vol-sizing-lo 0.5 --vol-sizing-hi 2.0"
run tail    "--tail-max-price 0.15 --tail-frac 0.5"
run voltail "--vol-sizing-ref-bps 9.58 --vol-sizing-lo 0.5 --vol-sizing-hi 2.0 --tail-max-price 0.15 --tail-frac 0.5"

python3 - <<'PY'
import json
def analyze(label):
    pnls=[]
    try:
        for line in open(f'/tmp/ddcmp/{label}.trades.jsonl'):
            t=json.loads(line)
            pnls.append((t.get('decision_ts_ns',0), t.get('pnl',0.0)))
    except FileNotFoundError:
        return None
    pnls.sort()
    eq=peak=maxdd=0.0
    for _,p in pnls:
        eq+=p; peak=max(peak,eq); maxdd=max(maxdd,peak-eq)
    net=sum(p for _,p in pnls)
    return net,maxdd,len(pnls)
print('%-8s %9s %9s %7s %8s %8s' % ('arm','NET','maxDD','n','DD/NET','DD/$2k'))
base=None
for a in ['flat','vol','tail','voltail']:
    r=analyze(a)
    if not r: print('%-8s MISSING (see log)'%a); continue
    net,dd,n=r
    if a=='flat': base=(net,dd)
    print('%-8s %9.0f %9.0f %7d %7.0f%% %7.0f%%' % (a,net,dd,n,100*dd/net if net else 0,100*dd/2000))
if base:
    print('(flat baseline: NET %.0f maxDD %.0f at $15 clips on W3)'%base)
PY
rm -f /tmp/ddcmp/*.trades.jsonl
echo ALL_DONE
