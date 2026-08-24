#!/usr/bin/env bash
# Pinned-tape golden harness: replays a fixed day (2026-06-25) through the
# exo_fade walk-forward runner with a fixed, canonical config and hashes the
# normalized output. Used as a byte-for-byte regression gate by later engine
# extraction tasks: `check` must print GOLDEN: IDENTICAL after any refactor
# that is supposed to be behavior-preserving.
#
# Flag notes vs. the original task brief (verified against
# `./target/release/pm-app walk-forward --help` on this branch):
#   --fee-curve-rate does not exist on `walk-forward` (it is an alpha-hunt-only
#     flag on HarnessConfig). walk-forward's fee/rebate accounting is fixed in
#     code (maker_rebate_bps=10.0, taker_fee_bps=0.0) regardless of CLI args;
#     this is the "canonical accounting" referenced in the commit message, so
#     the flag is simply omitted here.
#   --latency-ms does not exist; the equivalent flag is --taker-latency-ms.
#
# Usage: golden_replay.sh {record|check}
#   record - run the replay, normalize the output, write its sha256 to
#            tests/golden/day-2026-06-25.sha256, and keep the full JSON at
#            /tmp/golden-run.json for inspection.
#   check  - run the replay again, normalize, and compare against the
#            committed hash. Prints GOLDEN: IDENTICAL (exit 0) or
#            GOLDEN: DIVERGED (exit 1).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

MODE="${1:-}"
if [[ "$MODE" != "record" && "$MODE" != "check" ]]; then
  echo "usage: $0 {record|check}" >&2
  exit 2
fi

BIN=./target/release/pm-app
MARKETS_MANIFEST=/tmp/golden-markets.jsonl
OUT_MARKETS=/tmp/golden-run.jsonl
OUT_SUMMARY=/tmp/golden-run.json
NORMALIZED=/tmp/golden-run.normalized.json
HASH_FILE=tests/golden/day-2026-06-25.sha256

if [[ ! -x "$BIN" ]]; then
  echo "error: $BIN not found or not executable; run: cargo build --release -p pm-app" >&2
  exit 1
fi

# Step 1: rebuild the day manifest (self-contained). The canonical manifest
# encodes the market's date in a top-level "date" field (not the slug, which
# only carries the market's close epoch), so filter on that.
jq -c 'select(.date == "2026-06-25")' data/manifests/canonical/btc-updown-5m_up.jsonl > "$MARKETS_MANIFEST"

MARKET_COUNT=$(wc -l < "$MARKETS_MANIFEST" | tr -d ' ')
if [[ "$MARKET_COUNT" -eq 0 ]]; then
  echo "error: manifest filter matched zero markets for date 2026-06-25 (check the manifest's date encoding)" >&2
  exit 1
fi
echo "day manifest: $MARKET_COUNT markets" >&2

# Step 2: run the canonical replay.
"$BIN" walk-forward \
  --markets "$MARKETS_MANIFEST" \
  --strategies exo_fade \
  --starting-cash 1000 \
  --max-clip-usdc 50 \
  --spot-symbol BTCUSDT \
  --use-outcome-label \
  --taker-latency-ms 1250 \
  --local-cache-dir data/cache \
  --out-markets "$OUT_MARKETS" \
  --out-summary "$OUT_SUMMARY"

# Step 3: normalize. Per-market rows are already emitted in chronological
# (input) order, but we sort defensively by asset_id in case that ever
# changes under parallel execution. The summary's checkpoint_markets_out /
# checkpoint_summary_out fields echo back the --out-* paths, which are
# call-site artifacts (e.g. differ between /tmp/golden-run.json and a
# future --record-to override) rather than replay output, so they are
# stripped too. `.validation` is an always-present scorecard field (defaults
# to "UNVALIDATED") unrelated to replay bytes, so it is stripped as well;
# the skip-serializing-if optional blocks (jitter, window_label, sizing)
# stay None in this non-jitter, no-bankroll run and never reach the JSON.
jq -c -S '.' "$OUT_MARKETS" | jq -s -c 'sort_by(.asset_id)' > /tmp/golden-run.markets.sorted.json
jq -S 'del(.config_fingerprint, .validation, .run_config.shared.checkpoint_markets_out, .run_config.shared.checkpoint_summary_out)' "$OUT_SUMMARY" > /tmp/golden-run.summary.normalized.json

jq -n -c \
  --slurpfile markets /tmp/golden-run.markets.sorted.json \
  --slurpfile summary /tmp/golden-run.summary.normalized.json \
  '{markets: $markets[0], summary: $summary[0]}' > "$NORMALIZED"

HASH=$(shasum -a 256 "$NORMALIZED" | awk '{print $1}')

if [[ "$MODE" == "record" ]]; then
  echo "$HASH  day-2026-06-25.normalized.json" > "$HASH_FILE"
  echo "recorded hash: $HASH"
  exit 0
fi

# check mode
if [[ ! -f "$HASH_FILE" ]]; then
  echo "error: no committed hash at $HASH_FILE; run 'record' first" >&2
  exit 1
fi

EXPECTED=$(awk '{print $1}' "$HASH_FILE")
if [[ "$HASH" == "$EXPECTED" ]]; then
  echo "GOLDEN: IDENTICAL"
  exit 0
else
  echo "GOLDEN: DIVERGED (expected $EXPECTED, got $HASH)" >&2
  echo "GOLDEN: DIVERGED"
  exit 1
fi
