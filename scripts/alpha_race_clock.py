#!/usr/bin/env python3
"""Race-clock study: for each backtest entry, did a REAL taker print consume
the quote we targeted, and how long after our decision time? Uses the local
Polymarket trades channel. Output: the empirical competition clock —
fraction of our entries where the targeted liquidity actually traded, and
the latency distribution of those competitor takes.

Usage: python3 scripts/alpha_race_clock.py <trades_dump.jsonl> <canonical_up_manifest.jsonl> [max_markets]
"""
import glob
import json
import sys
from collections import defaultdict

import pyarrow.parquet as pq

TRADES_ROOT = "data/cache/raw/telonex/exchange=polymarket/channel=trades"


def load_trades(asset_id, date):
    files = glob.glob(f"{TRADES_ROOT}/date={date}/asset_id={asset_id}/*.parquet")
    if not files:
        return []
    out = []
    for f in files:
        t = pq.ParquetFile(f).read()
        names = t.schema.names
        ts_col = "timestamp_us"
        px_col = "price"
        sz_col = "size" if "size" in names else ("quantity" if "quantity" in names else None)
        side_col = "side" if "side" in names else None
        for r in t.to_pylist():
            try:
                out.append((int(r[ts_col]) * 1000, float(r[px_col]),
                            float(r[sz_col]) if sz_col else 0.0,
                            (r.get(side_col) or "").lower() if side_col else ""))
            except (TypeError, ValueError):
                continue
    out.sort()
    return out


def main():
    dump_path, manifest_path = sys.argv[1], sys.argv[2]
    max_markets = int(sys.argv[3]) if len(sys.argv) > 3 else 400

    by_slug = {}
    for line in open(manifest_path):
        r = json.loads(line)
        by_slug[r["slug"]] = r

    import datetime
    entries = []
    for line in open(dump_path):
        r = json.loads(line)
        open_ts = r["open_ts_ns"] // 1_000_000_000
        slug = f"btc-updown-5m-{open_ts}"
        m = by_slug.get(slug)
        if not m:
            continue
        date = datetime.datetime.fromtimestamp(open_ts, datetime.timezone.utc).date().isoformat()
        entries.append((m["asset_id"], date, r))
    entries = entries[:max_markets]
    print(f"{len(entries)} entries to race-check")

    taken = 0
    delays = []
    no_trades_data = 0
    cache = {}
    for asset_id, date, r in entries:
        key = (asset_id, date)
        if key not in cache:
            cache[key] = load_trades(asset_id, date)
        trades = cache[key]
        if not trades:
            no_trades_data += 1
            continue
        t0 = r["decision_ts_ns"]
        side = r["side"]
        limit = r["avg_price"]
        # Our entry bought `side` at ~avg_price. A competitor "takes the same
        # quote" if a real print occurs at a price at-or-better than ours
        # (for the Up token: a buy print at <= our price when we bought YES;
        # the trades file is per-asset = the Up token).
        horizon_ns = 60_000_000_000
        for ts, px, sz, tside in trades:
            if ts < t0:
                continue
            if ts > t0 + horizon_ns:
                break
            hit = (side == "Yes" and px <= limit + 0.005) or (side == "No" and px >= 1.0 - limit - 0.005)
            if hit:
                taken += 1
                delays.append((ts - t0) / 1e6)
                break

    n = len(entries) - no_trades_data
    print(f"with trades data: {n} | quote actually consumed within 60s: {taken} ({100*taken/max(n,1):.1f}%)")
    if delays:
        delays.sort()
        q = lambda p: delays[int(p * (len(delays) - 1))]
        print(f"competitor take delay ms: p10={q(.1):.0f} p50={q(.5):.0f} p90={q(.9):.0f}")
        print(f"takes slower than our 150ms: {100*sum(1 for d in delays if d > 150)/len(delays):.1f}%")
        print(f"takes slower than 1000ms:    {100*sum(1 for d in delays if d > 1000)/len(delays):.1f}%")


if __name__ == "__main__":
    main()
