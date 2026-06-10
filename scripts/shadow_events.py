#!/usr/bin/env python3
"""Pretty-print a pm-alpha shadow JSONL event stream."""
import json
import sys

events = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
for e in events:
    t = e.get("type")
    ts = e.get("ts_utc", "")[11:19]
    if t == "would_enter":
        touch = e.get("touch_price") or (e.get("touch") or {}).get("price")
        print(f"{ts} ENTER   {e['slug'][-10:]} {e['side']:4s} p={e.get('p_exo', 0):.2f} touch={touch}")
    elif t == "quote_probe":
        print(f"{ts} PROBE   still_quoted={e['still_quoted']} remaining={e.get('remaining_size') or 0:.0f}")
    elif t == "would_exit":
        m = e.get("mark_pnl_per_share")
        print(f"{ts} EXIT    mark={'+%.2f' % m if m is not None else 'null -> resolution'}")
    elif t == "resolution":
        print(f"{ts} RESOLVE {e['slug'][-10:]} {e['side']:4s} won={e['won']} settle={e['settle_pnl_per_share']:+.2f}")
summ = [e for e in events if e.get("type") == "summary"]
if summ:
    last = summ[-1]
    keys = ("n_entries_total", "probe_still_quoted_rate", "mean_mark_pnl_per_share", "n_marked", "n_settled")
    print()
    print("latest:", {k: last.get(k) for k in keys})
