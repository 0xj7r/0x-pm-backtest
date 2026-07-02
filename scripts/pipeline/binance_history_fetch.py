#!/usr/bin/env python3
"""Bulk-download Binance deep history from data.binance.vision (no API key).

Prefers monthly zips when the requested range covers a complete past month;
falls back to daily zips for partial months and the current month. Verifies
the published .CHECKSUM (sha256) when present, then converts to the repo
parquet cache layout:

  data/cache/raw/binance/exchange=binance/channel={ch}/symbol={SYM}/date={d}/data.parquet

Channels: spot aggTrades -> agg_trades, um aggTrades -> futures_agg_trades,
spot klines-1m -> klines_1m, um klines-1m -> futures_klines_1m. aggTrades
parquets match the existing loader schema exactly (price/quantity as strings,
transact_time_ms actually microseconds; see pm-telonex-loader binance_trades.rs).
Idempotent: a date is skipped if its date dir already holds any parquet.

NOTE (data roadmap): new data inputs are validated on TUNE/VERIFY windows
only. July 2026 is sealed; do not fetch or fit on it.

Usage:
  python3 scripts/pipeline/binance_history_fetch.py --market spot --symbol BTCUSDT \
      --channel aggTrades 2026-06-15 2026-06-16
  python3 scripts/pipeline/binance_history_fetch.py --market um --symbol BTCUSDT \
      --channel klines-1m 2026-06-01 2026-06-30 --dry-run
"""
from __future__ import annotations

import argparse
import calendar
import hashlib
import io
import subprocess
import sys
import tempfile
import zipfile
from datetime import date, timedelta
from pathlib import Path

import pyarrow as pa
import pyarrow.compute as pc
import pyarrow.csv as pacsv
import pyarrow.parquet as pq

REPO = Path(__file__).resolve().parents[2]
CACHE = REPO / "data/cache/raw/binance/exchange=binance"
BASE = "https://data.binance.vision/data"

CHANNEL_DIRS = {
    ("spot", "aggTrades"): "agg_trades",
    ("um", "aggTrades"): "futures_agg_trades",
    ("spot", "klines-1m"): "klines_1m",
    ("um", "klines-1m"): "futures_klines_1m",
}

AGG_COLS_SPOT = ["agg_trade_id", "price", "quantity", "first_trade_id",
                 "last_trade_id", "transact_time_ms", "is_buyer_maker",
                 "is_best_match"]
AGG_COLS_UM = AGG_COLS_SPOT[:-1]
KLINE_COLS = ["open_time", "open", "high", "low", "close", "volume",
              "close_time", "quote_volume", "count", "taker_buy_volume",
              "taker_buy_quote_volume", "ignore"]

DAY_US = 86_400_000_000


def market_base(market: str) -> str:
    return f"{BASE}/spot" if market == "spot" else f"{BASE}/futures/um"


def zip_url(market: str, symbol: str, channel: str, period: str, stamp: str) -> str:
    base = market_base(market)
    if channel == "aggTrades":
        return f"{base}/{period}/aggTrades/{symbol}/{symbol}-aggTrades-{stamp}.zip"
    return f"{base}/{period}/klines/{symbol}/1m/{symbol}-1m-{stamp}.zip"


def curl_fetch(url: str, out: Path) -> bool:
    r = subprocess.run(
        ["curl", "-sS", "-f", "--retry", "3", "--connect-timeout", "20",
         "-o", str(out), url],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        print(f"  fetch failed: {url} ({r.stderr.strip() or f'curl exit {r.returncode}'})")
        return False
    return True


def verify_checksum(zip_path: Path, url: str, tmp: Path) -> bool:
    """Verify .CHECKSUM if published; warn and continue when absent."""
    cs_path = tmp / (zip_path.name + ".CHECKSUM")
    r = subprocess.run(
        ["curl", "-sS", "-f", "--connect-timeout", "20",
         "-o", str(cs_path), url + ".CHECKSUM"],
        capture_output=True, text=True,
    )
    if r.returncode != 0:
        print(f"  checksum unavailable for {zip_path.name}, skipping verification")
        return True
    expected = cs_path.read_text().split()[0].strip().lower()
    actual = hashlib.sha256(zip_path.read_bytes()).hexdigest()
    if actual != expected:
        print(f"  CHECKSUM MISMATCH {zip_path.name}: got {actual}, want {expected}")
        return False
    return True


def read_zip_csv(zip_path: Path, market: str, channel: str) -> pa.Table:
    with zipfile.ZipFile(zip_path) as z:
        csv_bytes = z.read(z.namelist()[0])
    has_header = not csv_bytes.split(b",", 1)[0].strip().isdigit()
    if channel == "aggTrades":
        cols = AGG_COLS_SPOT if market == "spot" else AGG_COLS_UM
        types = {
            "agg_trade_id": pa.int64(), "price": pa.string(),
            "quantity": pa.string(), "first_trade_id": pa.int64(),
            "last_trade_id": pa.int64(), "transact_time_ms": pa.int64(),
            "is_buyer_maker": pa.bool_(),
        }
        if market == "spot":
            types["is_best_match"] = pa.bool_()
    else:
        cols = KLINE_COLS
        types = {
            "open_time": pa.int64(), "open": pa.string(), "high": pa.string(),
            "low": pa.string(), "close": pa.string(), "volume": pa.string(),
            "close_time": pa.int64(), "quote_volume": pa.string(),
            "count": pa.int64(), "taker_buy_volume": pa.string(),
            "taker_buy_quote_volume": pa.string(), "ignore": pa.string(),
        }
    return pacsv.read_csv(
        io.BytesIO(csv_bytes),
        read_options=pacsv.ReadOptions(column_names=cols,
                                       skip_rows=1 if has_header else 0),
        convert_options=pacsv.ConvertOptions(column_types=types),
    )


def normalize(table: pa.Table, market: str, channel: str, symbol: str) -> pa.Table:
    """Microsecond timestamps + exchange/symbol prefix, matching the cache schema."""
    ts_cols = ["transact_time_ms"] if channel == "aggTrades" else ["open_time", "close_time"]
    for name in ts_cols:
        idx = table.schema.get_field_index(name)
        if table.column(name)[0].as_py() < 10**14:  # ms epoch -> microseconds
            table = table.set_column(
                idx, name,
                pc.multiply(table.column(name), pa.scalar(1000, pa.int64())))
    if channel == "klines-1m":
        table = table.drop_columns(["ignore"])
    n = table.num_rows
    table = table.add_column(0, "exchange", pa.array(["binance"] * n, pa.string()))
    table = table.add_column(1, "symbol", pa.array([symbol] * n, pa.string()))
    if channel == "aggTrades" and market == "um":
        table = table.append_column("is_best_match", pa.array([True] * n, pa.bool_()))
    return table


def date_dir(channel_dir: str, symbol: str, day: str) -> Path:
    return CACHE / f"channel={channel_dir}" / f"symbol={symbol}" / f"date={day}"


def day_exists(channel_dir: str, symbol: str, day: str) -> bool:
    d = date_dir(channel_dir, symbol, day)
    return d.is_dir() and any(d.glob("*.parquet"))


def write_day(table: pa.Table, channel_dir: str, symbol: str, day: str) -> None:
    out_dir = date_dir(channel_dir, symbol, day)
    out_dir.mkdir(parents=True, exist_ok=True)
    tmp = out_dir / "data.parquet.part"
    pq.write_table(table, tmp, compression="snappy")
    tmp.rename(out_dir / "data.parquet")
    print(f"  {symbol} {day}: {table.num_rows} rows -> {out_dir / 'data.parquet'}")


def split_days(table: pa.Table, ts_col: str) -> dict[str, pa.Table]:
    day_idx = pc.cast(pc.floor(pc.divide(table.column(ts_col), DAY_US)), pa.int64())
    out = {}
    for epoch_day in pc.unique(day_idx).to_pylist():
        day = (date(1970, 1, 1) + timedelta(days=epoch_day)).isoformat()
        out[day] = table.filter(pc.equal(day_idx, epoch_day))
    return out


def month_days(year: int, month: int) -> list[date]:
    return [date(year, month, d + 1)
            for d in range(calendar.monthrange(year, month)[1])]


def plan(start: date, end: date, today: date):
    """Yield ('monthly', ym, [dates]) or ('daily', day_iso, [date])."""
    d = start
    while d <= end:
        mdays = month_days(d.year, d.month)
        month_complete = (d.year, d.month) < (today.year, today.month)
        if month_complete and mdays[0] >= start and mdays[-1] <= end:
            yield ("monthly", f"{d.year}-{d.month:02d}", mdays)
            d = mdays[-1] + timedelta(days=1)
        else:
            yield ("daily", d.isoformat(), [d])
            d += timedelta(days=1)


def fetch_zip_to_table(url: str, market: str, channel: str, symbol: str) -> pa.Table | None:
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        zp = tmp / url.rsplit("/", 1)[1]
        if not curl_fetch(url, zp):
            return None
        if not verify_checksum(zp, url, tmp):
            return None
        return normalize(read_zip_csv(zp, market, channel), market, channel, symbol)


def main() -> None:
    ap = argparse.ArgumentParser(
        description="Bulk-fetch Binance history from data.binance.vision")
    ap.add_argument("start_date")
    ap.add_argument("end_date")
    ap.add_argument("--market", choices=("spot", "um"), default="spot")
    ap.add_argument("--symbol", default="BTCUSDT")
    ap.add_argument("--channel", choices=("aggTrades", "klines-1m"),
                    default="aggTrades")
    ap.add_argument("--dry-run", action="store_true",
                    help="list planned fetches without downloading")
    args = ap.parse_args()

    start = date.fromisoformat(args.start_date)
    end = date.fromisoformat(args.end_date)
    if end < start:
        sys.exit("end_date must be >= start_date")

    channel_dir = CHANNEL_DIRS[(args.market, args.channel)]
    ts_col = "transact_time_ms" if args.channel == "aggTrades" else "open_time"
    fails = 0
    for kind, stamp, dates in plan(start, end, date.today()):
        missing = [d for d in dates if not day_exists(channel_dir, args.symbol, d.isoformat())]
        if not missing:
            print(f"{args.symbol} {stamp}: all {len(dates)} date(s) exist, skip")
            continue
        url = zip_url(args.market, args.symbol, args.channel, kind, stamp)
        if args.dry_run:
            print(f"PLAN {kind} {url} -> {len(missing)} missing date(s) "
                  f"under channel={channel_dir}")
            continue
        missing_set = {d.isoformat() for d in missing}
        table = fetch_zip_to_table(url, args.market, args.channel, args.symbol)
        if table is not None:
            for day, day_table in sorted(split_days(table, ts_col).items()):
                if day in missing_set:
                    write_day(day_table, channel_dir, args.symbol, day)
            continue
        if kind == "monthly":
            # Monthly dumps are published with a lag; fall back to daily zips.
            print(f"  monthly {stamp} unavailable, falling back to daily zips")
            for day in sorted(missing_set):
                durl = zip_url(args.market, args.symbol, args.channel, "daily", day)
                dtable = fetch_zip_to_table(durl, args.market, args.channel, args.symbol)
                if dtable is None:
                    fails += 1
                    continue
                for dday, day_table in split_days(dtable, ts_col).items():
                    if dday == day:
                        write_day(day_table, channel_dir, args.symbol, dday)
        else:
            fails += 1
    print(f"done, {fails} failures")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
