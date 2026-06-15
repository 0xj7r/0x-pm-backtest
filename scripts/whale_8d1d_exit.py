#!/usr/bin/env python3
"""8d1d exit behavior: hold-to-redemption vs early sell, recycle (sell~0.99 / buy~0.01),
laddered accumulation (buy fills per position)."""
import json
import re
import statistics as st
from collections import defaultdict
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


# winners
winner = {}
for l in open(f"{DIR}/winners.jsonl"):
    r = json.loads(l)
    if r.get("winner"):
        winner[r["cid"]] = r["winner"]

# per (cid, outcome) accumulate buy/sell shares; track buy fill count, sell px
pos = defaultdict(lambda: {"bq": 0.0, "sq": 0.0, "buys": [], "sells": [], "exp": 0, "asset": None, "hor": None})
for l in open(f"{DIR}/activity.jsonl"):
    r = json.loads(l)
    a, h, ws = series_of(r.get("slug"))
    if a is None:
        continue
    k = (r["conditionId"], r["outcome"])
    p = pos[k]
    p["asset"], p["hor"], p["exp"] = a, h, ws + HORIZON_SEC[h]
    if r["side"] == "BUY":
        p["bq"] += float(r["size"])
        p["buys"].append((r["timestamp"], float(r["price"]), float(r["size"])))
    else:
        p["sq"] += float(r["size"])
        p["sells"].append((r["timestamp"], float(r["price"]), float(r["size"])))

# laddered accumulation: distribution of buy fills per position (>=$5 cost proxy: bq*avgpx)
nbuys = []
for k, p in pos.items():
    if p["bq"] >= 10:
        nbuys.append(len(p["buys"]))
nbuys.sort()
n = len(nbuys)
print(f"== buy fills per position (positions >=10 shares), n={n} ==")
print(f"  med {nbuys[n//2]}  p75 {nbuys[3*n//4]}  p90 {nbuys[9*n//10]}  p99 {nbuys[99*n//100]}  max {nbuys[-1]}")
print(f"  single-fill positions: {100*sum(1 for x in nbuys if x==1)/n:.0f}%  >=3 fills: {100*sum(1 for x in nbuys if x>=3)/n:.0f}%")

# hold-to-redemption vs early exit: of shares bought, how many sold before expiry vs held
held_sh = sold_sh = 0.0
exit_frac = []  # fraction of position sold (per winning-side & losing-side)
held_count = sold_count = 0
for k, p in pos.items():
    if p["bq"] < 10:
        continue
    sold = min(p["sq"], p["bq"])
    sold_sh += sold
    held_sh += max(0, p["bq"] - p["sq"])
    f = sold / p["bq"]
    if f >= 0.95:
        sold_count += 1
    elif f <= 0.05:
        held_count += 1
tot = held_sh + sold_sh
print(f"\n== exit behavior (shares, positions >=10sh) ==")
print(f"  shares held to expiry: {held_sh:14,.0f} ({100*held_sh/tot:.0f}%)")
print(f"  shares sold before exp: {sold_sh:14,.0f} ({100*sold_sh/tot:.0f}%)")
print(f"  positions fully held(>=95%): {held_count}  fully sold(>=95%): {sold_count}")

# sell timing: where in window do sells land (frac of horizon)
sellfrac = defaultdict(float)
for k, p in pos.items():
    for ts, px, sz in p["sells"]:
        hor = HORIZON_SEC[p["hor"]]
        ws = p["exp"] - hor
        fr = (ts - ws) / hor
        bucket = "pre" if fr < 0 else ("early" if fr < 0.5 else ("late" if fr < 0.95 else "settle"))
        sellfrac[bucket] += px * sz
ts_tot = sum(sellfrac.values())
print(f"\n== SELL proceeds by window-timing ==")
for b in ["pre", "early", "late", "settle"]:
    print(f"  {b:7s}: {sellfrac[b]:12,.0f} ({100*sellfrac[b]/ts_tot:.0f}%)")

# recycle: sell at px>=0.97 then later buy same(cid,out) at px<=0.05 (or vice versa)
recycle_n = 0
recycle_usd = 0.0
for k, p in pos.items():
    hi_sell = [t for t in p["sells"] if t[1] >= 0.97]
    lo_buy = [t for t in p["buys"] if t[1] <= 0.05]
    if hi_sell and lo_buy:
        recycle_n += 1
        recycle_usd += sum(px * sz for _, px, sz in hi_sell)
print(f"\n== recycle (positions with both a >=0.97 sell and a <=0.05 buy) ==")
print(f"  positions: {recycle_n}  hi-sell usd in them: {recycle_usd:,.0f}")

# 99c-sell volume: how much sold at >=0.97 (locking near-certain wins)
sell_hi = sum(px*sz for k,p in pos.items() for ts,px,sz in p["sells"] if px>=0.97)
sell_all = sum(px*sz for k,p in pos.items() for ts,px,sz in p["sells"])
print(f"  sells at px>=0.97: {sell_hi:,.0f} of {sell_all:,.0f} total sell ({100*sell_hi/sell_all:.0f}%)")
