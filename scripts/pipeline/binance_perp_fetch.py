#!/usr/bin/env python3
"""Fetch Binance USD-M futures history from data.binance.vision for the
directional signal suite:

- aggTrades  (perp taker flow; same parquet schema as spot agg_trades)
- metrics    (5-minute open interest, long/short ratios, taker vol ratio)
- fundingRate (monthly files; one row per 8h funding event)

Liquidations are not published historically; the cascade proxy is perp
aggTrade bursts + OI drawdown from metrics.

Usage: python3 scripts/pipeline/binance_perp_fetch.py BTCUSDT 2026-02-12 2026-06-08
"""
import io
import sys
import zipfile
from datetime import date, timedelta
from pathlib import Path
from urllib.request import urlopen

import pyarrow as pa
import pyarrow.csv as pacsv
import pyarrow.parquet as pq

CACHE = Path("data/cache/raw/binance/exchange=binance")
BASE = "https://data.binance.vision/data/futures/um"


def _download(url):
    try:
        return urlopen(url, timeout=180).read()
    except Exception as e:
        print(f"  download failed: {url} ({e})")
        return None


def _unzip_csv(raw):
    with zipfile.ZipFile(io.BytesIO(raw)) as z:
        return z.read(z.namelist()[0])


def fetch_agg_trades(symbol, day):
    out = CACHE / "channel=futures_agg_trades" / f"symbol={symbol}" / f"date={day}" / f"{symbol}-aggTrades-{day}.parquet"
    if out.exists():
        return True
    raw = _download(f"{BASE}/daily/aggTrades/{symbol}/{symbol}-aggTrades-{day}.zip")
    if raw is None:
        return False
    cols = ["agg_trade_id", "price", "quantity", "first_trade_id", "last_trade_id",
            "transact_time_ms", "is_buyer_maker"]
    table = pacsv.read_csv(
        io.BytesIO(_unzip_csv(raw)),
        read_options=pacsv.ReadOptions(column_names=cols, skip_rows=1),
        convert_options=pacsv.ConvertOptions(column_types={
            "agg_trade_id": pa.int64(), "price": pa.string(), "quantity": pa.string(),
            "first_trade_id": pa.int64(), "last_trade_id": pa.int64(),
            "transact_time_ms": pa.int64(), "is_buyer_maker": pa.bool_(),
        }),
    )
    ts0 = table.column("transact_time_ms")[0].as_py()
    if ts0 < 10**14:
        import pyarrow.compute as pc
        table = table.set_column(
            table.schema.get_field_index("transact_time_ms"), "transact_time_ms",
            pc.multiply(table.column("transact_time_ms"), pa.scalar(1000, pa.int64())))
    n = table.num_rows
    table = table.add_column(0, "exchange", pa.array(["binance"] * n))
    table = table.add_column(1, "symbol", pa.array([symbol] * n))
    table = table.append_column("is_best_match", pa.array([True] * n, pa.bool_()))
    out.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(table, out, compression="snappy")
    print(f"  {symbol} {day} perp aggTrades: {n} rows")
    return True


def fetch_metrics(symbol, day):
    out = CACHE / "channel=futures_metrics" / f"symbol={symbol}" / f"date={day}" / f"{symbol}-metrics-{day}.parquet"
    if out.exists():
        return True
    raw = _download(f"{BASE}/daily/metrics/{symbol}/{symbol}-metrics-{day}.zip")
    if raw is None:
        return False
    table = pacsv.read_csv(io.BytesIO(_unzip_csv(raw)))
    out.parent.mkdir(parents=True, exist_ok=True)
    pq.write_table(table, out, compression="snappy")
    print(f"  {symbol} {day} metrics: {table.num_rows} rows")
    return True


def fetch_funding(symbol, months):
    ok = True
    for ym in months:
        out = CACHE / "channel=futures_funding" / f"symbol={symbol}" / f"{symbol}-fundingRate-{ym}.parquet"
        if out.exists():
            continue
        raw = _download(f"{BASE}/monthly/fundingRate/{symbol}/{symbol}-fundingRate-{ym}.zip")
        if raw is None:
            ok = False
            continue
        table = pacsv.read_csv(io.BytesIO(_unzip_csv(raw)))
        out.parent.mkdir(parents=True, exist_ok=True)
        pq.write_table(table, out, compression="snappy")
        print(f"  {symbol} {ym} funding: {table.num_rows} rows")
    return ok


def main():
    symbol, start, end = sys.argv[1], sys.argv[2], sys.argv[3]
    d = date.fromisoformat(start)
    stop = date.fromisoformat(end)
    months = sorted({f"{x.year}-{x.month:02d}" for x in (d, stop)} | {
        f"{y}-{m:02d}" for y in range(d.year, stop.year + 1)
        for m in range(1, 13)
        if date(y, m, 1) >= date(d.year, d.month, 1) and date(y, m, 1) <= stop
    })
    fails = 0
    fetch_funding(symbol, months) or (fails := fails + 1)
    while d <= stop:
        day = d.isoformat()
        fetch_agg_trades(symbol, day) or (fails := fails + 1)
        fetch_metrics(symbol, day) or (fails := fails + 1)
        d += timedelta(days=1)
    print(f"done, {fails} failures")


if __name__ == "__main__":
    main()
