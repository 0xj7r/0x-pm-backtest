#!/usr/bin/env python3
"""Pull non-TRADE activity (REDEEM, SPLIT, MERGE, etc.) for the whale wallet.
Caches to data/external/whale_8d1d/nontrade.jsonl."""
import json
import os
import time
import urllib.parse
import urllib.request

ADDR = "0x8d1d5d1c6041b13fc708b5d9f668070e1724ed4a"
BASE = "https://data-api.polymarket.com/activity"
OUT = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d/nontrade.jsonl"
START = 1775001600
TYPES = "REDEEM,SPLIT,MERGE,REWARD,CONVERSION"


def fetch_page(end_ts, offset=0):
    q = urllib.parse.urlencode({
        "user": ADDR, "limit": 500, "offset": offset,
        "start": START, "end": end_ts, "type": TYPES,
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
    raise RuntimeError("fetch failed")


def key(r):
    return (r.get("transactionHash"), r.get("asset"), r.get("type"),
            str(r.get("size")), r.get("timestamp"), str(r.get("usdcSize")))


def main():
    seen, rows = set(), []
    end_ts = int(time.time()) + 60
    pages = 0
    while True:
        page = fetch_page(end_ts)
        pages += 1
        if not isinstance(page, list) or not page:
            break
        min_ts = min(r.get("timestamp", 0) for r in page)
        for r in page:
            k = key(r)
            if k not in seen:
                seen.add(k)
                rows.append(r)
        if len(page) < 500:
            break
        if min_ts >= end_ts:
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
                if len(sub) < 500:
                    break
                off += 500
                time.sleep(0.12)
            end_ts = min_ts - 1
        else:
            end_ts = min_ts
        if end_ts < START:
            break
        time.sleep(0.12)
    rows.sort(key=lambda r: r.get("timestamp", 0))
    with open(OUT, "w") as f:
        for r in rows:
            f.write(json.dumps(r, separators=(",", ":")) + "\n")
    from collections import Counter
    print(f"done: {len(rows)} rows over {pages} pages")
    print(Counter(r.get("type") for r in rows))


if __name__ == "__main__":
    main()
