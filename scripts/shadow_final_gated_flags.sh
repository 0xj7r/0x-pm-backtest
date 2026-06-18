#!/usr/bin/env bash
# CLI flags for gated shadow-final (mom30 + min_entry_ask 0.45).
# Append to the existing shadow-final pm-app invocation on restart.
set -euo pipefail
echo "--skip-spot-misalign-s 30 --min-entry-ask 0.45"