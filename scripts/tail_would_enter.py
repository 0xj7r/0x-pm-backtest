#!/usr/bin/env python3
"""Tail shadow-final JSONL and print would_enter rows for executor parity checks.

Usage:
  python3 scripts/tail_would_enter.py ~/data/pm-alpha/shadow/shadow-final/shadow-20260616.jsonl
  python3 scripts/tail_would_enter.py --follow path/to/shadow.jsonl

Each line is a self-contained execution intent (no belief recompute).
"""
from __future__ import annotations

import argparse
import json
import sys
import time


def fmt(e: dict) -> str:
    p_exo = float(e.get("p_exo", 0))
    side = e.get("side", "")
    p_side = e.get("p_side")
    if p_side is None:
        p_side = p_exo if side == "up" else 1.0 - p_exo
    return (
        f"{e.get('ts_utc', '')[11:19]} ENTER {side.upper():4s} "
        f"{e.get('slug', '')[-24:]} "
        f"p_up={p_exo:.3f} p_side={float(p_side):.3f} "
        f"touch={e.get('touch_price')} edge={e.get('edge', 0):.3f} "
        f"limit={e.get('marketable_limit_price', 0):.2f} "
        f"clip={e.get('clip')} token={str(e.get('token_id', ''))[:12]}…"
    )


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("path")
    ap.add_argument("-f", "--follow", action="store_true")
    ap.add_argument("--offset", type=int, default=0, help="start byte offset")
    args = ap.parse_args()

    with open(args.path, "r", encoding="utf-8") as f:
        if args.offset:
            f.seek(args.offset)
        while True:
            line = f.readline()
            if not line:
                if not args.follow:
                    break
                time.sleep(0.2)
                continue
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            if e.get("type") != "would_enter":
                continue
            print(fmt(e), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())