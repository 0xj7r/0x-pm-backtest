#!/usr/bin/env bash
# SSOT CLI flags for prod shadow-final (mom30 + lottery-band floor + prod_gap_full).
# Regime stand-down flags (--skip-calm / --skip-expanded-mixed) were removed
# 2026-07-01: fit on Jun 14-19 live tape only, they blocked 99% of entries
# out-of-sample Jun 20-30 (10 entries in 10 days). Do not re-add without
# all-window backtest evidence + 48h paper parity.
# Source this file for arrays, or run standalone to print a single line for append.
set -euo pipefail

SHADOW_FINAL_GATED_FLAGS=(
  # Validated 90s pre-close stop, pinned explicitly (config-consistency-audit
  # M-1). The clap default now equals the frozen 90 (parity gate), but the SSOT
  # stays explicit so the running cmdline is self-describing for verify_deploy.
  --stop-before-close-s 90
  --skip-spot-misalign-s 30
  --min-entry-ask 0.45
  --skip-open-fav-gap
  --open-fav-p-min 0.88
  --open-fav-ask-max 0.62
  --open-fav-secs 300
)

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  printf '%s ' "${SHADOW_FINAL_GATED_FLAGS[@]}"
  echo
fi
