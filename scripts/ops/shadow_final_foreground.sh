#!/usr/bin/env bash
# Foreground shadow-final launcher for systemd (pm-shadow-final.service).
# Flags SSOT: shadow_final_gated_flags.sh; base flags mirror
# shadow_final_restart_gated.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ops/shadow_final_gated_flags.sh
source "$SCRIPT_DIR/shadow_final_gated_flags.sh"

BIN="${SHADOW_FINAL_BIN:-$HOME/pm-backtest/target/release/pm-app}"
OUT_DIR="${SHADOW_FINAL_OUT:-$HOME/data/pm-alpha/shadow-final}"
SLUG_PREFIX="${SHADOW_SLUG_PREFIX:-btc-updown-5m-}"
mkdir -p "$OUT_DIR"

exec "$BIN" shadow \
  --slug-prefix "$SLUG_PREFIX" \
  --edge-threshold 0.12 \
  --perp-price-weight 0.75 \
  --exit-after-s 0 \
  --rearm-edge 0.08 \
  --max-clips 2 \
  --min-entry-sigma-bps 3.0 \
  --vol-estimator realized \
  --vol-lookback-s 3600 \
  --skip-saturday \
  --out-dir "$OUT_DIR" \
  "${SHADOW_FINAL_GATED_FLAGS[@]}"
