#!/usr/bin/env bash
# SSOT CLI flags for the live twin (`pm-app shadow`).
#
# Engine flags only. Entry thresholds, gates and sizing live with the strategy,
# and the only strategy the twin will drive is `noop`, so this stream is
# entry-free until a deployable strategy exists. Pinned against
# `pm_shadow::default_shadow_args` by the pm-app config-parity test: change one
# and the test tells you to change the other.
# Source this file for the array, or run standalone to print a single line.
set -euo pipefail

SHADOW_FLAGS=(
  --strategy noop
  --perp-price-weight 0.75
  --exit-after-s 0
  --vol-estimator realized
  --vol-lookback-s 3600
)

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  printf '%s ' "${SHADOW_FLAGS[@]}"
  echo
fi
