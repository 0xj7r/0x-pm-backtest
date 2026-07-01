#!/usr/bin/env python3
"""REF + LIVE shadow P&L over a rolling window from Dublin-style paths.

Usage (on server):
  python3 scripts/shadow_pnl_hour.py
  python3 scripts/shadow_pnl_hour.py --hours 3

From laptop (via repo helper):
  ./scripts/shadow_pnl_hour.sh
  ./scripts/shadow_pnl_hour.sh --hours 3
"""
from __future__ import annotations

import argparse
import glob
import json
import re
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
LIVE_ENTER_RE = re.compile(
    r"LIVE ENTER\s+(?P<side>UP|DOWN)\s+(?P<slug>\S+)\s+"
    r"(?:p_up=[\d.]+\s+p_side=[\d.]+|p=[\d.]+)\s+"
    r"touch=(?P<touch>[\d.]+).*clip=(?P<clip>\d+)"
)
TS_RE = re.compile(r"(?P<ts>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
REDEEM_RE = re.compile(r"redeem OK slug=(?P<slug>\S+)")


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def load_jsonl_events(
    paths: list[str],
    cutoff: datetime,
    *,
    all_history: bool = False,
) -> tuple[list[dict], list[dict], list[dict]]:
    entries: list[dict] = []
    resolutions: list[dict] = []
    probes: list[dict] = []
    for fp in paths:
        with open(fp, encoding="utf-8") as f:
            for line in f:
                try:
                    e = json.loads(line)
                except json.JSONDecodeError:
                    continue
                ts = e.get("ts_utc", "")
                if not ts:
                    continue
                try:
                    t = parse_ts(ts)
                except ValueError:
                    continue
                if not all_history and t < cutoff:
                    continue
                typ = e.get("type")
                if typ == "would_enter":
                    entries.append(e)
                elif typ == "resolution":
                    resolutions.append(e)
                elif typ == "quote_probe":
                    probes.append(e)
    return entries, resolutions, probes


def index_resolutions_by_clip(
    entries: list[dict], resolutions: list[dict]
) -> dict[tuple[str, str, int], dict]:
    by_ss_ent: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for e in entries:
        by_ss_ent[(e["slug"], e["side"])].append(e)
    for ents in by_ss_ent.values():
        ents.sort(key=lambda x: x["ts_utc"])

    by_ss_res: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for r in resolutions:
        by_ss_res[(r["slug"], r["side"])].append(r)
    for ress in by_ss_res.values():
        ress.sort(key=lambda x: x["ts_utc"])

    out: dict[tuple[str, str, int], dict] = {}
    for (slug, side), ents in by_ss_ent.items():
        for ent, res in zip(ents, by_ss_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = res
    return out


def parse_live_log(
    log_path: Path, cutoff: datetime
) -> tuple[list[dict], set[str]]:
    live_entries: list[dict] = []
    redeemed_slugs: set[str] = set()
    with open(log_path, encoding="utf-8") as f:
        for line in f:
            line = ANSI_RE.sub("", line)
            ts_m = TS_RE.search(line)
            if not ts_m:
                continue
            try:
                t = parse_ts(ts_m.group("ts") + "Z")
            except ValueError:
                continue
            if t < cutoff:
                continue
            if "LIVE ENTER" in line:
                m = LIVE_ENTER_RE.search(line)
                if m:
                    live_entries.append(
                        {
                            "ts": ts_m.group("ts"),
                            "side": m.group("side").lower(),
                            "slug": m.group("slug"),
                            "touch": float(m.group("touch")),
                            "clip": int(m.group("clip")),
                        }
                    )
            rm = REDEEM_RE.search(line)
            if rm:
                redeemed_slugs.add(rm.group("slug"))
    return live_entries, redeemed_slugs


def live_pnl_at_touch(
    live_entries: list[dict],
    res_by_clip: dict[tuple[str, str, int], dict],
    redeemed_slugs: set[str],
    clip_usd: float,
) -> tuple[float, list[dict], list[dict]]:
    resolved: list[dict] = []
    open_legs: list[dict] = []
    total = 0.0
    for e in live_entries:
        key = (e["slug"], e["side"], e["clip"])
        res = res_by_clip.get(key)
        if e["slug"] in redeemed_slugs and res:
            won = bool(res["won"])
            sps = (1.0 - e["touch"]) if won else (-e["touch"])
            pnl = sps * (clip_usd / e["touch"])
            total += pnl
            resolved.append({**e, "won": won, "pnl": pnl})
        else:
            open_legs.append(e)
    return total, resolved, open_legs


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--hours",
        type=float,
        default=1.0,
        help="rolling window length (default: 1)",
    )
    ap.add_argument(
        "--shadow-dir",
        default="/home/ubuntu/data/pm-alpha/shadow-final",
        help="shadow-final JSONL file or directory",
    )
    ap.add_argument(
        "--live-log",
        default="/home/ubuntu/data/pm-alpha/shadow_exec_tail.log",
    )
    ap.add_argument(
        "--ref-clip-usd",
        type=float,
        default=50.0,
        help="REF telemetry clip (shadow-final target_notional)",
    )
    ap.add_argument(
        "--live-clip-usd",
        type=float,
        default=25.0,
        help="live executor clip",
    )
    args = ap.parse_args()

    now = datetime.now(timezone.utc)
    cutoff = now - timedelta(hours=args.hours)

    shadow_path = Path(args.shadow_dir)
    if shadow_path.is_dir():
        ref_files = sorted(glob.glob(str(shadow_path / "shadow-*.jsonl")))
    else:
        ref_files = [str(shadow_path)]

    entries, resolutions, probes = load_jsonl_events(ref_files, cutoff)
    all_entries, all_resolutions, _ = load_jsonl_events(
        ref_files, cutoff, all_history=True
    )
    res_by_clip = index_resolutions_by_clip(all_entries, all_resolutions)

    resolutions.sort(key=lambda x: x["ts_utc"])
    ref_pnl = sum(float(r.get("ladder_settle_pnl_usd") or 0) for r in resolutions)
    ref_wins = sum(1 for r in resolutions if r.get("won"))
    ref_losses = len(resolutions) - ref_wins

    print("=" * 62)
    print(
        f"REF shadow-final — last {args.hours:g}h "
        f"(since {cutoff.strftime('%H:%M')} UTC)"
    )
    print(f"Now: {now.strftime('%Y-%m-%d %H:%M:%S')} UTC")
    print("=" * 62)
    print(f"  would_enter:   {len(entries)}")
    print(f"  quote_probe:   {len(probes)}")
    print(f"  resolutions:   {len(resolutions)}  (W {ref_wins} / L {ref_losses})")
    print(f"  resolved P&L:  ${ref_pnl:+,.2f}  (@ ${args.ref_clip_usd:.0f}/clip telemetry)")
    print(f"  open legs:     {max(0, len(entries) - len(resolutions))}")

    if resolutions:
        print("\n  Resolutions:")
        for r in resolutions:
            ts = r["ts_utc"][11:19]
            tag = "W" if r.get("won") else "L"
            pnl_r = float(r.get("ladder_settle_pnl_usd") or 0)
            print(f"    {ts} {r['side'].upper():4s} {r['slug'][-10:]} {tag} ${pnl_r:+.2f}")

    live_log = Path(args.live_log)
    if live_log.exists():
        live_entries, redeemed = parse_live_log(live_log, cutoff)
        live_pnl, live_resolved, live_open = live_pnl_at_touch(
            live_entries, res_by_clip, redeemed, args.live_clip_usd
        )
        print()
        print("=" * 62)
        print(f"LIVE shadow_exec_tail — last {args.hours:g}h (@ ${args.live_clip_usd:.0f}/clip)")
        print("=" * 62)
        print(f"  LIVE ENTER:      {len(live_entries)}")
        print(f"  redeemed slugs:  {len(redeemed)}")
        print(f"  resolved est:    {len(live_resolved)}")
        print(f"  open:            {len(live_open)}")
        print(f"  resolved P&L:  ${live_pnl:+,.2f}  (gross @ touch, no fees)")

        if live_resolved:
            print("\n  Resolved:")
            for d in live_resolved:
                tag = "W" if d["won"] else "L"
                print(
                    f"    {d['ts'][11:19]} {d['side'].upper():4s} {d['slug'][-10:]} "
                    f"c{d['clip']} {tag} ${d['pnl']:+.2f}"
                )
        if live_open:
            print("\n  Open:")
            for d in live_open:
                print(
                    f"    {d['ts'][11:19]} {d['side'].upper():4s} {d['slug'][-10:]} "
                    f"c{d['clip']} touch={d['touch']:.2f}"
                )
    else:
        print(f"\n(live log not found: {live_log})")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())