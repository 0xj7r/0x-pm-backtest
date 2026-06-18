#!/usr/bin/env bash
# SSOT CLI flags for gated shadow-final (mom30 + min_entry_ask 0.45 + prod_gap_full).
# Source this file for arrays, or run standalone to print a single line for append.
set -euo pipefail

SHADOW_FINAL_GATED_FLAGS=(
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