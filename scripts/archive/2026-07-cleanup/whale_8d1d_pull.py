#!/usr/bin/env python3
"""Pull full /activity history for wallet 0x8d1d...ed4a from April 1 2026 to now.

Paginates by sliding the `end` timestamp cursor (avoids offset caps), dedupes
on transactionHash+asset+side+size+timestamp, caches raw rows to
data/external/whale_8d1d/activity.jsonl. Re-runs are incremental-safe: existing
cache is loaded and merged.
"""
import json
import os
import time
import urllib.parse
import urllib.request

ADDR = "0x8d1d5d1c6041b13fc708b5d9f668070e1724ed4a"
BASE = "https://data-api.polymarket.com/activity"
OUT_DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d"
OUT = os.path.join(OUT_DIR, "activity.jsonl")
START = 1775001600  # 2026-04-01 00:00:00 UTC


def fetch_page(end_ts, offset=0):
    q = urllib.parse.urlencode({
        "user": ADDR, "limit": 500, "offset": offset,
        "start": START, "end": end_ts, "type": "TRADE",
        "sortBy": "TIMESTAMP", "sortDirection": "DESC",
    })
    req = urllib.request.Request(f"{BASE}?{q}",
                                 headers={"User-Agent": "pm-research/1.0"})
    for attempt in range(5):
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.load(r)
        except Exception as e:
            print(f"  retry {attempt+1}: {e}")
            time.sleep(2 * (attempt + 1))
    raise RuntimeError("fetch failed after retries")


def key(row):
    return (row.get("transactionHash"), row.get("asset"), row.get("side"),
            str(row.get("size")), row.get("timestamp"), str(row.get("price")))


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    seen = set()
    rows = []
    if os.path.exists(OUT):
        with open(OUT) as f:
            for line in f:
                r = json.loads(line)
                k = key(r)
                if k not in seen:
                    seen.add(k)
                    rows.append(r)
        print(f"loaded {len(rows)} cached rows")

    end_ts = int(time.time()) + 60
    total_new = 0
    pages = 0
    while True:
        page = fetch_page(end_ts)
        pages += 1
        if not isinstance(page, list) or not page:
            break
        ts_list = [r.get("timestamp", 0) for r in page]
        min_ts = min(ts_list)
        new = 0
        for r in page:
            k = key(r)
            if k not in seen:
                seen.add(k)
                rows.append(r)
                new += 1
        total_new += new
        if pages % 10 == 0:
            print(f"  page {pages}: cursor {end_ts} -> min_ts {min_ts}, "
                  f"total {len(rows)}")
        if len(page) < 500:
            # exhausted down to START
            break
        if min_ts >= end_ts:
            # all 500 rows share one timestamp; drill with offset
            off = 500
            while True:
                sub = fetch_page(end_ts, offset=off)
                if not sub:
                    break
                for r in sub:
                    k = key(r)
                    if k not in seen:
                        seen.add(k)
                        rows.append(r)
                        total_new += 1
                if len(sub) < 500:
                    break
                off += 500
                time.sleep(0.12)
            end_ts = min_ts - 1
        else:
            end_ts = min_ts  # inclusive overlap; dedupe handles repeats
        if end_ts < START:
            break
        time.sleep(0.12)

    rows.sort(key=lambda r: (r.get("timestamp", 0), r.get("transactionHash") or ""))
    with open(OUT, "w") as f:
        for r in rows:
            f.write(json.dumps(r, separators=(",", ":")) + "\n")
    ts = [r.get("timestamp", 0) for r in rows]
    print(f"done: {len(rows)} rows ({total_new} new) over {pages} pages")
    if rows:
        print(f"span: {time.strftime('%Y-%m-%d %H:%M', time.gmtime(min(ts)))} "
              f"to {time.strftime('%Y-%m-%d %H:%M', time.gmtime(max(ts)))} UTC")


if __name__ == "__main__":
    main()
