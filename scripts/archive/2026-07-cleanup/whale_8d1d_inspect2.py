#!/usr/bin/env python3
import json
from collections import Counter

PATH = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d/activity.jsonl"
seen = Counter()
examples = {}
for l in open(PATH):
    r = json.loads(l)
    s = r.get("slug") or "?"
    if s.startswith(("btc-updown", "eth-updown", "xrp-updown", "sol-updown")):
        parts = s.split("-")
        fam = "-".join(parts[:3])  # e.g. btc-updown-5m
        seen[fam] += 1
        examples.setdefault(fam, s)
for f, c in seen.most_common():
    print(f"{c:7d}  {f}   e.g. {examples[f]}")
