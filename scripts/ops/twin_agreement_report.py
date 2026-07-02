#!/usr/bin/env python3
"""Daily decision-twin agreement: shadow-final vs shadow-final-b.

Two identical-config engines on independent feed connections. Where they
disagree, the decision was feed-microstate noise (untradeable); where they
agree, the decision is stable. The capturable edge is the agree-subset P&L.
Also splits by belief dwell to validate the dwell-gate hypothesis: stable
(high-dwell) entries should show higher twin agreement.

Usage:
  python3 scripts/ops/twin_agreement_report.py --date 2026-07-03 \
    --a-dir ~/data/pm-alpha/shadow-final --b-dir ~/data/pm-alpha/shadow-final-b
"""
from __future__ import annotations

import argparse
import glob
import json
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

CLIP = 50.0


def first_entries(shadow_dir: Path, day: str) -> dict[str, dict]:
    entries: dict[str, list] = defaultdict(list)
    resols: dict[tuple, list] = defaultdict(list)
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for line in open(fp, errors="replace"):
            if f'"ts_utc":"{day}' not in line:
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("type") == "would_enter" and int(ev.get("clip", 1)) == 1:
                entries[ev["slug"]].append(ev)
            elif ev.get("type") == "resolution":
                resols[(ev["slug"], ev["side"])].append(ev)
    out: dict[str, dict] = {}
    for slug, es in entries.items():
        es.sort(key=lambda x: x["ts_utc"])
        e = es[0]
        rs = resols.get((slug, e["side"]), [])
        touch = e.get("touch_price") or 0
        pnl = None
        if rs and touch:
            pnl = CLIP / touch * rs[0]["settle_pnl_per_share"]
        epoch = int(slug.rsplit("-", 1)[1])
        ts = datetime.fromisoformat(e["ts_utc"].replace("Z", "+00:00")).timestamp()
        out[slug] = {
            "side": e["side"],
            "secs": ts - epoch,
            "dwell": e.get("belief_dwell_s"),
            "pnl": pnl,
        }
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--date", default=None)
    ap.add_argument("--a-dir", default="/home/ubuntu/data/pm-alpha/shadow-final")
    ap.add_argument("--b-dir", default="/home/ubuntu/data/pm-alpha/shadow-final-b")
    ap.add_argument("--out", default=None, help="append one JSON line here")
    args = ap.parse_args()

    day = args.date or (datetime.now(timezone.utc) - timedelta(days=1)).strftime("%Y-%m-%d")
    a = first_entries(Path(args.a_dir).expanduser(), day)
    b = first_entries(Path(args.b_dir).expanduser(), day)
    both = set(a) & set(b)
    agree = [s for s in both if a[s]["side"] == b[s]["side"]]
    disagree = [s for s in both if a[s]["side"] != b[s]["side"]]

    def pnl_of(slugs):
        vals = [a[s]["pnl"] for s in slugs if a[s]["pnl"] is not None]
        return round(sum(vals), 2), len(vals)

    agree_pnl, _ = pnl_of(agree)
    dis_pnl, _ = pnl_of(disagree)

    def dwell_split(slugs):
        hi = [s for s in slugs if (a[s]["dwell"] or 0) >= 30]
        return len(hi), len(slugs) - len(hi)

    agree_hi, agree_lo = dwell_split(agree)
    dis_hi, dis_lo = dwell_split(disagree)
    hi_total = agree_hi + dis_hi
    lo_total = agree_lo + dis_lo

    report = {
        "day": day,
        "a_n": len(a),
        "b_n": len(b),
        "both": len(both),
        "agree_pct": round(100 * len(agree) / len(both), 1) if both else None,
        "agree_pnl_at_touch": agree_pnl,
        "disagree_pnl_at_touch": dis_pnl,
        "agree_pct_dwell_ge30": round(100 * agree_hi / hi_total, 1) if hi_total else None,
        "agree_pct_dwell_lt30": round(100 * agree_lo / lo_total, 1) if lo_total else None,
    }
    print(json.dumps(report, indent=2))
    if args.out:
        with open(args.out, "a") as f:
            f.write(json.dumps(report) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
