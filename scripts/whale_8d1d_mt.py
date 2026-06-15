#!/usr/bin/env python3
"""Maker/taker split (fee-detectable fills only) by month and by price bucket."""
import json
from collections import defaultdict
from datetime import datetime, timezone

DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d"

agg = defaultdict(lambda: defaultdict(float))
bypx = defaultdict(lambda: defaultdict(float))
for l in open(f"{DIR}/activity.jsonl"):
    r = json.loads(l)
    px, qty, usd = float(r["price"]), float(r["size"]), float(r["usdcSize"])
    raw = px * qty
    dev = (usd - raw) if r["side"] == "BUY" else (raw - usd)
    fee = 0.07 * px * (1 - px) * qty
    band = qty * 0.005 + 0.01
    if fee <= band:
        cls = "undetectable"
    elif abs(dev - fee) < 0.6 * fee:
        cls = "taker"
    elif abs(dev) < band:
        cls = "maker"
    else:
        cls = "odd"
    mon = datetime.fromtimestamp(r["timestamp"], timezone.utc).strftime("%Y-%m")
    agg[mon][cls] += usd
    b = min(int(px * 10), 9)
    bypx[b][cls] += usd

print("USD volume by month x class:")
for m in sorted(agg):
    row = agg[m]
    tot = sum(row.values())
    det = row.get("maker", 0) + row.get("taker", 0)
    mk = row.get("maker", 0) / det * 100 if det else 0
    print(f"  {m}: total {tot:11,.0f} maker {row.get('maker',0):11,.0f} "
          f"taker {row.get('taker',0):10,.0f} undet {row.get('undetectable',0):11,.0f} "
          f"odd {row.get('odd',0):9,.0f} | maker share of detectable {mk:.0f}%")
print("\nUSD volume by price bucket x class:")
for b in sorted(bypx):
    row = bypx[b]
    det = row.get("maker", 0) + row.get("taker", 0)
    mk = row.get("maker", 0) / det * 100 if det else 0
    print(f"  {b/10:.1f}-{(b+1)/10:.1f}: maker {row.get('maker',0):10,.0f} "
          f"taker {row.get('taker',0):10,.0f} undet {row.get('undetectable',0):11,.0f} "
          f"| maker share {mk:.0f}%")
