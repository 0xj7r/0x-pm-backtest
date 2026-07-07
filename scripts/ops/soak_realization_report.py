#!/usr/bin/env python3
"""Daily realization report: live shadow decisions vs same-day canonical replay.

For each UTC day, joins shadow-final `would_enter`/`resolution` (live feeds)
against the daily replay's per-trade dump (recorded feeds, canonical
accounting) per market, and reports:
  - side agreement on the overlap
  - $50 at-touch P&L of the live-decided set vs the replay-decided set
    (realization ratio; break-even 0.82 per docs/drawdown-sizing-2026-07.md)
  - the same split by entry-second bucket (<15s vs >=15s from window open),
    the candidate gate from docs/live-divergence-analysis-2026-07.md

Usage:
  python3 scripts/ops/soak_realization_report.py --date 2026-07-02 \
    --shadow-dir ~/data/pm-alpha/shadow-final --replay-dir data/runs/daily_replay
"""
from __future__ import annotations

import argparse
import glob
import json
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

CLIP = 50.0
DELAY_BUCKET_S = 15


def live_day(shadow_dir: Path, day: str) -> dict[str, dict]:
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
            if ev.get("type") == "would_enter":
                entries[(ev["slug"], ev["side"])].append(ev)
            elif ev.get("type") == "resolution":
                resols[(ev["slug"], ev["side"])].append(ev)
    out: dict[str, dict] = {}
    for k, ents in entries.items():
        ents.sort(key=lambda x: x["ts_utc"])
        rs = sorted(resols.get(k, []), key=lambda x: x["ts_utc"])
        for e, r in zip(ents, rs):
            if int(e.get("clip", 1)) > 1:
                continue
            touch = e.get("touch_price")
            if not touch:
                continue
            epoch = int(e["slug"].rsplit("-", 1)[1])
            ts = datetime.fromisoformat(e["ts_utc"].replace("Z", "+00:00")).timestamp()
            out[e["slug"]] = {
                "side": e["side"],
                "pnl": CLIP / touch * r["settle_pnl_per_share"],
                "won": bool(r.get("won")),
                "secs": ts - epoch,
            }
    return out


def replay_day(replay_dir: Path, day: str) -> dict[str, dict]:
    fp = replay_dir / f"{day}_trades.jsonl"
    if not fp.exists():
        return {}
    per_market: dict[int, list] = defaultdict(list)
    for line in open(fp):
        t = json.loads(line)
        per_market[t["open_ts_ns"]].append(t)
    out: dict[str, dict] = {}
    for open_ns, ts in per_market.items():
        ts.sort(key=lambda x: x["fill_ts_ns"])
        first = ts[0]
        slug = f"btc-updown-5m-{open_ns // 1_000_000_000}"
        out[slug] = {
            "side": "up" if first["side"].lower() in ("yes", "up") else "down",
            "pnl": first["pnl"],
            "won": bool(first["won"]),
            "secs": first["secs_from_open"],
        }
    return out


def bucket_pnl(rows: list[dict]) -> tuple[float, float]:
    early = sum(r["pnl"] for r in rows if r["secs"] < DELAY_BUCKET_S)
    late = sum(r["pnl"] for r in rows if r["secs"] >= DELAY_BUCKET_S)
    return early, late


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--date", default=None, help="UTC day (default yesterday)")
    ap.add_argument("--shadow-dir", default="/home/ubuntu/data/pm-alpha/shadow-final")
    ap.add_argument("--replay-dir", default="data/runs/daily_replay")
    ap.add_argument("--out", default=None, help="append one JSON line here")
    args = ap.parse_args()

    day = args.date or (datetime.now(timezone.utc) - timedelta(days=1)).strftime("%Y-%m-%d")
    live = live_day(Path(args.shadow_dir).expanduser(), day)
    rep = replay_day(Path(args.replay_dir).expanduser(), day)

    both = set(live) & set(rep)
    same = sum(1 for s in both if live[s]["side"] == rep[s]["side"])
    live_pnl = sum(v["pnl"] for v in live.values())
    rep_pnl = sum(v["pnl"] for v in rep.values())
    ratio = live_pnl / rep_pnl if rep_pnl else float("nan")
    le, ll = bucket_pnl(list(live.values()))
    re_, rl = bucket_pnl(list(rep.values()))

    report = {
        "day": day,
        "live_n": len(live),
        "replay_n": len(rep),
        "overlap": len(both),
        "side_agree_pct": round(100 * same / len(both), 1) if both else None,
        "live_pnl_at_touch": round(live_pnl, 2),
        "replay_pnl": round(rep_pnl, 2),
        "realization_ratio": round(ratio, 3) if rep_pnl else None,
        f"live_pnl_lt{DELAY_BUCKET_S}s": round(le, 2),
        f"live_pnl_ge{DELAY_BUCKET_S}s": round(ll, 2),
        f"replay_pnl_lt{DELAY_BUCKET_S}s": round(re_, 2),
        f"replay_pnl_ge{DELAY_BUCKET_S}s": round(rl, 2),
    }
    print(json.dumps(report, indent=2))
    if args.out:
        with open(args.out, "a") as f:
            f.write(json.dumps(report) + "\n")

    if not rep:
        print(f"NOTE: no replay trades for {day}; run scripts/ops/daily_replay_yesterday.sh first")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
