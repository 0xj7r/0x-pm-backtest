#!/usr/bin/env python3
"""Fetch full Polymarket activity for wallet ce25 and cache to jsonl."""
import json, sys, time, urllib.request, urllib.parse, os

ADDR = "0xce25e214d5cfe4f459cf67f08df581885aae7fdc"
OUT_DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_ce25"
BASE = "https://data-api.polymarket.com/activity"
START_TS = 1777593600  # 2026-05-01 00:00:00 UTC


def fetch(params):
    url = BASE + "?" + urllib.parse.urlencode(params)
    for attempt in range(6):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "research/1.0"})
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.loads(r.read())
        except Exception as e:
            print(f"  retry {attempt}: {e}", file=sys.stderr)
            time.sleep(2 * (attempt + 1))
    raise RuntimeError("fetch failed: " + url)


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    out_path = os.path.join(OUT_DIR, "activity_raw.jsonl")
    seen = set()
    total = 0
    # paginate backwards in time using 'end' cursor (timestamp-based chunking)
    end_ts = int(time.time()) + 3600
    f = open(out_path, "w")
    stall = 0
    while end_ts > START_TS:
        params = {"user": ADDR, "limit": 500, "start": START_TS, "end": end_ts,
                  "sortBy": "TIMESTAMP", "sortDirection": "DESC"}
        rows = fetch(params)
        if not isinstance(rows, list):
            print("unexpected payload:", str(rows)[:200], file=sys.stderr)
            break
        new = 0
        min_ts = end_ts
        for row in rows:
            ts = row.get("timestamp", 0)
            key = (row.get("transactionHash"), row.get("asset"), row.get("side"),
                   row.get("size"), row.get("price"), ts, row.get("type"))
            if key in seen:
                continue
            seen.add(key)
            f.write(json.dumps(row) + "\n")
            new += 1
            if ts < min_ts:
                min_ts = ts
        total += new
        print(f"end={end_ts} got={len(rows)} new={new} total={total} min_ts={min_ts}", flush=True)
        if len(rows) == 0:
            break
        if new == 0:
            # page of all-duplicates: step the cursor back 1s to escape
            end_ts = min_ts - 1
            stall += 1
            if stall > 50:
                print("stalled; aborting", file=sys.stderr)
                break
            continue
        stall = 0
        # if a full page shares one timestamp we could lose rows; offset paging within ts
        if min_ts == end_ts and len(rows) >= 500:
            # drill with offset
            offset = 500
            while True:
                params2 = dict(params, offset=offset)
                rows2 = fetch(params2)
                if not rows2:
                    break
                add = 0
                for row in rows2:
                    key = (row.get("transactionHash"), row.get("asset"), row.get("side"),
                           row.get("size"), row.get("price"), row.get("timestamp"), row.get("type"))
                    if key in seen:
                        continue
                    seen.add(key)
                    f.write(json.dumps(row) + "\n")
                    add += 1
                total += add
                print(f"  drill offset={offset} got={len(rows2)} new={add} total={total}", flush=True)
                if len(rows2) < 500:
                    break
                offset += 500
            end_ts = end_ts - 1
        else:
            end_ts = min_ts if min_ts < end_ts else end_ts - 1
        time.sleep(0.15)
    f.close()
    print(f"DONE total={total} -> {out_path}")


if __name__ == "__main__":
    main()
