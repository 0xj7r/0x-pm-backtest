# SOL / XRP up-down book-tape validation (W3, decontaminated)

Date: 2026-06-13
Goal: test whether the fade-candidate AND the lane strategies work on SOL and
XRP up-down markets, run SPOT-ONLY so the BTC-perp contamination bug cannot
poison the (tiny) non-BTC signal.

## Verdict (per asset, per strategy)

| asset | strategy | verdict | basis |
|-------|----------|---------|-------|
| SOL-5m | fade | RUN-PENDING (data acquired) | books backfilled, engine run blocked in this env |
| SOL-5m | lane | RUN-PENDING (data acquired) | books backfilled, engine run blocked in this env |
| XRP-5m | fade | RUN-PENDING (data acquired) | books backfilled, engine run blocked in this env |
| XRP-5m | lane | RUN-PENDING (data acquired) | books backfilled, engine run blocked in this env |

The data acquisition (the part that was actually missing) is DONE and verified.
The backtest binary `target/fast/pm-app` is blocked from execution in this
sandbox (every invocation, including `--version`, is denied by a permission
hook), so the P&L numbers could not be produced here. The run is fully wired and
one command away; see "How to finish" below.

## How tapes load for a non-BTC asset (the resolved question)

- Books are keyed purely by `(date, asset_id)`, NOT by slug or outcome:
  `raw/telonex/exchange=polymarket/channel=book_snapshot_25/date={date}/asset_id={asset_id}/{asset_id}_{date}_book_snapshot_25.parquet`
  (`crates/pm-telonex-loader/src/s3.rs::build_object_path` / `resolve_asset_day`).
- The harness resolves the tape in
  `crates/pm-app/src/walkforward.rs::load_replay_events_for_market`, reading the
  per-market `asset_id` straight from the manifest. Each market loads exactly one
  book tape.
- The fade run, when `--down-assets` is set, ALSO loads the complementary token's
  book (matched by slug from `down_all.jsonl`) for the real NO ladder
  (`crates/pm-app/src/alpha.rs` ~L448-471). A missing complement is tolerated
  (`unwrap_or_default()` -> empty down_events). The lane run loads only the
  primary book.
- Spot for the model comes from Binance agg_trades, symbol resolved by slug in
  `crates/pm-app/src/discovery.rs::spot_symbol_for_market`:
  sol -> SOLUSDT, xrp -> XRPUSDT. With `--perp-price-weight 0` and no
  `--perp-symbol`, NO perp tape is loaded, so BTC cannot contaminate.
- A tick cache (`data/cache/ticks/{date}/...`, keyed on `has_down`) short-circuits
  raw-book loading on subsequent runs (`alpha.rs` ~L435-447).

## Cache state found

- SOL/XRP W3 book tapes were NOT cached (0 of 3456 each). The existing W3 telonex
  book cache held only BTC-5m (288 asset dirs/day == one BTC-5m day).
- SOLUSDT and XRPUSDT Binance spot (agg_trades) ARE fully cached for all of W3
  (2026-05-06..05-18). So only book_snapshot_25 was missing.

## What was backfilled

Source: `s3://pm-research-data-prod/raw/telonex/.../channel=book_snapshot_25/`
via AWS profile `visumlabs` (the `default` creds were expired/invalid).

Scope (disk-constrained: only ~21-24GB free vs a 20GB guard; full 12-day W3 for
both 5m assets would have been ~9.4GB and breached the guard):
- 6-day W3 core: 2026-05-09 .. 2026-05-14
- SOL-5m and XRP-5m, primary (resolved-winner) + complement (down_all) tokens.

Result (downloaded into `data/cache/raw/telonex/.../book_snapshot_25/`):

| asset | markets (6d) | primary books | complement books |
|-------|-------------|---------------|------------------|
| SOL-5m | 1728 | 1208 / 1728 (70%) | 1173 / 1728 |
| XRP-5m | 1728 | 1169 / 1728 (68%) | 1166 / 1728 |

~2.6GB total downloaded (well under the 8GB cap). The ~30% of markets with no
primary book are a GENUINE archive gap: those asset_ids return an empty S3
listing (illiquid tokens that never produced a book_snapshot_25). Confirmed by
direct `s3 ls` on several missing keys. 70% coverage over 6 days (~1200 markets
each) is enough for a directional verdict once the engine runs.

15m / Feb-Mar / Apr samples were NOT fetched: the 5m W3 core already consumed the
safe disk budget, and the task ordered "5m W3 first."

## How to finish (one command, then score)

The run script is committed and decontaminated (no `--perp-symbol`,
`--perp-price-weight 0`, correct 6-day window, no touch of 2026-05-19..06-30):

    bash scripts/solxrp_books_w3.sh

It writes 4 runs to `data/runs/alpha/solxrp/`:
`sol5_fade`, `xrp5_fade`, `sol5_lane`, `xrp5_lane` (each `.json` + `.trades.jsonl`).
First pass converts books -> ticks (slower); it has a >=20GB disk guard.

Scoring (from the existing offline scorer / trades.jsonl), per the task:
- window_secs 300 (5m).
- lane: apply offline `sigma_bar_bps >= 4` floor.
- fade: apply offline `sigma_bar_bps >= 3` floor.
- report fee-net NET, hit, Sharpe, entries/day, $/day.

Benchmarks to compare against:
- BTC-5m fade-candidate: $1,634/day (W3).
- BTC lane: ~$126/day (W3, sigma>=4).

Verdict rubric: VIABLE-BOOK / MARGINAL / DEAD / DATA-MISSING.

## Contamination check (honoured)

No `--perp-symbol` is passed for SOL/XRP; `--perp-price-weight 0` is set on every
run, so no perp tape (BTC or otherwise) is loaded. Spot is SOLUSDT/XRPUSDT only.
