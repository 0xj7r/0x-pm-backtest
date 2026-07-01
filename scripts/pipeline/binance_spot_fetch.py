#!/usr/bin/env python3
"""Fetch Binance daily aggTrades from data.binance.vision and write parquets
matching the telonex cache schema (see pm-telonex-loader/src/binance_trades.rs:
price/quantity as strings, transact_time_ms actually microseconds).

Usage: python3 scripts/pipeline/binance_spot_fetch.py SOLUSDT 2026-05-20 2026-06-08
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

CACHE = Path("data/cache/raw/binance/exchange=binance/channel=agg_trades")
COLS = ["agg_trade_id", "price", "quantity", "first_trade_id", "last_trade_id",
        "transact_time_ms", "is_buyer_maker", "is_best_match"]


def fetch_day(symbol: str, day: str) -> bool:
    out_dir = CACHE / f"symbol={symbol}" / f"date={day}"
    out_path = out_dir / f"{symbol}-aggTrades-{day}.parquet"
    if out_path.exists():
        print(f"{symbol} {day}: exists")
        return True
    url = f"https://data.binance.vision/data/spot/daily/aggTrades/{symbol}/{symbol}-aggTrades-{day}.zip"
    try:
        raw = urlopen(url, timeout=120).read()
    except Exception as e:
        print(f"{symbol} {day}: download failed ({e})")
        return False
    with zipfile.ZipFile(io.BytesIO(raw)) as z:
        csv_bytes = z.read(z.namelist()[0])
    table = pacsv.read_csv(
        io.BytesIO(csv_bytes),
        read_options=pacsv.ReadOptions(column_names=COLS),
        convert_options=pacsv.ConvertOptions(column_types={
            "agg_trade_id": pa.int64(),
            "price": pa.string(),
            "quantity": pa.string(),
            "first_trade_id": pa.int64(),
            "last_trade_id": pa.int64(),
            "transact_time_ms": pa.int64(),
            "is_buyer_maker": pa.bool_(),
            "is_best_match": pa.bool_(),
        }),
    )
    # Binance vision daily files use microsecond timestamps since 2025; the
    # loader expects microseconds in this field. Guard against ms-era files.
    ts0 = table.column("transact_time_ms")[0].as_py()
    if ts0 < 10**14:  # millisecond epoch => convert to microseconds
        import pyarrow.compute as pc
        table = table.set_column(
            table.schema.get_field_index("transact_time_ms"),
            "transact_time_ms",
            pc.multiply(table.column("transact_time_ms"), pa.scalar(1000, pa.int64())),
        )
    n = table.num_rows
    table = table.add_column(0, "exchange", pa.array(["binance"] * n, pa.string()))
    table = table.add_column(1, "symbol", pa.array([symbol] * n, pa.string()))
    out_dir.mkdir(parents=True, exist_ok=True)
    pq.write_table(table, out_path, compression="snappy")
    print(f"{symbol} {day}: {n} rows -> {out_path}")
    return True


def main():
    symbol, start, end = sys.argv[1], sys.argv[2], sys.argv[3]
    d = date.fromisoformat(start)
    stop = date.fromisoformat(end)
    fails = 0
    while d <= stop:
        if not fetch_day(symbol, d.isoformat()):
            fails += 1
        d += timedelta(days=1)
    print(f"done, {fails} failures")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
