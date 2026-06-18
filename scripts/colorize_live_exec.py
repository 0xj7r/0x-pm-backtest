#!/usr/bin/env python3
"""Colorize shadow_exec_tail.log lines for tmux LIVE pane.

Strips embedded tracing ANSI before parsing (log file is not plain text).
"""
import re
import sys

accent = sys.argv[1] if len(sys.argv) > 1 else "\033[32m"
label = sys.argv[2] if len(sys.argv) > 2 else "LIVE"
R = "\033[0m"
ENTER = "\033[1;33m"
OK = "\033[1;32m"
BAD = "\033[1;31m"
WARN = "\033[1;35m"
DIM = "\033[90m"

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
TS_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
ENTER_RE = re.compile(
    r"LIVE ENTER (UP|DOWN) (\S+) "
    r"(?:p_up=([\d.]+) p_side=([\d.]+)|p=([\d.]+)) "
    r"touch=([\d.]+) edge=([\d.]+) clip=(\d+)"
)
SLUG_RE = re.compile(r"slug=(\S+)")
REDEEM_RE = re.compile(r"redeem OK slug=(\S+)")

for line in sys.stdin:
    line = ANSI_RE.sub("", line).strip()
    if not line:
        continue
    ts_m = TS_RE.search(line)
    ts = ts_m.group(1)[11:19] if ts_m else "??:??:??"
    head = f"{accent}{ts} {label:<4}{R}"

    m = ENTER_RE.search(line)
    if m:
        side, slug = m.group(1), m.group(2)
        p_up, p_side, p_legacy = m.group(3), m.group(4), m.group(5)
        touch, edge, clip = m.group(6), m.group(7), m.group(8)
        if p_up is not None:
            p_txt = f"p_up={p_up} p_side={p_side}"
        else:
            p_txt = f"p={p_legacy}"
        print(
            f"{head} {ENTER}ENTER {side:<4}{R} {slug} {p_txt} "
            f"touch={touch} edge={edge} clip={clip}"
        )
        continue

    if "SUBMITTED" in line and "accepted=true" in line:
        sm = SLUG_RE.search(line)
        slug = sm.group(1) if sm else "?"
        print(f"{head} {OK}FILLED{R} {slug}")
        continue

    if "submit miss" in line:
        sm = SLUG_RE.search(line)
        slug = sm.group(1) if sm else "?"
        print(f"{head} {BAD}MISS{R}  {slug}")
        continue

    rm = REDEEM_RE.search(line)
    if rm:
        print(f"{head} {OK}REDEEM{R} {rm.group(1)}")
        continue

    if "SHADOW REAL-MONEY" in line:
        print(f"{head} {WARN}ARMED real money{R}")
        continue

    if "shadow WOULD_ENTER" in line or "gamma resolved" in line:
        continue

    if "shadow_jsonl tail start" in line or "shadow_exec_tail:" in line:
        print(f"{head} {DIM}{line.split('Z')[-1].strip()[:70]}{R}")