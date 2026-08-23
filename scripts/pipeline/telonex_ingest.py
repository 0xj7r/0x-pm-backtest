"""Ingest historical Polymarket data directly from the Telonex API.

Pulls per (asset_id, date, channel) parquet files into the same cache layout the
backtest's `--local-cache-dir` reads:
  data/cache/raw/telonex/exchange=polymarket/channel={ch}/date={d}/asset_id={a}/{a}_{d}_{ch}.parquet

Telonex: GET /v1/downloads/{exchange}/{channel}/{date}?asset_id=... -> 302 ->
presigned parquet. Auth: Bearer $TELONEX_API_KEY (Plus tier = unlimited).
Idempotent (skips files already present). Bounded concurrency.

Usage:
  python scripts/telonex_ingest.py --manifest <both-legs.jsonl> \
      --channels book_snapshot_25,trades [--limit N] [--concurrency 6]
  python scripts/telonex_ingest.py --spot btcusdt,ethusdt --from 2026-05-01 --to 2026-06-09
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import date as Date, timedelta
from pathlib import Path

import requests

API = "https://api.telonex.io/v1/downloads"
CACHE = "data/cache/raw/telonex"


def load_key() -> str:
    key = os.environ.get("TELONEX_API_KEY")
    if not key:
        envf = Path(".env.local")
        if envf.exists():
            for line in envf.read_text().splitlines():
                if line.startswith("TELONEX_API_KEY="):
                    key = line.split("=", 1)[1].strip()
                    break
    if not key:
        sys.exit("TELONEX_API_KEY not set (env or .env.local)")
    return key


def dst_path(exchange: str, channel: str, date: str, asset_id: str) -> Path:
    return Path(CACHE) / f"exchange={exchange}" / f"channel={channel}" / \
        f"date={date}" / f"asset_id={asset_id}" / f"{asset_id}_{date}_{channel}.parquet"


def fetch_one(session: requests.Session, key: str, exchange: str, channel: str,
              date: str, asset_id: str) -> str:
    dst = dst_path(exchange, channel, date, asset_id)
    if dst.exists() and dst.stat().st_size > 0:
        return "skip"
    url = f"{API}/{exchange}/{channel}/{date}"
    headers = {"Authorization": f"Bearer {key}"}
    for attempt in range(6):
        try:
            r = session.get(url, params={"asset_id": asset_id}, headers=headers,
                            allow_redirects=True, timeout=120)
            if r.status_code == 404:
                return "404"
            if r.status_code == 429:
                time.sleep(min(2 ** attempt, 30))
                continue
            if r.status_code >= 400:
                return f"err{r.status_code}"
            content = r.content  # ChunkedEncodingError can surface here
        except requests.exceptions.RequestException:
            # Connection reset / chunked-encoding / timeout: retry with backoff.
            time.sleep(min(2 ** attempt, 30))
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        tmp = dst.with_suffix(".tmp")
        tmp.write_bytes(content)
        tmp.rename(dst)
        return "ok"
    return "err_retry"


def daterange(start: str, end: str):
    d0 = Date.fromisoformat(start)
    d1 = Date.fromisoformat(end)
    cur = d0
    while cur < d1:
        yield cur.isoformat()
        cur += timedelta(days=1)


def run(tasks, key, exchange, concurrency):
    counts: dict[str, int] = {}
    with requests.Session() as session:
        with ThreadPoolExecutor(max_workers=concurrency) as pool:
            futs = [pool.submit(fetch_one, session, key, exchange, ch, d, a)
                    for (ch, d, a) in tasks]
            done = 0
            for f in as_completed(futs):
                res = f.result()
                counts[res] = counts.get(res, 0) + 1
                done += 1
                if done % 200 == 0:
                    print(f"  {done}/{len(futs)} {counts}", file=sys.stderr, flush=True)
    return counts


def main(argv=None) -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--manifest", help="both-legs jsonl (asset_id, date per row)")
    p.add_argument("--channels", default="book_snapshot_25,trades")
    p.add_argument("--spot", help="comma symbols for binance spot (e.g. btcusdt,ethusdt)")
    p.add_argument("--crypto-prices", help="comma symbols for polymarket crypto_prices (e.g. btcusd)")
    p.add_argument("--from", dest="from_date")
    p.add_argument("--to", dest="to_date")
    p.add_argument("--limit", type=int, default=0, help="cap assets (for testing)")
    p.add_argument("--concurrency", type=int, default=6)
    args = p.parse_args(argv)
    key = load_key()

    if args.manifest:
        rows = [json.loads(l) for l in open(args.manifest)]
        pairs = sorted({(r["asset_id"], r["date"]) for r in rows})
        if args.limit:
            pairs = pairs[: args.limit]
        channels = [c.strip() for c in args.channels.split(",") if c.strip()]
        tasks = [(ch, d, a) for (a, d) in pairs for ch in channels]
        print(f"polymarket: {len(pairs)} (asset,date) x {len(channels)} channels = {len(tasks)} files")
        print(run(tasks, key, "polymarket", args.concurrency))

    if args.spot:
        if not (args.from_date and args.to_date):
            sys.exit("--spot requires --from and --to")
        channels = [c.strip() for c in args.channels.split(",") if c.strip()]
        syms = [s.strip() for s in args.spot.split(",")]
        tasks = [(ch, d, sym) for sym in syms for d in daterange(args.from_date, args.to_date)
                 for ch in channels]
        print(f"binance spot: {len(tasks)} files")
        print(run(tasks, key, "binance", args.concurrency))

    if args.crypto_prices:
        if not (args.from_date and args.to_date):
            sys.exit("--crypto-prices requires --from and --to")
        syms = [s.strip() for s in args.crypto_prices.split(",")]
        tasks = [("crypto_prices", d, sym) for sym in syms
                 for d in daterange(args.from_date, args.to_date)]
        print(f"crypto_prices: {len(tasks)} files")
        print(run(tasks, key, "polymarket", args.concurrency))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
