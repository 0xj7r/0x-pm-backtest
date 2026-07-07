#!/usr/bin/env python3
"""Per-day live P&L split by belief dwell and entry timing, per stream.

The decision-quality evidence stream: the June replay can only price a
gate's COST (it has no phantom entries); this report measures the live
BENEFIT side, split by the two decision-time quality signals. Judged
against docs/latency-truth and the dwell sweep to decide the gate.

Usage:
  python3 scripts/ops/dwell_split_report.py --date 2026-07-07 \
    --streams final=data/runs/shadow_final_sync fast=data/runs/shadow_fast_sync
"""
from __future__ import annotations

import argparse
import glob
import json
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

CLIP = 50.0
DWELL_BUCKETS = ((0, 15), (15, 30), (30, 1e9))
SECS_BUCKETS = ((0, 15), (15, 60), (60, 1e9))


def first_clip(ev: dict) -> bool:
    # Log-only streams count clips 1-based at emit; deferred-commit streams
    # (fast_live) emit the pre-commit value 0. Both mean "first entry".
    return int(ev.get("clip", 1)) <= 1


def day_rows(shadow_dir: Path, day: str) -> list[dict]:
    entries: dict[tuple, list] = defaultdict(list)
    resols: dict[tuple, list] = defaultdict(list)
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for line in open(fp, errors="replace"):
            if f'"ts_utc":"{day}' not in line:
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("type") == "would_enter" and first_clip(ev):
                entries[(ev["slug"], ev["side"])].append(ev)
            elif ev.get("type") == "resolution":
                resols[(ev["slug"], ev["side"])].append(ev)
    rows = []
    for k, es in entries.items():
        es.sort(key=lambda x: x["ts_utc"])
        rs = sorted(resols.get(k, []), key=lambda x: x["ts_utc"])
        e = es[0]
        if not rs or not e.get("touch_price"):
            continue
        epoch = int(k[0].rsplit("-", 1)[1])
        ts = datetime.fromisoformat(e["ts_utc"].replace("Z", "+00:00")).timestamp()
        rows.append({
            "pnl": CLIP / e["touch_price"] * rs[0]["settle_pnl_per_share"],
            "won": bool(rs[0].get("won")),
            "secs": ts - epoch,
            "dwell": e.get("belief_dwell_s"),
        })
    return rows


def split(rows: list[dict], key: str, buckets) -> list[dict]:
    out = []
    for lo, hi in buckets:
        sel = [r for r in rows if r[key] is not None and lo <= r[key] < hi]
        out.append({
            "bucket": f"{key}[{lo},{'inf' if hi > 1e8 else int(hi)})",
            "n": len(sel),
            "hit": round(100 * sum(r["won"] for r in sel) / len(sel), 1) if sel else None,
            "pnl": round(sum(r["pnl"] for r in sel), 2),
        })
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--date", default=None)
    ap.add_argument("--streams", nargs="+",
                    default=["final=data/runs/shadow_final_sync",
                             "fast=data/runs/shadow_fast_sync"])
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    day = args.date or (datetime.now(timezone.utc) - timedelta(days=1)).strftime("%Y-%m-%d")

    report = {"day": day, "streams": {}}
    for spec in args.streams:
        name, d = spec.split("=", 1)
        rows = day_rows(Path(d).expanduser(), day)
        unk_dwell = sum(1 for r in rows if r["dwell"] is None)
        report["streams"][name] = {
            "n": len(rows),
            "pnl": round(sum(r["pnl"] for r in rows), 2),
            "dwell_unknown": unk_dwell,
            "by_dwell": split(rows, "dwell", DWELL_BUCKETS),
            "by_secs": split(rows, "secs", SECS_BUCKETS),
        }
    print(json.dumps(report, indent=2))
    if args.out:
        with open(args.out, "a") as f:
            f.write(json.dumps(report) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
