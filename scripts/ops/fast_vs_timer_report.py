#!/usr/bin/env python3
"""Daily head-to-head: fast engine vs timer engine on identical markets.

The direct live measurement of the latency tax. Both streams run the same
gated config; on common (market, side) entries the outcomes are identical,
so the P&L difference is purely entry-price capture. Timer-only entries are
stale-edge trades the fast engine's fresher view skipped.

Usage:
  python3 scripts/ops/fast_vs_timer_report.py --date 2026-07-08 \
    --out data/runs/daily_replay/fast_vs_timer.jsonl
"""
from __future__ import annotations

import argparse
import glob
import json
from collections import defaultdict
from datetime import datetime, timedelta, timezone

CLIP = 50.0


def load(dirpat: str, day: str) -> dict:
    ents, res = {}, defaultdict(list)
    for fp in sorted(glob.glob(dirpat)):
        for line in open(fp, errors="replace"):
            if f'"ts_utc":"{day}' not in line:
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            k = (ev.get("slug"), ev.get("side"))
            if ev.get("type") == "would_enter" and int(ev.get("clip", 1)) <= 1:
                if k not in ents:
                    ents[k] = ev
            elif ev.get("type") == "resolution":
                res[k].append(ev)
    rows = {}
    for k, e in ents.items():
        rs = sorted(res.get(k, []), key=lambda x: x["ts_utc"])
        t = e.get("touch_price")
        if rs and t:
            rows[k] = {
                "pnl": CLIP / t * rs[0]["settle_pnl_per_share"],
                "won": bool(rs[0].get("won")),
                "ts": e["ts_utc"],
            }
    return rows


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--date", default=None)
    ap.add_argument("--timer-dir", default="data/runs/shadow_final_sync")
    ap.add_argument("--fast-dir", default="data/runs/shadow_fast_sync")
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    day = args.date or (datetime.now(timezone.utc) - timedelta(days=1)).strftime("%Y-%m-%d")

    timer = load(f"{args.timer_dir}/shadow-*.jsonl", day)
    fast = load(f"{args.fast_dir}/shadow-*.jsonl", day)
    common = set(timer) & set(fast)

    def ts(s):
        return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()

    deltas = sorted(ts(timer[k]["ts"]) - ts(fast[k]["ts"]) for k in common)
    report = {
        "day": day,
        "timer_n": len(timer),
        "fast_n": len(fast),
        "common_n": len(common),
        "timer_pnl": round(sum(v["pnl"] for v in timer.values()), 2),
        "fast_pnl": round(sum(v["pnl"] for v in fast.values()), 2),
        "common_timer_pnl": round(sum(timer[k]["pnl"] for k in common), 2),
        "common_fast_pnl": round(sum(fast[k]["pnl"] for k in common), 2),
        "latency_tax_on_common": round(
            sum(timer[k]["pnl"] for k in common) - sum(fast[k]["pnl"] for k in common), 2),
        "fast_earlier_med_s": round(deltas[len(deltas) // 2], 3) if deltas else None,
        "timer_only_n": len(set(timer) - set(fast)),
        "timer_only_pnl": round(sum(timer[k]["pnl"] for k in set(timer) - set(fast)), 2),
        "fast_only_n": len(set(fast) - set(timer)),
        "fast_only_pnl": round(sum(fast[k]["pnl"] for k in set(fast) - set(timer)), 2),
    }
    print(json.dumps(report, indent=2))
    if args.out:
        with open(args.out, "a") as f:
            f.write(json.dumps(report) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
