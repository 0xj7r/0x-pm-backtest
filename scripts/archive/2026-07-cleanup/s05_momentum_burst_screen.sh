#!/usr/bin/env bash
# S05 spot momentum burst screen — directional-day sleeve (aligned continuation).
#
# Thesis: when BTC spot moves in the lookback window, buy the ALIGNED PM side
# before the book reprices. Compare fade baseline (prod gates) vs aligned variants.
#
# Usage:
#   ./scripts/s05_momentum_burst_screen.sh
#   WINDOWS=SMOKE ./scripts/s05_momentum_burst_screen.sh   # 1-day smoke (2026-06-10)
#   WINDOWS=TUNE VARIANTS="prod_mom30_ask045 s05_mom60_aligned" ./scripts/s05_momentum_burst_screen.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/s05_momentum_burst}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"
CACHE="${CACHE:-data/cache}"
TICKS="${TICKS:-data/cache/ticks}"
# PM book cache starts 2026-05-07; TUNE (Feb–Apr) has no telonex books — use VERIFY/HOLDOUT/JUN0117.
WINDOWS="${WINDOWS:-VERIFY HOLDOUT JUN0117}"
VARIANTS="${VARIANTS:-prod_mom30_ask045 s05_mom60_aligned s05_mom30_aligned}"
SKIP_EXISTING="${SKIP_EXISTING:-1}"

mkdir -p "$OUT"

if [[ ! -x "$BIN" ]]; then
  cargo build -p pm-app --release
fi

COMMON=(
  --markets "$MANIFEST"
  --slug-prefix btc-updown-5m-
  --down-assets data/manifests/canonical/down_all.jsonl
  --local-cache-dir "$CACHE"
  --tick-cache-dir "$TICKS"
  --exit-after-s 0
  --perp-symbol BTCUSDT
  --perp-price-weight 0.75
  --vol-estimator realized
  --vol-lookback-s 3600
  --edge-thresholds 0.12
  --notional-usdc 50
  --latency-ms 150
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
    TUNE)    echo "2026-02-12 2026-04-30" ;;  # requires telonex books (not in local cache)
    VERIFY)  echo "2026-05-07 2026-05-18" ;;
    HOLDOUT) echo "2026-05-19 2026-05-28" ;;
    JUN0117) echo "2026-06-01 2026-06-17" ;;
    SMOKE)   echo "2026-06-10 2026-06-10" ;;
    *) echo "unknown window $1" >&2; exit 1 ;;
  esac
}

gate_flags() {
  case "$1" in
    # Fade baseline from drawdown_gate_sweep (production shadow-final gates).
    prod_mom30_ask045)
      printf '%s\n' --skip-spot-misalign-s 30 --min-entry-ask 0.45
      ;;
    # S05 aligned continuation: require spot agreement, wider ask band.
    s05_mom60_aligned)
      printf '%s\n' \
        --aligned-mode --align-min-mid 0.55 \
        --skip-spot-misalign-s 60 \
        --min-entry-ask 0.35 --max-entry-ask 0.75
      ;;
    s05_mom30_aligned)
      printf '%s\n' \
        --aligned-mode --align-min-mid 0.55 \
        --skip-spot-misalign-s 30 \
        --min-entry-ask 0.35 --max-entry-ask 0.75
      ;;
    *)
      echo "unknown variant $1" >&2
      exit 1
      ;;
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
    if [[ "$SKIP_EXISTING" == "1" && -s "$json" ]]; then
      echo "== skip $tag (exists) =="
    else
      echo "== $tag ($DS .. $DE) =="
      extra=()
      while IFS= read -r flag; do extra+=("$flag"); done < <(gate_flags "$var")
      "$BIN" alpha "${COMMON[@]}" "${extra[@]}" \
        --date-start "$DS" --date-end "$DE" \
        --out-json "$json" \
        > "$log" 2>&1
    fi
    python3 - "$json" "$win" "$var" "$summary_tsv" "$DS" "$DE" <<'PY'
import json, sys
from pathlib import Path

path, win, var, tsv, ds, de = sys.argv[1:7]
with open(path) as f:
    r = json.load(f)
skip_load = int(r.get("n_skipped_load_error", 0))
if skip_load:
    print(f"  WARN n_skipped_load_error={skip_load} (bad/missing book cache)", file=sys.stderr)
cells = r.get("sweep") or [r]
rep = cells[0].get("report", {})
agg = rep.get("aggregate") or rep.get("cells", {}).get("BTC-300s", {})
n = int(agg.get("n_trades", 0))
net = float(agg.get("total_pnl", agg.get("net_pnl_usd", 0)))
hit = float(agg.get("hit_rate", 0)) * 100
per = net / n if n else 0.0

max_dd_day = 0.0
daily = agg.get("daily_pnl") or agg.get("by_day") or {}
if isinstance(daily, dict) and daily:
    max_dd_day = min(float(v) for v in daily.values())

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
echo "=== S05 momentum burst summary ==="
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