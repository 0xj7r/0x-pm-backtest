#!/usr/bin/env bash
# Foreground live-twin launcher for systemd (pm-shadow-final.service).
# Flags SSOT: shadow_flags.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ops/shadow_flags.sh
source "$SCRIPT_DIR/shadow_flags.sh"

BIN="${SHADOW_FINAL_BIN:-$HOME/pm-backtest/target/release/pm-app}"
OUT_DIR="${SHADOW_FINAL_OUT:-$HOME/data/pm-alpha/shadow-final}"
SLUG_PREFIX="${SHADOW_SLUG_PREFIX:-btc-updown-5m-}"
mkdir -p "$OUT_DIR"

exec "$BIN" shadow \
  --slug-prefix "$SLUG_PREFIX" \
  --out-dir "$OUT_DIR" \
  --decide-interval-ms "${SHADOW_DECIDE_INTERVAL_MS:-1000}" \
  "${SHADOW_FLAGS[@]}"
