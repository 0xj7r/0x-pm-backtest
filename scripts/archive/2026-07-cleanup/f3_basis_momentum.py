"""F3: perp basis momentum study for BTC-5m fade trades.

Per-day 1s last-price series for Binance spot (agg_trades) and perp
(futures_agg_trades), basis in bps, per-trade basis trend over prior 60s and
300s. Threshold fit on W3 only; frozen validation on W1/W2.
"""
import datetime
import json
import os
import sys

import numpy as np
import pyarrow.parquet as pq

BASE = "/Users/jackreid/go/polymarket-backtest"
CACHE = f"{BASE}/data/cache/raw/binance/exchange=binance"
RUNS = f"{BASE}/data/runs/alpha/feemin"
CUTOFF = datetime.date(2026, 5, 19)

WINDOWS = {
    "W3": ("base.trades.jsonl", datetime.date(2026, 5, 7), datetime.date(2026, 5, 18)),
    "W1": ("W1_base.trades.jsonl", datetime.date(2026, 2, 12), datetime.date(2026, 3, 31)),
    "W2": ("W2_base.trades.jsonl", datetime.date(2026, 4, 1), datetime.date(2026, 4, 30)),
}


def load_day_1s(channel, date):
    """86400-array of last trade price per UTC second (NaN where no trade)."""
    d = f"{CACHE}/channel={channel}/symbol=BTCUSDT/date={date}"
    files = [f for f in os.listdir(d) if f.endswith(".parquet")]
    assert len(files) == 1, (d, files)
    t = pq.ParquetFile(f"{d}/{files[0]}").read(columns=["price", "transact_time_ms"])
    us = t.column("transact_time_ms").to_numpy()  # actually microseconds
    px = np.asarray(t.column("price").to_pylist(), dtype=float)
    day_start_us = int(
        datetime.datetime.combine(date, datetime.time(), datetime.timezone.utc).timestamp() * 1e6
    )
    sec = ((us - day_start_us) // 1_000_000).astype(np.int64)
    ok = (sec >= 0) & (sec < 86400)
    sec, px = sec[ok], px[ok]
    arr = np.full(86400, np.nan)
    arr[sec] = px  # rows time-ordered; last write per second wins
    return arr


def ffill(a):
    idx = np.where(~np.isnan(a), np.arange(len(a)), 0)
    np.maximum.accumulate(idx, out=idx)
    return a[idx]


def build_basis(dates):
    spot_parts, perp_parts = [], []
    for date in dates:
        assert date < CUTOFF, date
        spot_parts.append(load_day_1s("agg_trades", date))
        perp_parts.append(load_day_1s("futures_agg_trades", date))
    spot = ffill(np.concatenate(spot_parts))
    perp = ffill(np.concatenate(perp_parts))
    basis = (perp / spot - 1.0) * 1e4
    t0 = int(
        datetime.datetime.combine(dates[0], datetime.time(), datetime.timezone.utc).timestamp()
    )
    return t0, basis


def load_trades(fname):
    out = []
    with open(f"{RUNS}/{fname}") as fh:
        for line in fh:
            d = json.loads(line)
            if d.get("window_secs") != 300:
                continue
            out.append(d)
    return out


def main():
    results = {}
    for wname, (fname, a, b) in WINDOWS.items():
        dates = []
        cur = a
        while cur <= b:
            dates.append(cur)
            cur += datetime.timedelta(days=1)
        t0, basis = build_basis(dates)
        trades = load_trades(fname)
        rows = []
        skipped = 0
        for tr in trades:
            ts = tr["decision_ts_ns"] // 1_000_000_000
            i = ts - t0
            if i < 300 or i >= len(basis) or np.isnan(basis[i]) or np.isnan(basis[i - 300]):
                skipped += 1
                continue
            rows.append(
                {
                    "pnl": tr["pnl"],
                    "won": bool(tr["won"]),
                    "side": tr["side"],
                    "d60": float(basis[i] - basis[i - 60]),
                    "d300": float(basis[i] - basis[i - 300]),
                    "basis": float(basis[i]),
                }
            )
        results[wname] = rows
        print(f"{wname}: {len(trades)} trades(300s), {len(rows)} matched, {skipped} skipped",
              file=sys.stderr)
    with open("/tmp/f3_rows.json", "w") as fh:
        json.dump(results, fh)
    print("saved /tmp/f3_rows.json")


if __name__ == "__main__":
    main()
