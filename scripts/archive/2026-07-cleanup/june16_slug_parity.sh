#!/usr/bin/env bash
# Jun 16 slug-level parity: shadow-final JSONL vs offline alpha trades.
set -euo pipefail
cd "$(dirname "$0")/.."

DAY="${DAY:-2026-06-16}"
BIN="${BIN:-./target/release/pm-app}"
OUT="${OUT:-data/runs/june16_slug_parity}"
SHADOW="${SHADOW:-$OUT/shadow_${DAY}.jsonl}"
MANIFEST="${MANIFEST:-data/manifests/canonical/btc-updown-5m_up.jsonl}"

mkdir -p "$OUT"

if [[ ! -x "$BIN" ]]; then
  cargo build -p pm-app --release
fi

TRADES="$OUT/backtest_${DAY}_trades.jsonl"
JSON="$OUT/backtest_${DAY}.json"

echo "== gated alpha backtest $DAY =="
"$BIN" alpha \
  --markets "$MANIFEST" \
  --local-cache-dir data/cache \
  --tick-cache-dir data/cache/ticks \
  --date-start "$DAY" --date-end "$DAY" \
  --trades-out "$TRADES" \
  --out-json "$JSON" \
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
  --skip-spot-misalign-s 30 \
  --min-entry-ask 0.45 \
  > "$OUT/backtest_${DAY}.log" 2>&1

if [[ ! -f "$SHADOW" ]]; then
  echo "shadow JSONL missing: $SHADOW" >&2
  exit 1
fi

python3 scripts/shadow_slug_parity.py \
  --shadow "$SHADOW" \
  --backtest "$TRADES" \
  --day "$DAY" | tee "$OUT/parity_${DAY}.txt"