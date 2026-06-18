#!/usr/bin/env python3
"""Colorize shadow-final JSONL for tmux REF pane."""
import json
import sys

accent = sys.argv[1] if len(sys.argv) > 1 else "\033[37m"
label = sys.argv[2] if len(sys.argv) > 2 else ""
R = "\033[0m"
ENTER = "\033[1;33m"
WON = "\033[1;32m"
LOST = "\033[1;31m"
DIM = "\033[90m"

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        o = json.loads(line)
    except json.JSONDecodeError:
        continue
    t = o.get("type", "")
    ts = o.get("ts_utc", "")[11:19]
    slug = str(o.get("slug", ""))
    head = f"{accent}{ts} {label:<4}{R}"
    if t == "would_enter":
        print(
            f"{head} {ENTER}ENTER {str(o.get('side', '')).upper():<4}{R} {slug} "
            f"p={o.get('p_exo', 0):.3f} touch={o.get('touch_price')} "
            f"edge={o.get('edge', 0):.3f} clip={o.get('clip')}"
        )
    elif t == "resolution":
        won = o.get("won")
        c = WON if won else LOST
        pnl = o.get("ladder_settle_pnl_usd")
        print(
            f"{head} {c}{'WON ' if won else 'LOST'} {str(o.get('side', '')).upper():<4}{R} "
            f"{slug} pnl={pnl}"
        )
    elif t == "summary":
        print(
            f"{head} {DIM}summary active={o.get('n_active_markets')} "
            f"entries={o.get('n_entries_total')}{R}"
        )
sys.stdout.flush()