#!/usr/bin/env bash
# Family A (momentum drift) under the hunt protocol: tune on May 7-18 against
# the exit-30s champion; verify on May 19-28 ONLY if tune beats the
# no-momentum baseline (exit30/skip0 tune total: $21,476).
set -uo pipefail
cd "$(dirname "$0")/.."
BIN=./target/fast/pm-app
MAY=data/manifests/may2026_focused/markets_btc.jsonl
NIGHT=data/runs/alpha/overnight
BASELINE_TUNE_PNL=21476

pids=()
for MOM in 60 300; do
  for W in 0.5 1.0; do
    NAME="momA_m${MOM}_w${W}"
    "$BIN" alpha --local-cache-dir data/cache --markets "$MAY" \
      --date-start 2026-05-07 --date-end 2026-05-18 \
      --latency-ms 150 --edge-thresholds 0.12 --vol-lookback-s 3600 \
      --exit-after-s 30 --momentum-lookback-s "$MOM" --momentum-weight "$W" \
      --out-json "$NIGHT/$NAME.json" --trades-out "$NIGHT/$NAME.trades.jsonl" \
      > "$NIGHT/$NAME.log" 2>&1 &
    pids+=($!)
  done
done
for p in "${pids[@]}"; do wait "$p" || echo "WARN momA job failed"; done

BEST=$(python3 - <<'EOF'
import json, glob
best=(None,-1e18)
for f in glob.glob('data/runs/alpha/overnight/momA_m*_w*.json'):
    d=json.loads(open(f).read())
    pnl=d['sweep'][0]['report']['aggregate']['total_pnl']
    print(f"# {f}: {pnl:.0f}")
    if pnl>best[1]: best=(f,pnl)
print(f"{best[0]}|{best[1]:.0f}")
EOF
)
echo "$BEST"
TOP=$(echo "$BEST" | tail -1)
PNL=${TOP##*|}
FILE=${TOP%%|*}
if [ "${PNL%.*}" -gt "$BASELINE_TUNE_PNL" ]; then
  MOM=$(python3 -c "import json;print(json.loads(open('$FILE').read())['model_cfg']['momentum_lookback_s'])")
  W=$(python3 -c "import json;print(json.loads(open('$FILE').read())['model_cfg']['momentum_weight'])")
  echo "momentum improves tune ($PNL > $BASELINE_TUNE_PNL): verifying m=$MOM w=$W on test window"
  "$BIN" alpha --local-cache-dir data/cache --markets "$MAY" \
    --date-start 2026-05-19 --date-end 2026-05-28 \
    --latency-ms 150 --edge-thresholds 0.12 --vol-lookback-s 3600 \
    --exit-after-s 30 --momentum-lookback-s "$MOM" --momentum-weight "$W" \
    --out-json "$NIGHT/momA_verify_test.json" --trades-out "$NIGHT/momA_verify_test.trades.jsonl" \
    > "$NIGHT/momA_verify_test.log" 2>&1 || echo "WARN verify failed"
else
  echo "momentum does NOT improve tune ($PNL <= $BASELINE_TUNE_PNL): family rejected, no test-window spend"
fi
echo MOMENTUM_FAMILY_DONE
