#!/usr/bin/env python3
"""Fetch Deribit DVOL (volatility index) history via the public API.

GET https://www.deribit.com/api/v2/public/get_volatility_index_data
    ?currency=BTC&start_timestamp=..&end_timestamp=..&resolution=60

Paginates backwards via the `continuation` field (the API caps each page)
and writes one parquet per UTC day:

  data/cache/raw/deribit/channel=dvol/date=YYYY-MM-DD/data.parquet

Columns: currency (string), timestamp_ms (int64), open/high/low/close
(float64), one row per candle at the requested resolution (default 60s).
Idempotent: a date is skipped if its date dir already holds any parquet.

NOTE (data roadmap): new data inputs are validated on TUNE/VERIFY windows
only. July 2026 is sealed; do not fetch or fit on it.

Usage:
  python3 scripts/pipeline/deribit_iv_fetch.py 2026-06-15 2026-06-16
  python3 scripts/pipeline/deribit_iv_fetch.py 2026-06-01 2026-06-30 --currency ETH
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request
from datetime import date, timedelta
from pathlib import Path

import pyarrow as pa
import pyarrow.parquet as pq

REPO = Path(__file__).resolve().parents[2]
CACHE = REPO / "data/cache/raw/deribit/channel=dvol"
API = "https://www.deribit.com/api/v2/public/get_volatility_index_data"

EPOCH = date(1970, 1, 1)


def day_bounds_ms(d: date) -> tuple[int, int]:
    start = (d - EPOCH).days * 86_400_000
    return start, start + 86_400_000 - 1


def api_page(currency: str, start_ms: int, end_ms: int, resolution: int) -> dict:
    url = (f"{API}?currency={currency}&start_timestamp={start_ms}"
           f"&end_timestamp={end_ms}&resolution={resolution}")
    last_err: Exception | None = None
    for attempt in range(5):
        try:
            with urllib.request.urlopen(url, timeout=60) as resp:
                body = json.load(resp)
            if "result" not in body:
                raise RuntimeError(f"unexpected response: {body}")
            return body["result"]
        except Exception as e:
            last_err = e
            time.sleep(1.5 * (attempt + 1))
    raise RuntimeError(f"deribit request failed after retries: {url} ({last_err})")


def fetch_day(currency: str, d: date, resolution: int) -> pa.Table:
    start_ms, end_ms = day_bounds_ms(d)
    rows: dict[int, list[float]] = {}
    cursor = end_ms
    while cursor > start_ms:
        result = api_page(currency, start_ms, cursor, resolution)
        data = result.get("data") or []
        for ts, o, h, lo, c in data:
            if start_ms <= ts <= end_ms:
                rows[int(ts)] = [float(o), float(h), float(lo), float(c)]
        cont = result.get("continuation")
        if not data or cont is None or cont >= cursor:
            break
        cursor = cont
    ts_sorted = sorted(rows)
    return pa.table({
        "currency": pa.array([currency] * len(ts_sorted), pa.string()),
        "timestamp_ms": pa.array(ts_sorted, pa.int64()),
        "open": pa.array([rows[t][0] for t in ts_sorted], pa.float64()),
        "high": pa.array([rows[t][1] for t in ts_sorted], pa.float64()),
        "low": pa.array([rows[t][2] for t in ts_sorted], pa.float64()),
        "close": pa.array([rows[t][3] for t in ts_sorted], pa.float64()),
    })


def main() -> None:
    ap = argparse.ArgumentParser(description="Fetch Deribit DVOL history to parquet")
    ap.add_argument("start_date")
    ap.add_argument("end_date")
    ap.add_argument("--currency", default="BTC", choices=("BTC", "ETH"))
    ap.add_argument("--resolution", type=int, default=60,
                    help="candle resolution in seconds (default 60)")
    args = ap.parse_args()

    start = date.fromisoformat(args.start_date)
    end = date.fromisoformat(args.end_date)
    if end < start:
        sys.exit("end_date must be >= start_date")

    fails = 0
    d = start
    while d <= end:
        day = d.isoformat()
        out_dir = CACHE / f"date={day}"
        if out_dir.is_dir() and any(out_dir.glob("*.parquet")):
            print(f"{args.currency} {day}: exists")
        else:
            try:
                table = fetch_day(args.currency, d, args.resolution)
            except RuntimeError as e:
                print(f"{args.currency} {day}: FAILED ({e})")
                fails += 1
                d += timedelta(days=1)
                continue
            if table.num_rows == 0:
                print(f"{args.currency} {day}: no data returned")
                fails += 1
            else:
                out_dir.mkdir(parents=True, exist_ok=True)
                tmp = out_dir / "data.parquet.part"
                pq.write_table(table, tmp, compression="snappy")
                tmp.rename(out_dir / "data.parquet")
                print(f"{args.currency} {day}: {table.num_rows} rows -> "
                      f"{out_dir / 'data.parquet'}")
        d += timedelta(days=1)
    print(f"done, {fails} failures")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
