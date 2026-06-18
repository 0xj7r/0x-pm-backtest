#!/usr/bin/env python3
"""Compare shadow-final JSONL vs offline backtest decision log for one UTC day."""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path


def norm_side(side: str) -> str:
    s = (side or "").lower()
    if s in ("up", "yes"):
        return "up"
    if s in ("down", "no"):
        return "down"
    return s


def load_shadow(path: Path, day: str) -> list[dict]:
    out = []
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        e = json.loads(line)
        if e.get("type") != "would_enter":
            continue
        ts = e.get("ts_utc", "")
        if not ts.startswith(day):
            continue
        side = norm_side(e.get("side", ""))
        out.append(
            {
                "key": (e["slug"], side, int(e.get("clip", 1))),
                "slug": e["slug"],
                "side": side,
                "clip": int(e.get("clip", 1)),
                "touch": float(e.get("touch_price") or 0),
                "edge": float(e.get("edge") or 0),
                "ts": ts,
            }
        )
    return out


def slug_from_row(e: dict) -> str:
    slug = e.get("slug") or ""
    if slug:
        return slug
    open_ns = e.get("open_ts_ns")
    window = int(e.get("window_secs") or 300)
    if open_ns is not None:
        open_s = int(open_ns) // 1_000_000_000
        if window == 300:
            return f"btc-updown-5m-{open_s}"
        if window == 900:
            return f"btc-updown-15m-{open_s}"
    return ""


def row_day(slug: str, ts: str) -> str:
    if ts:
        return ts[:10]
    try:
        from datetime import datetime, timezone

        close = int(slug.rsplit("-", 1)[1]) + 300
        return datetime.fromtimestamp(close, tz=timezone.utc).date().isoformat()
    except (ValueError, IndexError):
        return ""


def load_backtest(path: Path, day: str) -> list[dict]:
    out = []
    clip_by_slug_side: dict[tuple[str, str], int] = defaultdict(int)
    rows_raw = []
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        e = json.loads(line)
        slug = slug_from_row(e)
        if not slug:
            continue
        ts = e.get("ts_utc") or e.get("ts") or ""
        if day not in ts and row_day(slug, ts) != day:
            continue
        side = norm_side(e.get("side") or e.get("entry_side") or "")
        if not side:
            continue
        rows_raw.append((slug, side, e, ts))
        clip_by_slug_side[(slug, side)] += 1

    for slug, side, e, ts in rows_raw:
        clip = int(e.get("clip") or clip_by_slug_side[(slug, side)])
        out.append(
            {
                "key": (slug, side, clip),
                "slug": slug,
                "side": side,
                "clip": clip,
                "touch": float(
                    e.get("touch_price")
                    or e.get("entry_touch")
                    or e.get("side_ask_at_decision")
                    or 0
                ),
                "edge": float(e.get("edge") or 0),
                "ts": ts,
            }
        )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow", required=True, help="combined shadow-final JSONL")
    ap.add_argument("--backtest", required=True, help="alpha --decision-log JSONL")
    ap.add_argument("--day", required=True, help="UTC date YYYY-MM-DD")
    args = ap.parse_args()

    shadow = load_shadow(Path(args.shadow), args.day)
    back = load_backtest(Path(args.backtest), args.day)

    s_keys = {x["key"] for x in shadow}
    b_keys = {x["key"] for x in back}

    only_s = s_keys - b_keys
    only_b = b_keys - s_keys
    both = s_keys & b_keys

    # side agreement on shared slugs (ignore clip)
    by_slug_s = defaultdict(set)
    by_slug_b = defaultdict(set)
    for x in shadow:
        by_slug_s[x["slug"]].add(x["side"])
    for x in back:
        by_slug_b[x["slug"]].add(x["side"])

    opp = []
    for slug in set(by_slug_s) & set(by_slug_b):
        if by_slug_s[slug] != by_slug_b[slug]:
            opp.append((slug, by_slug_s[slug], by_slug_b[slug]))

    print(f"=== slug parity {args.day} ===")
    print(f"shadow would_enter: {len(shadow)}")
    print(f"backtest entries:   {len(back)}")
    print(f"matched keys:       {len(both)}")
    print(f"shadow-only keys:   {len(only_s)}")
    print(f"backtest-only keys: {len(only_b)}")
    print(f"opposite side slugs: {len(opp)}")

    if only_s:
        print("\nshadow-only (first 15):")
        for k in sorted(only_s)[:15]:
            print(f"  {k}")
    if only_b:
        print("\nbacktest-only (first 15):")
        for k in sorted(only_b)[:15]:
            print(f"  {k}")
    if opp:
        print("\nopposite side (first 15):")
        for slug, ss, bs in opp[:15]:
            print(f"  {slug[-20:]} shadow={ss} backtest={bs}")

    return 0 if not only_s and not only_b and not opp else 1


if __name__ == "__main__":
    raise SystemExit(main())