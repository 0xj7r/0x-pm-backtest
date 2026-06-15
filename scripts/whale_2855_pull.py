#!/usr/bin/env python3
"""Pull full activity history for wallet 0x2855... from Polymarket data API.

API semantics (verified): start AND end are inclusive; pagination does not
overlap; identical (tx,asset,side,size,ts) rows are REAL distinct fills, so
no dedupe. Non-overlapping day windows [cur, cur+86399]; on HTTP 400 or
offset>=10000 the window splits recursively. Per-day shards in
data/external/whale_2855/raw/, concatenated to activity.jsonl at the end.
"""
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

ADDR = "0x2855555a48ee7ec2e67272701651bfe77034ebe8"
BASE = "https://data-api.polymarket.com/activity"
DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_2855"
RAW = os.path.join(DIR, "raw")
OUT = os.path.join(DIR, "activity.jsonl")
START = 1772323200  # 2026-03-01 00:00 UTC
WINDOW = 86400


def get_page(start, end, offset):
    q = urllib.parse.urlencode({
        "user": ADDR, "limit": 500, "offset": offset,
        "start": start, "end": end,
    })
    for attempt in range(6):
        req = urllib.request.Request(f"{BASE}?{q}",
                                     headers={"User-Agent": "pm-research/1.0"})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.load(r)
        except urllib.error.HTTPError as e:
            if e.code == 400:
                raise
            time.sleep(1.5 * (attempt + 1))
        except Exception:
            time.sleep(1.5 * (attempt + 1))
    raise RuntimeError(f"failed window {start}-{end} offset {offset}")


def fetch_window(start, end):
    """Inclusive [start, end]."""
    rows, offset = [], 0
    while True:
        try:
            page = get_page(start, end, offset)
        except urllib.error.HTTPError:
            if end - start < 2:
                raise
            mid = (start + end) // 2
            return fetch_window(start, mid) + fetch_window(mid + 1, end)
        if not isinstance(page, list) or not page:
            break
        rows.extend(page)
        if len(page) < 500:
            break
        offset += 500
        if offset >= 10000:
            mid = (start + end) // 2
            return fetch_window(start, mid) + fetch_window(mid + 1, end)
        time.sleep(0.05)
    return rows


def pull_day(cur, end_all):
    day = time.strftime("%Y-%m-%d", time.gmtime(cur))
    shard = os.path.join(RAW, f"{day}.jsonl")
    if os.path.exists(shard):
        return day, -1
    w_end = min(cur + WINDOW - 1, end_all)
    rows = fetch_window(cur, w_end)
    tmp = shard + ".tmp"
    with open(tmp, "w") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")
    os.rename(tmp, shard)
    return day, len(rows)


def main():
    os.makedirs(RAW, exist_ok=True)
    end_all = int(time.time())
    days = list(range(START, end_all, WINDOW))
    with ThreadPoolExecutor(max_workers=4) as ex:
        for day, n in ex.map(lambda c: pull_day(c, end_all), days):
            print(f"{day}: {'cached' if n < 0 else n}", flush=True)
    n_total = 0
    with open(OUT, "w") as out:
        for cur in days:
            day = time.strftime("%Y-%m-%d", time.gmtime(cur))
            shard = os.path.join(RAW, f"{day}.jsonl")
            with open(shard) as f:
                for line in f:
                    out.write(line)
                    n_total += 1
    print(f"DONE: {n_total} rows -> {OUT}")


if __name__ == "__main__":
    main()
