#!/usr/bin/env bash
# Recommended-config CLI flags: the deployed gated config MINUS min_entry_ask.
# docs/archive/2026-07/deployed-config-negative-2026-07.md: min_entry_ask 0.45 alone turned the
# June backtest from +$1,957 to -$149 by blocking the cheap-underdog fades
# (ask < 0.45), the payoff tail. This variant drops it (explicit 0.0) and keeps
# the open-fav + spot-misalign gates for their (cheap) drawdown protection.
# Purpose: soak the cheap-underdog entries LIVE so their realization can be
# measured against the matched replay - the experiment that decides whether the
# +$1,297 recovered config is real or a backtest mirage.
set -euo pipefail

SHADOW_RECOMMENDED_FLAGS=(
  --stop-before-close-s 90
  --min-entry-ask 0.0
  --skip-spot-misalign-s 30
  --skip-open-fav-gap
  --open-fav-p-min 0.88
  --open-fav-ask-max 0.62
  --open-fav-secs 300
)

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  printf '%s ' "${SHADOW_RECOMMENDED_FLAGS[@]}"
  echo
fi
