#!/usr/bin/env python3
"""Daily paper-soak scorecard: PASS/FAIL per UTC day against hard criteria.

Criteria (a soak day PASSES when all hold):
  - heartbeats: >= MIN_HEARTBEATS shadow-final summary lines (feed/process uptime)
  - entries:    > 0 would_enter on non-Saturday days (engine actually deciding)
  - parity:     orphan == 0 and missed_ref <= MAX_MISSED in the executor log
  - mismatch:   0 side mismatches between submits and reference

Usage (cron, 00:10 UTC, scores the previous day):
  python3 scripts/ops/paper_soak_report.py
  python3 scripts/ops/paper_soak_report.py --date 2026-07-02
"""
from __future__ import annotations

import argparse
import glob
import json
import re
from datetime import datetime, timedelta, timezone
from pathlib import Path

MIN_HEARTBEATS = 1380  # 1440 minutes minus restart/warmup slack
MAX_MISSED = 5

PARITY_RE = re.compile(r'\{"type":"parity_stats".*?\}')


def day_events(shadow_dir: Path, day: str) -> dict:
    hb = 0
    entries = 0
    resolutions = 0
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for line in open(fp, errors="replace"):
            if f'"ts_utc":"{day}' not in line:
                continue
            if '"type":"summary"' in line:
                hb += 1
            elif '"type":"would_enter"' in line:
                entries += 1
            elif '"type":"resolution"' in line:
                resolutions += 1
    return {"heartbeats": hb, "entries": entries, "resolutions": resolutions}


def day_parity(exec_log: Path, day: str) -> dict:
    """Last parity_stats of the day plus mismatch/orphan event counts."""
    last_stats: dict = {}
    orphans = 0
    mismatches = 0
    if not exec_log.exists():
        return {"orphan_events": -1, "mismatch_events": -1, "last_stats": {}}
    for line in open(exec_log, errors="replace"):
        if day not in line:
            continue
        if '"type":"parity_stats"' in line:
            m = PARITY_RE.search(line)
            if m:
                try:
                    last_stats = json.loads(m.group(0))
                except json.JSONDecodeError:
                    pass
        elif '"kind":"orphan"' in line:
            orphans += 1
        elif '"kind":"mismatch"' in line or "side mismatch" in line:
            mismatches += 1
    return {"orphan_events": orphans, "mismatch_events": mismatches, "last_stats": last_stats}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--date", default=None, help="UTC day to score (default: yesterday)")
    ap.add_argument("--shadow-dir", default="/home/ubuntu/data/pm-alpha/shadow-final")
    ap.add_argument("--exec-log", default="/home/ubuntu/data/pm-alpha/shadow_exec_tail.log")
    ap.add_argument("--out", default="/home/ubuntu/data/pm-alpha/week_monitor/soak_report.log")
    args = ap.parse_args()

    day = args.date or (datetime.now(timezone.utc) - timedelta(days=1)).strftime("%Y-%m-%d")
    is_saturday = datetime.strptime(day, "%Y-%m-%d").weekday() == 5

    ev = day_events(Path(args.shadow_dir), day)
    pa = day_parity(Path(args.exec_log), day)
    stats = pa["last_stats"]

    checks = {
        "heartbeats": ev["heartbeats"] >= MIN_HEARTBEATS,
        "entries": is_saturday or ev["entries"] > 0,
        "orphan": pa["orphan_events"] == 0 and stats.get("orphan", 0) == 0,
        "missed_ref": stats.get("missed_ref", 0) <= MAX_MISSED,
        "mismatch": pa["mismatch_events"] == 0,
    }
    verdict = "PASS" if all(checks.values()) else "FAIL"
    failed = ",".join(k for k, ok in checks.items() if not ok) or "-"

    line = (
        f"{day} {verdict} hb={ev['heartbeats']} entries={ev['entries']}"
        f" resol={ev['resolutions']} matched={stats.get('matched', 0)}"
        f" orphan={pa['orphan_events']}/{stats.get('orphan', 0)}"
        f" missed={stats.get('missed_ref', 0)} mismatch={pa['mismatch_events']}"
        f" failed={failed}{' (saturday)' if is_saturday else ''}"
    )
    print(line)
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("a") as f:
        f.write(line + "\n")
    return 0 if verdict == "PASS" else 1


if __name__ == "__main__":
    raise SystemExit(main())
