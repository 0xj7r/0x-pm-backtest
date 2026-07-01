#!/usr/bin/env python3
import json
from collections import Counter

PATH = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d/activity.jsonl"
rows = [json.loads(l) for l in open(PATH)]
print("n =", len(rows))
print("keys:", sorted(rows[0].keys()))
print(json.dumps(rows[-1], indent=1))

slugs = Counter()
for r in rows:
    s = r.get("slug") or "?"
    # strip trailing date/time tokens to family-ize
    slugs[s] += 1
print("\ntop 15 raw slugs:")
for s, c in slugs.most_common(15):
    print(f"  {c:6d}  {s}")

# family heuristic: drop tokens that look like dates/times/numbers
fams = Counter()
for r in rows:
    s = r.get("slug") or "?"
    toks = [t for t in s.split("-") if not any(ch.isdigit() for ch in t)
            and t not in ("am", "pm", "et", "edt", "est")]
    fams["-".join(toks)] += 1
print("\ntop 25 families:")
for s, c in fams.most_common(25):
    print(f"  {c:6d}  {s}")
