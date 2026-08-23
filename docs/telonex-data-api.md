# Telonex data API (direct ingestion)

Telonex (`https://api.telonex.io`) is the external provider of historical
Polymarket + Binance market data. Our S3 bucket `pm-research-data-prod/raw/telonex/...`
is only a **mirror** we populated earlier; it is stale (frozen ~2026-05-28).
For fresh data (June+, new tokens) ingest **directly from the Telonex API**.

## Auth & key

- Key lives in `.env.local` as `TELONEX_API_KEY` (gitignored; never commit/echo it).
- Auth header: `Authorization: Bearer $TELONEX_API_KEY`.
- **Download quota**: our key is **Plus = unlimited downloads** (confirmed
  2026-06-08). Bulk ingestion is unblocked; no per-pull quota gating needed.

## Coverage (as of these docs)

- Polymarket off-chain (trades, quotes, book_snapshot_5/25/full): from **2025-10-11**,
  updated daily within hours of midnight UTC. (Best coverage from 2026-01-19.)
- Polymarket onchain_fills: from market inception.
- Polymarket crypto_prices (Chainlink oracle, single-asset): from **2026-04-02**,
  symbols btcusd/ethusd/solusd/xrpusd/bnbusd/dogeusd/hypeusd. This is the *actual*
  resolution feed for crypto up/down markets.
- Binance spot (btcusdt/ethusdt/solusdt/xrpusdt; trades/quotes/book_snapshot_5/25):
  from **2026-02-06**. book_snapshot_full not available (max 25 levels).

## Download endpoint

```
GET https://api.telonex.io/v1/downloads/{exchange}/{channel}/{date}
  -> 302 redirect to a presigned S3 parquet URL (expires 15 min)
```

- `{exchange}`: `polymarket` | `binance`
- `{channel}`: `trades | quotes | book_snapshot_5 | book_snapshot_25 |
  book_snapshot_full | onchain_fills | crypto_prices`
- `{date}`: `YYYY-MM-DD`. One file per (asset, date), Apache Parquet.
- Asset selector (one of): `asset_id` | `slug`+`outcome` | `market_id`+`outcome`
  | `slug`+`outcome_id` | `market_id`+`outcome_id`. Binance + crypto_prices are
  single-asset: pass `slug`/`asset_id` = lowercase symbol (e.g. `btcusdt`,
  `btcusd`), no outcome.

```bash
curl -L "https://api.telonex.io/v1/downloads/polymarket/book_snapshot_25/2026-06-01?asset_id=<UP_OR_DOWN_TOKEN>" \
  -H "Authorization: Bearer $TELONEX_API_KEY" -o out.parquet
```

**Both legs:** pass each outcome token's `asset_id` separately (or
`slug`+`outcome=Yes`/`No`). This is how we fix the up-leg-only gap — pull
`asset_id_0` AND `asset_id_1` per market.

## Metadata datasets (no key needed, updated daily)

```
https://api.telonex.io/v1/datasets/polymarket/markets   # both-legs master, current
https://api.telonex.io/v1/datasets/polymarket/tags
```

```python
import pandas as pd
markets = pd.read_parquet("https://api.telonex.io/v1/datasets/polymarket/markets")
```

This replaces the stale `s3://pm-research-backtest-prod/artifacts/markets-full.parquet`
(dated 2026-05-24) for manifest generation, and carries data-availability per channel.

## Python SDK

```
pip install telonex[all]        # pandas + polars
```

```python
from telonex import download, get_dataframe, get_availability, get_markets_dataframe
download(api_key=os.environ["TELONEX_API_KEY"], exchange="polymarket",
         channel="book_snapshot_25", from_date="2026-06-01", to_date="2026-06-02",
         asset_id="<token>", download_dir="./datasets", concurrency=5)
# get_availability() needs NO key; returns per-channel from_date/to_date.
```

Errors: `AuthenticationError`, `NotFoundError(date)`, `RateLimitError(retry_after)`,
`EntitlementError(downloads_remaining)`, `ValidationError`.

## How this maps to our pipeline

- Parquet schema matches our existing `pm-telonex-loader` (timestamp_us, slug,
  asset_id, outcome, bid_price_0..24/ask_price_0..24). So downloaded files drop
  into the same `data/cache/raw/telonex/.../channel=.../date=.../asset_id=.../`
  layout the loader + `--local-cache-dir` already read.
- Existing `pm-app/src/discovery.rs::fetch_availability` already calls the public
  `/v1/availability/polymarket` endpoint (no auth). The downloads endpoint is the
  authenticated bulk path we add.
- **Cost/disk:** run ingestion in-region (us-east-1) where the presigned S3 lives;
  pull both legs only for the markets we backtest; prune after. Quota-permitting,
  fan out per (token, duration, date) with bounded concurrency.

## Open items

- Key tier confirmed Plus (unlimited) — bulk ingest unblocked.
- Prefer `crypto_prices` (Chainlink) over Binance spot for the resolution-aligned
  signal on crypto up/down markets — worth A/B in the model.
