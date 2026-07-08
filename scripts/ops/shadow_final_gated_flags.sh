#!/usr/bin/env bash
# SSOT CLI flags for prod shadow-final (mom30 + lottery-band floor + prod_gap_full).
# Regime stand-down flags (--skip-calm / --skip-expanded-mixed) were removed
# 2026-07-01: fit on Jun 14-19 live tape only, they blocked 99% of entries
# out-of-sample Jun 20-30 (10 entries in 10 days). Do not re-add without
# all-window backtest evidence + 48h paper parity.
# Source this file for arrays, or run standalone to print a single line for append.
set -euo pipefail

SHADOW_FINAL_GATED_FLAGS=(
  # Validated 90s pre-close stop. Explicit here because the shadow subcommand's
  # clap default is 10, and sync_decide_cfg propagates the CLI value into the
  # decide gate; omitting this flag made shadow-final enter up to 10s before
  # close (vs the backtest/fast_live 90s window) - config-consistency-audit M-1.
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
