#!/usr/bin/env bash
# Validate the deep selldown-stop (eps 0.10/0.15) on W1 (trend) + W2 (mixed) -
# the windows where a stop could HURT by cutting the fade's winning reversions.
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
OUT=/tmp/wstop; mkdir -p "$OUT"
COMMON="--local-cache-dir data/cache --markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m- --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --stop-before-close-s 90 --fee-curve-rate 0.07 --perp-symbol BTCUSDT --perp-price-weight 0.75 --edge-thresholds 0.12 --exit-after-s 0 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 --vol-estimator realized --vol-lookback-s 3600 --notional-usdc 15"
go(){ $BIN alpha $COMMON --selldown-stop-eps $4 --date-start $2 --date-end $3 --trades-out "$OUT/$1.trades.jsonl" --out-json "$OUT/$1.json" > "$OUT/$1.log" 2>&1; echo "done $1"; }
( go w1_e10 2026-02-12 2026-03-31 0.10 ) &
( go w1_e15 2026-02-12 2026-03-31 0.15 ) &
( go w2_e10 2026-04-01 2026-04-30 0.10 ) &
( go w2_e15 2026-04-01 2026-04-30 0.15 ) &
wait
python3 - <<'PY'
import json
def stat(l):
    p=[]; ns=0; real=hold=0.0
    try:
        for line in open(f'/tmp/wstop/{l}.trades.jsonl'):
            t=json.loads(line); p.append((t.get('decision_ts_ns',0),t.get('pnl',0.0)))
            if t.get('stopped'): ns+=1; real+=t.get('pnl',0.0); hold+=t.get('stop_hold_pnl',0.0) or 0.0
    except FileNotFoundError: return None
    p.sort(); eq=pk=mdd=0.0
    for _,x in p: eq+=x; pk=max(pk,eq); mdd=max(mdd,pk-eq)
    return sum(x for _,x in p),mdd,ns,real-hold
base={'w1':(24578,778),'w2':(16685,1282)}
print('%-7s %8s %8s | %9s %8s | %7s %9s' % ('arm','NET','maxDD','flatNET','flatDD','n_stop','stop_gain'))
for w in ('w1','w2'):
    bn,bd=base[w]
    for e in ('e10','e15'):
        r=stat(f'{w}_{e}')
        if not r: print(f'{w}_{e} MISSING'); continue
        net,mdd,ns,gain=r
        print('%-7s %8.0f %8.0f | %9.0f %8.0f | %7d %+9.0f' % (f'{w}_{e}',net,mdd,bn,bd,ns,gain))
PY
rm -f /tmp/wstop/*.trades.jsonl
echo ALL_DONE
