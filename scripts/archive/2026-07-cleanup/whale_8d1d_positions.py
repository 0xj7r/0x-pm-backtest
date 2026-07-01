#!/usr/bin/env python3
import json
import urllib.request

ADDR = "0x8d1d5d1c6041b13fc708b5d9f668070e1724ed4a"
url = (f"https://data-api.polymarket.com/positions?user={ADDR}"
       f"&limit=50&sortBy=CURRENT&sortDirection=DESC")
req = urllib.request.Request(url, headers={"User-Agent": "pm-research/1.0"})
with urllib.request.urlopen(req, timeout=30) as r:
    ps = json.load(r)
print(f"{len(ps)} open positions")
for p in ps[:15]:
    print(f"  {p.get('slug','')[:44]:46s} {p.get('outcome',''):4s} "
          f"sz {float(p.get('size',0)):9,.0f} avg {float(p.get('avgPrice',0)):.3f} "
          f"cur {float(p.get('curPrice',0)):.3f} val {float(p.get('currentValue',0)):8,.0f} "
          f"pnl {float(p.get('cashPnl',0)):+8,.0f}")
