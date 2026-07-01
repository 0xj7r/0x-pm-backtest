#!/usr/bin/env python3
"""LIVE executor ↔ shadow-final would_enter parity (UP and DOWN).

Checks:
  1. ORPHAN LIVE — LIVE ENTER with no matching REF would_enter
  2. MISSED REF→LIVE — REF would_enter with no matching LIVE ENTER (catches
     accidental side cutoff, submit failures, tail lag)

Usage:
  python3 scripts/ops/compare_live_ref.py \\
    --shadow /path/to/combined-shadow.jsonl \\
    --live-log /path/to/shadow_exec_tail.log \\
    --since-hours 3

Matching key: (slug, side, clip) within --window-s (default 3s) on timestamp.

Exit 0 when orphans=0 and missed=0; else exit 1.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import time
from collections import Counter
from datetime import datetime, timezone

LIVE_RE = re.compile(
    r"LIVE ENTER\s+(UP|DOWN)\s+"
    r"(?P<slug>\S+)\s+"
    r"(?:p_up=(?P<p_up>[\d.]+)\s+p_side=(?P<p_side>[\d.]+)|p=(?P<p>[\d.]+))\s+"
    r"touch=(?P<touch>[\d.]+)\s+"
    r"edge=(?P<edge>[\d.]+)\s+"
    r"clip=(?P<clip>\d+)"
)

# Log prefix: 2026-06-16T13:50:00.778950Z
LIVE_ISO_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")
TS_HMS_RE = re.compile(r"(\d{2}:\d{2}:\d{2})")


def parse_live_line(line: str, day: str) -> dict | None:
    m = LIVE_RE.search(line)
    if not m:
        return None
    epoch = 0.0
    iso_m = LIVE_ISO_RE.search(line)
    if iso_m:
        epoch = datetime.fromisoformat(iso_m.group(1).replace("Z", "+00:00")).timestamp()
    ts_hms = None
    ts_m = TS_HMS_RE.search(line)
    if ts_m:
        ts_hms = ts_m.group(1)
        if epoch == 0.0 and day:
            dt = datetime.strptime(f"{day} {ts_hms}", "%Y-%m-%d %H:%M:%S").replace(
                tzinfo=timezone.utc
            )
            epoch = dt.timestamp()
    p_up = m.group("p_up") or m.group("p")
    p_side_raw = m.group("p_side")
    side = m.group(1).lower()
    p_exo = float(p_up)
    p_side = float(p_side_raw) if p_side_raw else (p_exo if side == "up" else 1.0 - p_exo)
    return {
        "side": side,
        "slug": m.group("slug"),
        "p_exo": p_exo,
        "p_side": p_side,
        "touch": float(m.group("touch")),
        "edge": float(m.group("edge")),
        "clip": int(m.group("clip")),
        "ts_hms": ts_hms,
        "epoch": epoch,
        "raw": line.strip(),
    }


def load_shadow(path: str, since_epoch: float) -> list[dict]:
    out = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            if e.get("type") != "would_enter":
                continue
            ts = e.get("ts_utc", "")
            epoch = (
                datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp() if ts else 0.0
            )
            if since_epoch > 0.0 and epoch < since_epoch:
                continue
            out.append(
                {
                    "side": e.get("side"),
                    "slug": e.get("slug"),
                    "p_exo": float(e.get("p_exo", 0)),
                    "touch": float(e.get("touch_price", 0)),
                    "edge": float(e.get("edge", 0)),
                    "clip": int(e.get("clip", 1)),
                    "ts_utc": ts,
                    "epoch": epoch,
                }
            )
    return out


def match_pair(
    a: dict, b: dict, window_s: float, *, a_is_live: bool
) -> bool:
    if a["slug"] != b["slug"] or a["side"] != b["side"] or a["clip"] != b["clip"]:
        return False
    ae = a["epoch"]
    be = b["epoch"]
    if ae <= 0.0 or be <= 0.0:
        return False
    return abs(ae - be) <= window_s


def side_label(side: str) -> str:
    return "UP" if side == "up" else "DOWN" if side == "down" else side.upper()


def fmt_ts(epoch: float, ts_utc: str = "") -> str:
    if ts_utc:
        return ts_utc[11:19]
    if epoch > 0.0:
        return datetime.fromtimestamp(epoch, tz=timezone.utc).strftime("%H:%M:%S")
    return "?"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--shadow", required=True)
    ap.add_argument("--live-log")
    ap.add_argument("--day", default="", help="UTC date fallback for LIVE hh:mm:ss stamps")
    ap.add_argument("--window-s", type=float, default=3.0)
    ap.add_argument(
        "--since-hours",
        type=float,
        default=0.0,
        help="Only compare events in the last N hours (0 = all)",
    )
    args = ap.parse_args()

    day = args.day or datetime.now(timezone.utc).strftime("%Y-%m-%d")
    since_epoch = 0.0
    if args.since_hours > 0.0:
        since_epoch = time.time() - args.since_hours * 3600.0

    shadow = load_shadow(args.shadow, since_epoch)
    by_slug: dict[str, list[dict]] = {}
    for s in shadow:
        by_slug.setdefault(s["slug"], []).append(s)

    live_lines = sys.stdin.readlines() if not args.live_log else open(args.live_log).readlines()
    lives: list[dict] = []
    for line in live_lines:
        live = parse_live_line(line, day)
        if not live:
            continue
        if since_epoch > 0.0:
            if live["epoch"] <= 0.0:
                continue
            if live["epoch"] < since_epoch:
                continue
        lives.append(live)

    ref_sides = Counter(side_label(s["side"]) for s in shadow)
    live_sides = Counter(side_label(l["side"]) for l in lives)

    orphans: list[tuple[dict, str]] = []
    matched_live: set[int] = set()
    matched_ref: set[int] = set()

    for i, live in enumerate(lives):
        if live["epoch"] <= 0.0:
            orphans.append((live, "no_ts"))
            continue
        hit_idx = None
        for j, s in enumerate(shadow):
            if match_pair(live, s, args.window_s, a_is_live=True):
                hit_idx = j
                break
        if hit_idx is not None:
            matched_live.add(i)
            matched_ref.add(hit_idx)
            hit = shadow[hit_idx]
            dp = live["p_exo"] - hit["p_exo"]
            if abs(dp) > 0.05 or abs(live["edge"] - hit["edge"]) > 0.05:
                print(
                    f"MATCH_DRIFT slug={live['slug'][-16:]} {side_label(live['side'])} "
                    f"LIVE p={live['p_exo']:.3f} edge={live['edge']:.3f} | "
                    f"REF p={hit['p_exo']:.3f} edge={hit['edge']:.3f} Δp={dp:+.3f}"
                )
        else:
            orphans.append((live, "no_ref_enter"))
            print(
                f"ORPHAN LIVE {fmt_ts(live['epoch'], live.get('ts_hms', ''))} "
                f"{side_label(live['side'])} {live['slug'][-20:]} "
                f"p={live['p_exo']:.3f} touch={live['touch']:.2f} edge={live['edge']:.3f} "
                f"clip={live['clip']}"
            )

    # Only flag MISSED after the executor was clearly tailing (first LIVE ENTER
    # in window). Pre-tail REF signals are expected gaps, not side cutoffs.
    live_floor = min((l["epoch"] for l in lives if l["epoch"] > 0.0), default=0.0)

    missed: list[dict] = []
    for j, s in enumerate(shadow):
        if j in matched_ref:
            continue
        if live_floor > 0.0 and s["epoch"] < live_floor - args.window_s:
            continue
        missed.append(s)
        print(
            f"MISSED REF→LIVE {fmt_ts(s['epoch'], s.get('ts_utc', ''))} "
            f"{side_label(s['side'])} {s['slug'][-20:]} "
            f"p={s['p_exo']:.3f} touch={s['touch']:.2f} edge={s['edge']:.3f} "
            f"clip={s['clip']}"
        )

    print()
    window_note = f"last {args.since_hours:g}h" if args.since_hours > 0 else "full log"
    print(f"# Parity window: {window_note}")
    print(
        f"REF:  UP={ref_sides.get('UP', 0)} DOWN={ref_sides.get('DOWN', 0)} "
        f"(total={len(shadow)})"
    )
    print(
        f"LIVE: UP={live_sides.get('UP', 0)} DOWN={live_sides.get('DOWN', 0)} "
        f"(total={len(lives)})"
    )
    print(
        f"live_entries={len(lives)} matched={len(matched_live)} "
        f"orphans={len(orphans)} missed_ref={len(missed)}"
    )

    down_missed = sum(1 for s in missed if s.get("side") == "down")
    up_missed = sum(1 for s in missed if s.get("side") == "up")
    if down_missed > 0:
        print(f"ALERT: {down_missed} DOWN REF signal(s) not executed by LIVE")
    if up_missed > 0:
        print(f"ALERT: {up_missed} UP REF signal(s) not executed by LIVE")
    if ref_sides.get("DOWN", 0) > 0 and live_sides.get("DOWN", 0) == 0:
        print("ALERT: REF signaled DOWN but LIVE took none in this window")
    elif ref_sides.get("DOWN", 0) == live_sides.get("DOWN", 0) and down_missed == 0:
        print("OK: DOWN side parity clean")

    return 1 if orphans or missed else 0


if __name__ == "__main__":
    sys.exit(main())