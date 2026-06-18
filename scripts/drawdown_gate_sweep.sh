#!/usr/bin/env bash
# Drawdown gate sweep — compare production gates vs gap/loss/momentum variants.
#
# Focus windows: JUN0117 (full June cache), JUN1017 (recent live drawdown band),
# VERIFY/HOLDOUT sanity.
#
# Usage:
#   ./scripts/drawdown_gate_sweep.sh
#   WINDOWS="JUN1017" VARIANTS="prod_mom30_ask045 prod_gap_full" ./scripts/drawdown_gate_sweep.sh
#   EXPORT_TRADES=1 ./scripts/drawdown_gate_sweep.sh   # writes .trades.jsonl for whipsaw validate
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/drawdown_gate_sweep}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"
TICKS="${TICKS:-data/cache/ticks}"
WINDOWS="${WINDOWS:-JUN0117 JUN1017 VERIFY HOLDOUT}"
VARIANTS="${VARIANTS:-prod_mom30_ask045 prod_gap_open5 prod_gap_full prod_gap_full_p90 prod_gap_full_p85 prod_loss2 prod_gap_full_loss2 prod_mom60 prod_mom300 prod_against}"
EXPORT_TRADES="${EXPORT_TRADES:-0}"
SKIP_EXISTING="${SKIP_EXISTING:-1}"

mkdir -p "$OUT"

if [[ ! -x "$BIN" ]]; then
  cargo build -p pm-app --release
fi

COMMON=(
  --markets "$MANIFEST"
  --local-cache-dir "$CACHE"
  --tick-cache-dir "$TICKS"
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
)

window_dates() {
  case "$1" in
    VERIFY)   echo "2026-05-07 2026-05-18" ;;
    HOLDOUT)  echo "2026-05-19 2026-05-28" ;;
    JUNE)     echo "2026-06-01 2026-06-07" ;;
    JUN1017)  echo "2026-06-10 2026-06-17" ;;
    JUN0117)  echo "2026-06-01 2026-06-17" ;;
    *) echo "unknown window $1" >&2; exit 1 ;;
  esac
}

gate_flags() {
  # Production baseline: mom30 + min ask 0.45 (current shadow-final stack).
  local base=(--skip-spot-misalign-s 30 --min-entry-ask 0.45)
  case "$1" in
    prod_mom30_ask045)     printf '%s\n' "${base[@]}" ;;
    prod_gap_open5)        printf '%s\n' "${base[@]}" --skip-open-fav-gap ;;
    prod_gap_full)         printf '%s\n' "${base[@]}" --skip-open-fav-gap --open-fav-p-min 0.88 --open-fav-ask-max 0.62 --open-fav-secs 300 ;;
    prod_gap_full_p90)     printf '%s\n' "${base[@]}" --skip-open-fav-gap --open-fav-p-min 0.90 --open-fav-ask-max 0.60 --open-fav-secs 300 ;;
    prod_gap_full_p85)     printf '%s\n' "${base[@]}" --skip-open-fav-gap --open-fav-p-min 0.85 --open-fav-ask-max 0.65 --open-fav-secs 300 ;;
    prod_loss2)            printf '%s\n' "${base[@]}" --pause-after-consec-losses 2 ;;
    prod_gap_full_loss2)   printf '%s\n' "${base[@]}" --skip-open-fav-gap --open-fav-p-min 0.88 --open-fav-ask-max 0.62 --open-fav-secs 300 --pause-after-consec-losses 2 ;;
    prod_mom60)            printf '%s\n' --skip-spot-misalign-s 60 --min-entry-ask 0.45 ;;
    prod_mom300)           printf '%s\n' --skip-spot-misalign-s 300 --min-entry-ask 0.45 ;;
    prod_against)          printf '%s\n' "${base[@]}" --skip-spot-against-all ;;
    *) echo "unknown variant $1" >&2; exit 1 ;;
  esac
}

summary_tsv="$OUT/summary.tsv"
if [[ ! -s "$summary_tsv" ]]; then
  echo -e "window\tvariant\tn_trades\tnet_usd\thit_pct\tper_trade\tmax_dd_day" > "$summary_tsv"
fi

for win in $WINDOWS; do
  read -r DS DE <<< "$(window_dates "$win")"
  for var in $VARIANTS; do
    tag="${win}_${var}"
    log="$OUT/${tag}.log"
    json="$OUT/${tag}.json"
    trades="$OUT/${tag}.trades.jsonl"
    if [[ "$SKIP_EXISTING" == "1" && -s "$json" ]]; then
      echo "== skip $tag (exists) =="
    else
      echo "== $tag ($DS .. $DE) =="
      extra=()
      while IFS= read -r flag; do extra+=("$flag"); done < <(gate_flags "$var")
      if [[ "$EXPORT_TRADES" == "1" ]]; then
        "$BIN" alpha "${COMMON[@]}" "${extra[@]}" \
          --date-start "$DS" --date-end "$DE" \
          --out-json "$json" \
          --trades-out "$trades" \
          > "$log" 2>&1
      else
        "$BIN" alpha "${COMMON[@]}" "${extra[@]}" \
          --date-start "$DS" --date-end "$DE" \
          --out-json "$json" \
          > "$log" 2>&1
      fi
    fi
    python3 - "$json" "$win" "$var" "$summary_tsv" "$DS" "$DE" <<'PY'
import json, sys
from collections import defaultdict
from datetime import date, timedelta
from pathlib import Path

path, win, var, tsv, ds, de = sys.argv[1:7]
with open(path) as f:
    r = json.load(f)
cells = r.get("sweep") or [r]
rep = cells[0].get("report", {})
agg = rep.get("aggregate") or rep.get("cells", {}).get("BTC-300s", {})
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", agg.get("net_pnl_usd", 0)))
hit = float(agg.get("hit_rate", 0)) * 100
per = net / n if n else 0.0

# Worst single-day PnL if daily breakdown present
max_dd_day = 0.0
daily = agg.get("daily_pnl") or agg.get("by_day") or {}
if isinstance(daily, dict) and daily:
    max_dd_day = min(float(v) for v in daily.values())
elif "cells" in rep:
    pass

lines = Path(tsv).read_text().splitlines()
header, body = lines[0], lines[1:]
body = [ln for ln in body if ln.strip() and not ln.startswith(f"{win}\t{var}\t")]
body.append(f"{win}\t{var}\t{n}\t{net:.2f}\t{hit:.1f}\t{per:.2f}\t{max_dd_day:.2f}")
Path(tsv).write_text(header + "\n" + "\n".join(body) + "\n")
print(f"  n={n} NET=${net:+,.0f} hit={hit:.1f}% ${per:+.2f}/tr")
PY
  done
done

echo ""
echo "=== Drawdown gate sweep summary ==="
python3 - "$summary_tsv" <<'PY'
import sys
from collections import defaultdict
from pathlib import Path

rows = []
for line in Path(sys.argv[1]).read_text().splitlines()[1:]:
    if not line.strip():
        continue
    win, var, n, net, hit, per, *rest = line.split("\t")
    rows.append((win, var, int(n), float(net), float(hit), float(per)))

by_win = defaultdict(list)
for r in rows:
    by_win[r[0]].append(r)

for win in sorted(by_win):
    rs = sorted(by_win[win], key=lambda x: -x[3])
    print(f"\n## {win}")
    print(f"{'variant':<24} {'trades':>7} {'NET':>12} {'hit%':>7} {'$/tr':>8}")
    print("-" * 62)
    base_net = next((r[3] for r in rs if r[1] == "prod_mom30_ask045"), None)
    for r in rs:
        delta = f" ({r[3]-base_net:+.0f})" if base_net is not None and r[1] != "prod_mom30_ask045" else ""
        print(f"{r[1]:<24} {r[2]:>7} ${r[3]:>10,.0f} {r[4]:>6.1f}% ${r[5]:>+7.2f}{delta}")
PY

echo ""
echo "Wrote $summary_tsv"