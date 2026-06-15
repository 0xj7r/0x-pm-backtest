#!/usr/bin/env bash
# Deep "salvage" selldown-stop sweep on W3: does selling clearly-losing held
# positions beat holding to redemption? Uses the trade record's stop_hold_pnl
# (counterfactual hold P&L) to measure it directly. Chains after the vol-sizing
# sweep so the CPU is free. $15 clips, target/fast.
set -uo pipefail
cd "$(dirname "$0")/.."
until grep -q ALL_DONE /tmp/volall_run.log 2>/dev/null; do sleep 30; done
echo "[$(date -u +%H:%M:%S)] vol-sizing sweep done; starting deep-stop sweep"
BIN=./target/fast/pm-app
OUT=/tmp/stopcmp
mkdir -p "$OUT"
COMMON="--local-cache-dir data/cache --markets data/manifests/canonical/btc-updown-5m_up.jsonl --slug-prefix btc-updown-5m- --down-assets data/manifests/canonical/down_all.jsonl --tick-cache-dir data/cache/ticks --latency-ms 150 --stop-before-close-s 90 --fee-curve-rate 0.07 --perp-symbol BTCUSDT --perp-price-weight 0.75 --edge-thresholds 0.12 --exit-after-s 0 --rearm-edge 0.08 --max-clips 2 --min-marginal-edge 0.04 --vol-estimator realized --vol-lookback-s 3600 --notional-usdc 15"
W3="--date-start 2026-05-07 --date-end 2026-05-18"
go(){ echo "[$(date -u +%H:%M:%S)] $1"; $BIN alpha $COMMON --selldown-stop-eps $2 $W3 --trades-out "$OUT/$1.trades.jsonl" --out-json "$OUT/$1.json" > "$OUT/$1.log" 2>&1; }
go flat   -1.0
go e10    0.10
go e15    0.15
go e20    0.20
go e25    0.25

python3 - <<'PY'
import json
def stat(label):
    pnls=[]; n=ns=0; real=hold=0.0
    try:
        for line in open(f'/tmp/stopcmp/{label}.trades.jsonl'):
            t=json.loads(line); pnls.append((t.get('decision_ts_ns',0), t.get('pnl',0.0))); n+=1
            if t.get('stopped'):
                ns+=1; real+=t.get('pnl',0.0); hold+=t.get('stop_hold_pnl',0.0) or 0.0
    except FileNotFoundError: return None
    pnls.sort(); eq=peak=mdd=0.0
    for _,p in pnls:
        eq+=p; peak=max(peak,eq); mdd=max(mdd,peak-eq)
    return sum(p for _,p in pnls), mdd, n, ns, real, hold
print('%-6s %9s %8s %8s %9s %9s %10s' % ('eps','NET','maxDD','n_stop','stop_real','stop_hold','stop_gain'))
for a in ('flat','e10','e15','e20','e25'):
    r=stat(a)
    if not r: print('%-6s MISSING'%a); continue
    net,mdd,n,ns,real,hold=r
    gain=real-hold  # >0 = selling beat holding on the stopped trades
    print('%-6s %9.0f %8.0f %8d %9.0f %9.0f %+10.0f' % (a,net,mdd,ns,real,hold,gain))
print('stop_gain > 0 => selling the losers beat holding them; < 0 => holding was better (stop cut reversions)')
PY
rm -f /tmp/stopcmp/*.trades.jsonl
echo ALL_DONE
