#!/usr/bin/env bash
# CLI flags for gated shadow-final (mom30 + min_entry_ask 0.45 + prod_gap_full).
# Append to the existing shadow-final pm-app invocation on restart.
set -euo pipefail
echo "--skip-spot-misalign-s 30 --min-entry-ask 0.45 --skip-open-fav-gap --open-fav-p-min 0.88 --open-fav-ask-max 0.62 --open-fav-secs 300"