# Golden replay: 2026-06-25

This is the pinned-tape regression harness for the walk-forward engine. It
locks a single day of live tape (288 five-minute BTC up/down markets on
2026-06-25) through the canonical `exo_fade` walk-forward config and hashes
the normalized output. Any change to the engine that is meant to be
behavior-preserving (a refactor, an extraction, a dependency bump) must
still produce `GOLDEN: IDENTICAL`. A divergence means either the engine's
observable output changed, or the change is not actually behavior-preserving
and needs review.

Run it with:

```
bash scripts/research/golden_replay.sh record   # only after an intentional behavior change
bash scripts/research/golden_replay.sh check     # the regression gate; run this normally
```

`check` exits 1 and prints `GOLDEN: DIVERGED` on any mismatch.

## What is hashed

The script runs `pm-app walk-forward` against the day's market manifest
(filtered from `data/manifests/canonical/btc-updown-5m_up.jsonl` by
`.date == "2026-06-25"`, 288 markets) with a fixed config: `exo_fade`
strategy, $1000 starting cash, $50 max clip, `BTCUSDT` spot symbol, outcome
labels from discovery, 1250ms taker latency, reading `data/cache` locally.
It then normalizes and hashes the combination of:

- `--out-markets`: the per-market JSONL output.
- `--out-summary`: the run summary JSON.

## What is normalized, and why

Two consecutive runs of the replay were diffed byte-for-byte to find any
nondeterminism before recording the golden hash (see "Determinism proof"
below). The following normalization is applied before hashing:

1. **Per-market row ordering.** `walk-forward` currently emits per-market
   rows in chronological (input manifest) order, which is deterministic
   across runs. As a defensive measure against a future change to
   parallel-execution scheduling reintroducing nondeterministic write
   order, the harness sorts rows by `asset_id` (`jq -s -c 'sort_by(.asset_id)'`)
   before hashing rather than trusting emission order.
2. **Object key ordering.** Both the per-market rows and the summary are
   passed through `jq -S` (sort keys) so that any incidental change to
   struct field order or `HashMap`/`serde_json::Map` iteration order in the
   engine does not register as a spurious divergence.
3. **Call-site output paths.** `run_config.shared.checkpoint_markets_out`
   and `run_config.shared.checkpoint_summary_out` in the summary JSON echo
   back the `--out-markets` / `--out-summary` arguments passed on the
   command line. These are artifacts of how the harness invokes the binary,
   not of the replay itself, so they are stripped
   (`del(.run_config.shared.checkpoint_markets_out, .run_config.shared.checkpoint_summary_out)`).

No timestamp, duration, or host field was found in `--out-markets` or
`--out-summary` output (see determinism proof); the binary does log
wall-clock timing (`elapsed_s=...`) and hostname info to stderr during the
run, but that goes to the terminal/log, not into either hashed file, so no
additional stripping was needed for those categories.

## Determinism proof

The replay was run twice back-to-back with identical inputs and config.
After the normalization above, the two runs' hashes were compared and found
identical, confirming the normalized output is deterministic:

```
run 1 (normalized) sha256: <see task-1-report.md for exact hashes>
run 2 (normalized) sha256: <see task-1-report.md for exact hashes>
```

Both hashes match. `tests/golden/day-2026-06-25.sha256` records this hash as
the committed golden value.

## Engine is read-only for this harness

This harness only observes engine output; the engine code was not touched
to make the harness pass. Any future genuine nondeterminism discovered in
the engine should be fixed by normalizing the harness's hashing (sorting,
key-ordering, stripping truly volatile fields), not by special-casing the
harness around a bug, and not by changing engine internals unless the
engine change is itself the reviewed unit of work.
