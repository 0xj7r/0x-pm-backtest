#!/usr/bin/env bash
# Pinned-tape golden harness: replays a fixed day (2026-06-25) through the
# walk-forward runner with a fixed, canonical config and hashes the normalized
# output. `check` must print GOLDEN: IDENTICAL after any change that is
# supposed to be behavior-preserving.
#
# The replay runs the `fixture` strategy
# (pm-strategy::fixture::ThresholdFadeStrategy): deterministic, test-only, and
# not deployable. That is the point. A golden anchored to a tradeable strategy
# only gates the engine for as long as that strategy lives, and the exo_fade
# variant this harness used to carry submitted zero orders on the pinned day,
# so it gated almost none of the order/fill/settlement path. The fixture
# submits 126 orders across the day's 288 markets and all of them fill.
#
# Flag notes (verified against `./target/release/pm-app walk-forward --help`):
#   --fee-curve-rate exists (added with constraint 2) and defaults to the
#     canonical 0.07; this script deliberately omits it so the replay runs at
#     the canonical default. maker_rebate_bps=10.0 / taker_fee_bps=0.0 remain
#     fixed in code; the curve fee is charged on taker fills on top of those.
#   --latency-ms does not exist; the equivalent flag is --taker-latency-ms.
#   --allow-fixture is required; without it the CLI rejects the strategy id,
#     which is what keeps the fixture from being deployable.
#
# Usage: golden_replay.sh {record|check} [fixture]
#   The trailing `fixture` is optional and is the only accepted variant; it is
#   spelled out at call sites that want to be explicit about what is replayed.
#
#   record - run the replay, normalize the output, write its sha256 to
#            tests/golden/day-2026-06-25-fixture.sha256, and keep the full JSON
#            for inspection. Only after an INTENTIONAL behavior change: this
#            hash is the anchor and must not be re-recorded to make a refactor
#            pass.
#   check  - run the replay again, normalize, and compare against the
#            committed hash. Prints GOLDEN: IDENTICAL (exit 0) or
#            GOLDEN: DIVERGED (exit 1).

set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

MODE="${1:-}"
VARIANT="${2:-fixture}"
if [[ "$MODE" != "record" && "$MODE" != "check" ]]; then
  echo "usage: $0 {record|check} [fixture]" >&2
  exit 2
fi
if [[ "$VARIANT" != "fixture" ]]; then
  echo "error: unknown variant '$VARIANT'; the only variant is 'fixture' (the exo_fade variant retired with the strategy)" >&2
  exit 2
fi

BIN=./target/release/pm-app
MARKETS_MANIFEST=/tmp/golden-markets.jsonl

STRATEGY_ARGS=(--strategies fixture --allow-fixture)
BASENAME=day-2026-06-25-fixture

OUT_MARKETS="/tmp/golden-run-$VARIANT.jsonl"
OUT_SUMMARY="/tmp/golden-run-$VARIANT.json"
NORMALIZED="/tmp/golden-run-$VARIANT.normalized.json"
SORTED_MARKETS="/tmp/golden-run-$VARIANT.markets.sorted.json"
NORMALIZED_SUMMARY="/tmp/golden-run-$VARIANT.summary.normalized.json"
HASH_FILE="tests/golden/$BASENAME.sha256"

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
echo "day manifest: $MARKET_COUNT markets ($VARIANT variant)" >&2

# Step 2: run the canonical replay.
"$BIN" walk-forward \
  --markets "$MARKETS_MANIFEST" \
  "${STRATEGY_ARGS[@]}" \
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
jq -c -S '.' "$OUT_MARKETS" | jq -s -c 'sort_by(.asset_id)' > "$SORTED_MARKETS"
jq -S 'del(.config_fingerprint, .validation, .run_config.shared.checkpoint_markets_out, .run_config.shared.checkpoint_summary_out)' "$OUT_SUMMARY" > "$NORMALIZED_SUMMARY"

jq -n -c \
  --slurpfile markets "$SORTED_MARKETS" \
  --slurpfile summary "$NORMALIZED_SUMMARY" \
  '{markets: $markets[0], summary: $summary[0]}' > "$NORMALIZED"

HASH=$(shasum -a 256 "$NORMALIZED" | awk '{print $1}')

if [[ "$MODE" == "record" ]]; then
  echo "$HASH  $BASENAME.normalized.json" > "$HASH_FILE"
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
