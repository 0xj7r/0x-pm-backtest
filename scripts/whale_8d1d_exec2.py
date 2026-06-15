#!/usr/bin/env python3
"""Execution evolution: monthly clip/position sizes, late-favourite share,
both-sides pair share, sell price distribution."""
import json
import re
import statistics as st
from collections import Counter, defaultdict
from datetime import datetime, timezone, timedelta

DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d"
ET = timezone(timedelta(hours=-4))
MONTHS = {m: i + 1 for i, m in enumerate(
    "january february march april may june july august september october november december".split())}
HORIZON_SEC = {"5m": 300, "15m": 900, "1h": 3600}


def series_of(slug):
    m = re.match(r"(btc|eth|xrp|sol)-updown-(5m|15m|1h)-(\d+)", slug or "")
    if m:
        return m.group(1), m.group(2), int(m.group(3))
    m = re.match(r"(bitcoin|ethereum|solana|xrp)-up-or-down-(\w+)-(\d+)-2026-(\d+)(am|pm)-et", slug or "")
    if m:
        asset = {"bitcoin": "btc", "ethereum": "eth", "solana": "sol", "xrp": "xrp"}[m.group(1)]
        hr = int(m.group(4)) % 12 + (12 if m.group(5) == "pm" else 0)
        start = int(datetime(2026, MONTHS[m.group(2)], int(m.group(3)), hr, tzinfo=ET).timestamp())
        return asset, "1h", start
    return None, None, None


recs = []
for l in open(f"{DIR}/activity.jsonl"):
    r = json.loads(l)
    a, h, ws = series_of(r.get("slug"))
    if a is None:
        continue
    recs.append((r["timestamp"], r["conditionId"], a, h, ws, r["side"],
                 r["outcome"], float(r["price"]), float(r["size"]), float(r["usdcSize"])))

# monthly: fills, buy usd, position cost, max shares per position
posq = defaultdict(lambda: [0.0, 0.0])     # (cid,out) -> [buy_sh, buy_usd]
pos_mon = {}
for ts, cid, a, h, ws, side, out, px, qty, usd in recs:
    if side == "BUY":
        k = (cid, out)
        posq[k][0] += qty
        posq[k][1] += usd
        pos_mon[k] = datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m")
bym = defaultdict(list)
shm = defaultdict(list)
for k, (q, u) in posq.items():
    if u >= 5:
        bym[pos_mon[k]].append(u)
        shm[pos_mon[k]].append(q)
print("== position cost by month (positions >=$5) ==")
for m in sorted(bym):
    v = sorted(bym[m])
    s = sorted(shm[m])
    n = len(v)
    print(f"  {m}: n={n:5d} usd med {v[n//2]:6.0f} p90 {v[9*n//10]:6.0f} "
          f"p99 {v[99*n//100]:7.0f} max {v[-1]:8,.0f} | shares med {s[n//2]:5.0f} "
          f"p99 {s[99*n//100]:6.0f} max {s[-1]:8,.0f}")

# late favourite share: BUY px>=0.85 in final 30% of window
seg = defaultdict(float)
for ts, cid, a, h, ws, side, out, px, qty, usd in recs:
    if side != "BUY":
        continue
    frac = (ts - ws) / HORIZON_SEC[h]
    late = frac >= 0.7
    if px >= 0.85:
        seg["fav_late" if late else "fav_early"] += usd
    elif 0.30 <= px <= 0.70:
        seg["mid_late" if late else "mid_early"] += usd
    elif px <= 0.10:
        seg["tail_late" if late else "tail_early"] += usd
    else:
        seg["other"] += usd
tot = sum(seg.values())
print("\n== BUY USD by price x timing segment ==")
for k, v in sorted(seg.items(), key=lambda kv: -kv[1]):
    print(f"  {k:10s} {v:12,.0f}  {100*v/tot:5.1f}%")

# both-sides markets
both = defaultdict(lambda: [0.0, 0.0])
for ts, cid, a, h, ws, side, out, px, qty, usd in recs:
    if side == "BUY" and usd >= 5:
        both[cid][0 if out == "Up" else 1] += usd
n_both = sum(1 for v in both.values() if v[0] > 0 and v[1] > 0)
print(f"\nmarkets with >=$5 buys on BOTH sides: {n_both}/{len(both)} "
      f"({100*n_both/len(both):.1f}%)")

# sell price distribution
sd = defaultdict(float)
for ts, cid, a, h, ws, side, out, px, qty, usd in recs:
    if side == "SELL":
        sd[min(int(px * 10), 9)] += usd
ts_ = sum(sd.values())
print(f"\nSELL usd total {ts_:,.0f}; distribution:")
for b in sorted(sd):
    print(f"  {b/10:.1f}-{(b+1)/10:.1f}: {100*sd[b]/ts_:5.1f}%")

# fills/day by month, markets/day
fd = defaultdict(set)
fc = Counter()
for ts, cid, *_ in recs:
    d = datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%d")
    fd[d].add(cid)
    fc[d] += 1
print("\nper-day: fills med", st.median(fc.values()),
      "markets med", st.median(len(v) for v in fd.values()))
mm = defaultdict(list)
for d, c in fc.items():
    mm[d[:7]].append(c)
for m in sorted(mm):
    print(f"  {m}: days {len(mm[m])} fills/day med {st.median(mm[m]):.0f}")
