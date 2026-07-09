#!/usr/bin/env bash
# Foreground launcher for the RECOMMENDED-config shadow stream (no min_entry_ask).
# Mirrors shadow_final_foreground.sh but sources shadow_recommended_flags.sh and
# writes to its own out dir. Paper/log-only. See docs/deployed-config-negative-2026-07.md.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ops/shadow_recommended_flags.sh
source "$SCRIPT_DIR/shadow_recommended_flags.sh"

BIN="${SHADOW_FINAL_BIN:-$HOME/pm-backtest/target/release/pm-app}"
OUT_DIR="${SHADOW_RECOMMENDED_OUT:-$HOME/data/pm-alpha/shadow-recommended}"
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
  --decide-interval-ms "${SHADOW_DECIDE_INTERVAL_MS:-1000}" \
  "${SHADOW_RECOMMENDED_FLAGS[@]}"
