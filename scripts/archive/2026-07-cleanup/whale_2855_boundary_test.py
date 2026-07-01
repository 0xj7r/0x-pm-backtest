#!/usr/bin/env python3
"""Test data-api /activity start/end inclusivity and within-window duplicates."""
import json
import urllib.parse
import urllib.request
from collections import Counter

ADDR = "0x2855555a48ee7ec2e67272701651bfe77034ebe8"
BASE = "https://data-api.polymarket.com/activity"


def fetch(start, end, offset=0, limit=500):
    q = urllib.parse.urlencode({"user": ADDR, "limit": limit, "offset": offset,
                                "start": start, "end": end})
    req = urllib.request.Request(f"{BASE}?{q}",
                                 headers={"User-Agent": "pm-research/1.0"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


# active period: 2026-03-30 ~ ts 1774841000
T = 1774841000
rows = fetch(T, T + 600)
print(f"window [T, T+600]: {len(rows)} rows")
ts_min = min(r["timestamp"] for r in rows)
ts_max = max(r["timestamp"] for r in rows)
print(f"ts range: {ts_min - T} .. {ts_max - T} (relative)")

# pick a timestamp with rows, test single-second windows
c = Counter(r["timestamp"] for r in rows)
ts, n = c.most_common(1)[0]
print(f"\nbusiest second {ts}: {n} rows in big window")
a = fetch(ts, ts)
print(f"window [ts, ts]: {len(a)} rows")
b = fetch(ts - 1, ts)
bn = sum(1 for r in b if r["timestamp"] == ts)
print(f"window [ts-1, ts]: rows at ts = {bn}")
d = fetch(ts, ts + 1)
dn = sum(1 for r in d if r["timestamp"] == ts)
print(f"window [ts, ts+1]: rows at ts = {dn}")

# duplicate keys within one window
key = lambda r: (r.get("transactionHash"), r.get("asset"), r.get("side"),
                 r.get("size"), r.get("timestamp"), r.get("type"))
kc = Counter(key(r) for r in rows)
dups = {k: v for k, v in kc.items() if v > 1}
print(f"\nwithin-window duplicate keys: {len(dups)} (rows affected: "
      f"{sum(v for v in dups.values())})")
for k, v in list(dups.items())[:5]:
    print(f"  x{v}: side={k[2]} size={k[3]} ts={k[4]} tx={k[0][:18]}...")

# pagination overlap test: fetch with limit=10 offset=0 and offset=10, compare
p0 = fetch(T, T + 600, offset=0, limit=10)
p1 = fetch(T, T + 600, offset=10, limit=10)
s0 = [key(r) for r in p0]
s1 = [key(r) for r in p1]
print(f"\npagination: page0 last == page1 first? {s0[-1] == s1[0]}")
print(f"overlap count: {len(set(s0) & set(s1))}")
