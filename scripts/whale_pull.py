#!/usr/bin/env python3
"""Pull a wallet's Polymarket /activity (all event types) over N days and
cache to data/runs/whales/<addr>.jsonl. Window-level resume: completed
30-min windows are recorded in <addr>.windows.json and skipped on re-run.

Usage: python3 scripts/whale_pull.py <address> <days>
"""
import json
import os
import sys
import time
import urllib.parse
import urllib.request

BASE = "https://data-api.polymarket.com/activity"
OUT_DIR = "data/runs/whales"
WIN = 1800


def fetch_window(addr, start, end):
    rows, offset = [], 0
    while offset <= 10000:
        q = urllib.parse.urlencode({
            "user": addr, "limit": 500, "offset": offset,
            "start": start, "end": end,
        })
        req = urllib.request.Request(f"{BASE}?{q}",
                                     headers={"User-Agent": "pm-research/1.0"})
        page = None
        for attempt in range(8):
            try:
                with urllib.request.urlopen(req, timeout=30) as r:
                    page = json.load(r)
                if isinstance(page, list):
                    break
                page = None
            except Exception:
                pass
            if attempt == 7:
                raise RuntimeError(f"window {start} offset {offset}: "
                                   "no valid page after retries")
            time.sleep(2.0 * (attempt + 1))
        if not page:
            break
        rows.extend(page)
        if len(page) < 500:
            break
        offset += 500
        time.sleep(0.15)
    return rows


def main():
    addr = sys.argv[1].lower()
    days = float(sys.argv[2])
    repair = len(sys.argv) > 3 and sys.argv[3] == "repair"
    os.makedirs(OUT_DIR, exist_ok=True)
    out_path = f"{OUT_DIR}/{addr}.jsonl"
    meta_path = f"{OUT_DIR}/{addr}.windows.json"
    done = set()
    if os.path.exists(meta_path):
        done = set(json.load(open(meta_path)))
    if repair and done and os.path.exists(out_path):
        # re-fetch "done" windows that hold zero rows (possible silent
        # rate-limit empties)
        counts = {}
        for line in open(out_path):
            ts = json.loads(line).get("timestamp", 0)
            w = ts // WIN * WIN
            counts[w] = counts.get(w, 0) + 1
        empty = {w for w in done if counts.get(w, 0) == 0}
        print(f"repair: re-fetching {len(empty)} empty windows")
        done -= empty

    end = int(time.time()) // WIN * WIN
    start = end - int(days * 86400)
    total = 0
    with open(out_path, "a") as out:
        cur = start
        while cur < end:
            if cur not in done:
                rows = fetch_window(addr, cur, cur + WIN)
                for r in rows:
                    out.write(json.dumps(r, separators=(",", ":")) + "\n")
                out.flush()
                total += len(rows)
                done.add(cur)
                json.dump(sorted(done), open(meta_path, "w"))
                time.sleep(0.15)
            cur += WIN
    print(f"{addr}: pulled {total} new events; windows cached: {len(done)}")


if __name__ == "__main__":
    main()
