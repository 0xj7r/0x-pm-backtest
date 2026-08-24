# Golden replay: 2026-06-25

This is the pinned-tape regression harness for the walk-forward engine. It
locks a single day of live tape (288 five-minute BTC up/down markets on
2026-06-25) through a fixed walk-forward config and hashes the normalized
output. Any change to the engine that is meant to be
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

## The anchor is the fixture replay

The day is replayed through `fixture`
(`pm_strategy::fixture::ThresholdFadeStrategy`), a deterministic test-only
strategy that is not deployable: the CLI rejects the id unless
`--allow-fixture` is also passed. `golden_replay.sh {record|check}` runs it;
a trailing `fixture` argument is accepted so call sites can be explicit, and
is the only variant.

It is the anchor because a golden that depends on a tradeable strategy only
gates the engine for as long as that strategy is alive. The fixture submits
126 orders across the day's 288 markets, all of which fill as takers
(3149.42 shares, mean slippage 15.00 bps), so the hash covers order
submission, depth-weighted taker fills, the taker fee curve, mark-to-market,
settlement, and per-strategy aggregation. It is deliberately a loser
(-$65.95 over the day): it is plumbing, not alpha, and no one should mistake
it for a strategy.

The rule (`crates/pm-strategy/src/fixture.rs`): after a 16-event warmup,
once per market, buy the cheap side at a fixed 25-share clip when the YES
mid sits outside a fixed 0.35/0.65 band, then hold to resolution. No spot,
no trade tape, no model, no config surface. It reads only `event.yes_mid`
and `ctx.events_seen`, so it is insensitive to `Ctx` fields being removed,
which is exactly what makes it a stable anchor across the engine slimming.

### The retired exo_fade variant

The harness was originally anchored to an `exo_fade` replay hashed into
`tests/golden/day-2026-06-25.sha256`. That file and that variant were
deleted when `exo_fade` was deleted. It is worth recording WHY it was a weak
gate, so nobody rebuilds the same thing: `exo_fade` submitted only **5**
orders across the day's 288 markets on 2026-06-25 (all 5 filled as takers, 0
maker fills; see the fee-curve audit table below), so its hash mostly pinned
the loader, the manifest walk, and 283 markets of no-op rows, and would have
caught only a sliver of a fill-model or settlement regression. Its last
recorded value was
`41313410e8a700ca40dd467b0fb62d767b202fda536f2af66b084b9d94f1f83a`; the
history and the audit trail below are kept for provenance.

The `exo_fade_equivalence` GATE B parity harness
(`pm-alpha::equivalence` plus its CI test and the `pm-app` report bin)
retired at the same commit, for the same reason: it proved that the backtest
and live paths built identical `DecisionInputs` for the fade, and there is
no longer a fade.

## What is hashed

The script runs `pm-app walk-forward` against the day's market manifest
(filtered from `data/manifests/canonical/btc-updown-5m_up.jsonl` by
`.date == "2026-06-25"`, 288 markets) with a fixed config: `fixture`
strategy (behind `--allow-fixture`), $1000 starting cash, $50 max clip,
`BTCUSDT` spot symbol, outcome labels from discovery, 1250ms taker latency,
reading `data/cache` locally.
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

## Fixture anchor recorded 2026-08-24

`tests/golden/day-2026-06-25-fixture.sha256` was recorded on 2026-08-24 at
the pre-kill tree (the exo variant still `GOLDEN: IDENTICAL` at hash
`41313410e8a700ca40dd467b0fb62d767b202fda536f2af66b084b9d94f1f83a`, GATE B
still PASS), so the anchor is pinned to known-good engine behavior before
anything was deleted:

```
7edbce58530c588860752e53578a32a466831d6943113a78acf394a7e5356263
```

Determinism was proved the same way as the exo variant: `record fixture`
followed by two independent `check fixture` runs, both `GOLDEN: IDENTICAL`.

From that commit onward the fixture hash is the gate, and it must NOT be
re-recorded to make a refactor pass. A divergence after the anchor means
the engine's observable behavior changed and needs bisecting, not a new
hash.

## Engine is read-only for this harness

This harness only observes engine output; the engine code was not touched
to make the harness pass. Any future genuine nondeterminism discovered in
the engine should be fixed by normalizing the harness's hashing (sorting,
key-ordering, stripping truly volatile fields), not by special-casing the
harness around a bug, and not by changing engine internals unless the
engine change is itself the reviewed unit of work.

## History: the retired exo_fade variant

Everything from here down documents the retired `exo_fade` variant and is
kept for provenance only. None of these hashes is a live gate; the live gate
is `day-2026-06-25-fixture.sha256`, recorded under "Fixture anchor" above.

### Determinism proof (exo variant)

The replay was run twice back-to-back with identical inputs and config
(record on 2026-08-24, then an independent full `check` re-run). After the
normalization above, both runs produced the same normalized sha256:

```
25bcddc6d618c7ac81d6c1e0215da6e5e9153ed26464fe60d9a2908c5538a10d
```

`tests/golden/day-2026-06-25.sha256` recorded this hash as the committed
golden value; `check` printed `GOLDEN: IDENTICAL` against it.

### Hash re-recorded 2026-08-24: taker fee curve became default-on (exo variant)

Constraint 2 (`docs/CONSTRAINTS.md`, "Fee-net always") documented an open
gap: the validated Polymarket crypto taker fee curve
(`0.07 * p * (1-p)` per share, charged on every taker fill) existed only as
an opt-in in `pm-alpha`'s research harness; `pm-backtest`'s
`taker_fee_bps` was 0.0 and no curve fee was charged at all, so a default
engine run understated real trading costs. Closing that gap is an
INTENTIONAL behavior change to the walk-forward engine's fill accounting
(`crates/pm-backtest/src/fills.rs`, `crates/pm-backtest/src/config.rs`),
so the golden hash changed and was re-recorded.

`WalkForwardConfig`/`RunnerConfig` gained `taker_fee_curve_rate: f64`
(default `0.07`, serialized into the config fingerprint). Every taker fill
now charges `curve_fee(rate, fill_price, shares)` in addition to the
existing `taker_fee_bps` (unchanged at 0.0). Maker fills are unaffected
(rebate only). `walk-forward` exposes `--fee-curve-rate` (default `0.07`);
a rate below the canonical `0.07` requires `--fantasy` and watermarks the
run (`crates/pm-backtest/src/validate.rs::validate_fee_rate`). The golden
harness does not pass `--fee-curve-rate`, so it now runs at the new
default of `0.07` rather than the previous implicit `0.0`.

Audit trail (both hashes are `tests/golden/day-2026-06-25.normalized.json`
sha256 values for the identical 2026-06-25 replay, same manifest and CLI
flags):

| | hash | overall `total_pnl` |
|---|---|---|
| old (no taker fee curve, `taker_fee_curve_rate` implicitly 0.0) | `25bcddc6d618c7ac81d6c1e0215da6e5e9153ed26464fe60d9a2908c5538a10d` | -22.4948 |
| new (taker fee curve default-on, rate 0.07) | `41313410e8a700ca40dd467b0fb62d767b202fda536f2af66b084b9d94f1f83a` | -25.2898 |

The old-hash P&L above was reproduced for this audit by rerunning the
golden replay with `--fee-curve-rate 0.0 --fantasy` (bypassing the new
sub-canonical-rate floor) against the same manifest and config; it matches
what the pre-change engine produced. The fee drag over the day's 5 taker
fills is -$2.80 (about $0.56/fill on notionals sized against the $50 max
clip), consistent with `0.07 * p * (1-p) * shares` at realistic fill
prices and share counts: a real but proportionate cost, not a wipeout.

`tests/golden/day-2026-06-25.sha256` now records the new hash;
`check` prints `GOLDEN: IDENTICAL` against it.
