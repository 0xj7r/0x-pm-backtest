#!/usr/bin/env bash
# Overall (3-window) flat vs vol-sizing: NET + equity-curve max-drawdown at the
# $15 pilot clip. W1/W2 run here; W3 already measured (flat 9269/744, vol 7705/546).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=/tmp/volall
mkdir -p "$OUT"
COMMON="--local-cache-dir data/cache --markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m- --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --stop-before-close-s 90 --fee-curve-rate 0.07 --perp-symbol BTCUSDT --perp-price-weight 0.75 --edge-thresholds 0.12 --exit-after-s 0 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 --vol-estimator realized --vol-lookback-s 3600 --notional-usdc 15"
VOL="--vol-sizing-ref-bps 9.58 --vol-sizing-lo 0.5 --vol-sizing-hi 2.0"

go(){ # label start end extra
  $BIN alpha $COMMON $4 --date-start $2 --date-end $3 --trades-out "$OUT/$1.trades.jsonl" --out-json "$OUT/$1.json" > "$OUT/$1.log" 2>&1
  echo "[$(date -u +%H:%M:%S)] done $1"
}

( go w1_flat 2026-02-12 2026-03-31 "" ) &
( go w1_vol  2026-02-12 2026-03-31 "$VOL" ) &
( go w2_flat 2026-04-01 2026-04-30 "" ) &
( go w2_vol  2026-04-01 2026-04-30 "$VOL" ) &
wait

python3 - <<'PY'
import json
def stat(label):
    pnls=[]
    try:
        for line in open(f'/tmp/volall/{label}.trades.jsonl'):
            t=json.loads(line); pnls.append((t.get('decision_ts_ns',0), t.get('pnl',0.0)))
    except FileNotFoundError: return None
    pnls.sort(); eq=peak=mdd=0.0
    for _,p in pnls:
        eq+=p; peak=max(peak,eq); mdd=max(mdd,peak-eq)
    return sum(p for _,p in pnls), mdd, len(pnls)
W3={'flat':(9269,744),'vol':(7705,546)}
rows={}
for w in ('w1','w2'):
    for a in ('flat','vol'):
        r=stat(f'{w}_{a}')
        rows[(w,a)]=(r[0],r[1]) if r else (None,None)
rows[('w3','flat')]=W3['flat']; rows[('w3','vol')]=W3['vol']
print('%-6s | %10s %8s | %10s %8s | %8s' % ('win','flat NET','flatDD','vol NET','volDD','NET d%'))
tf=tv=0; mddf=mddv=0
for w in ('w1','w2','w3'):
    fn,fd=rows[(w,'flat')]; vn,vd=rows[(w,'vol')]
    if fn is None or vn is None: print(w,'MISSING'); continue
    tf+=fn; tv+=vn; mddf=max(mddf,fd); mddv=max(mddv,vd)
    print('%-6s | %10.0f %8.0f | %10.0f %8.0f | %+7.0f%%' % (w,fn,fd,vn,vd,100*(vn/fn-1)))
print('%-6s | %10.0f %8s | %10.0f %8s | %+7.0f%%' % ('TOTAL',tf,'',tv,'',100*(tv/tf-1)))
print('worst single-window maxDD: flat %.0f  vol %.0f  (%.0f%% of $2k -> %.0f%%)' % (mddf,mddv,100*mddf/2000,100*mddv/2000))
print('return-per-worstDD: flat %.1f  vol %.1f' % (tf/mddf, tv/mddv))
PY
rm -f /tmp/volall/*.trades.jsonl
echo ALL_DONE
