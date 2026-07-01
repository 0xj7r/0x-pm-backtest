#!/usr/bin/env python3
"""Fetch authoritative winners for every conditionId the whale traded, via
clob.polymarket.com/markets/{cid} (tokens[].winner). Parallel workers, caches
incrementally to data/external/whale_8d1d/winners.jsonl; safe to re-run."""
import json
import os
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor

DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d"
OUT = f"{DIR}/winners.jsonl"

cids = []
seen_c = set()
for l in open(f"{DIR}/activity.jsonl"):
    c = json.loads(l)["conditionId"]
    if c not in seen_c:
        seen_c.add(c)
        cids.append(c)

done = set()
if os.path.exists(OUT):
    for l in open(OUT):
        try:
            done.add(json.loads(l)["cid"])
        except Exception:
            pass
todo = [c for c in cids if c not in done]
print(f"{len(cids)} markets, {len(done)} cached, {len(todo)} to fetch", flush=True)

lock = threading.Lock()
out = open(OUT, "a")
count = [0, 0]


def work(cid):
    row = {"cid": cid}
    for attempt in range(5):
        try:
            req = urllib.request.Request(
                f"https://clob.polymarket.com/markets/{cid}",
                headers={"User-Agent": "pm-research/1.0"})
            with urllib.request.urlopen(req, timeout=20) as r:
                m = json.load(r)
            w = [t.get("outcome") for t in m.get("tokens", []) if t.get("winner")]
            row["winner"] = w[0] if w else None
            row["closed"] = m.get("closed")
            break
        except Exception as e:
            if attempt == 4:
                row["error"] = str(e)[:80]
            else:
                time.sleep(1.0 + attempt)
    with lock:
        out.write(json.dumps(row) + "\n")
        count[0] += 1
        if "error" in row:
            count[1] += 1
        if count[0] % 2000 == 0:
            out.flush()
            print(f"  {count[0]}/{len(todo)} errs={count[1]}", flush=True)


with ThreadPoolExecutor(max_workers=16) as ex:
    list(ex.map(work, todo))
out.close()
print(f"done {count[0]}, errs={count[1]}", flush=True)
