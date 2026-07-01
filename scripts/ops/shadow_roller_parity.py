#!/usr/bin/env python3
"""Shadow roller ↔ reference wallet parity (split / penny-dump / redeem timing).

Compares shadow_roller_logger JSONL against whale /activity cache or live pull.

Usage:
  python3 scripts/ops/shadow_roller_parity.py \\
    --shadow-dir ~/data/pm-alpha/shadow-roller \\
    --wallet 0x4d64518a17816c43719e4337294b61107611e544 \\
    --since-hours 6

Exit 0 when missed_shadow=0 and missed_wallet=0 for all three legs; else 1.
"""
from __future__ import annotations

import argparse
import glob
import json
import re
import statistics as st
import sys
import time
import urllib.parse
import urllib.request
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

ACTIVITY_API = "https://data-api.polymarket.com/activity"

UPDOWN = re.compile(r"^([a-z]+)-updown-(\d+)m-(\d+)$")
SPLIT = "roller_would_split"
DUMP = "roller_would_dump"
REDEEM = "roller_would_redeem"


def parse_ts(ts: str) -> float:
    return datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()


def slug_meta(slug: str) -> dict | None:
    m = UPDOWN.match(slug or "")
    if not m:
        return None
    mins = int(m.group(2))
    start = int(m.group(3))
    return {"asset": m.group(1), "horizon": f"{mins}m", "wstart": start, "wclose": start + mins * 60}


def load_shadow(shadow_dir: Path, since_epoch: float) -> dict[str, dict[str, dict]]:
    """slug -> leg -> event dict."""
    out: dict[str, dict[str, dict]] = defaultdict(dict)
    for fp in sorted(glob.glob(str(shadow_dir / "roller-*.jsonl"))):
        for line in Path(fp).read_text().splitlines():
            if not line.strip():
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            typ = ev.get("type")
            if typ not in (SPLIT, DUMP, REDEEM):
                continue
            ts = ev.get("ts_utc", "")
            epoch = parse_ts(ts) if ts else 0.0
            if since_epoch > 0 and epoch < since_epoch:
                continue
            slug = ev.get("slug", "")
            if not slug:
                continue
            out[slug][typ] = ev
    return out


def fetch_wallet_live(addr: str, since_epoch: float) -> list[dict]:
    rows: list[dict] = []
    offset = 0
    end = int(time.time())
    while offset <= 10000:
        q = urllib.parse.urlencode({
            "user": addr,
            "limit": 500,
            "offset": offset,
            "start": int(since_epoch),
            "end": end,
        })
        req = urllib.request.Request(
            f"{ACTIVITY_API}?{q}",
            headers={"User-Agent": "pm-roller-parity/1.0"},
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            page = json.load(resp)
        if not isinstance(page, list) or not page:
            break
        rows.extend(page)
        if len(page) < 500:
            break
        offset += 500
        time.sleep(0.1)
    return rows


def load_wallet_rows(path: Path, since_epoch: float) -> list[dict]:
    seen: set[tuple] = set()
    rows: list[dict] = []
    if not path.exists():
        return rows
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        r = json.loads(line)
        if r.get("timestamp", 0) < since_epoch:
            continue
        key = (
            r.get("transactionHash"),
            r.get("type"),
            r.get("asset"),
            r.get("side"),
            r.get("size"),
            r.get("price"),
            r.get("timestamp"),
            r.get("slug"),
        )
        if key in seen:
            continue
        seen.add(key)
        rows.append(r)
    rows.sort(key=lambda x: x["timestamp"])
    return rows


def wallet_legs(rows: list[dict]) -> dict[str, dict[str, dict]]:
    """Per slug: split / dump / redeem reference events."""
    by_slug: dict[str, list[dict]] = defaultdict(list)
    for r in rows:
        meta = slug_meta(r.get("slug", ""))
        if not meta or meta["asset"] != "btc" or meta["horizon"] != "5m":
            continue
        by_slug[r["slug"]].append(r)

    out: dict[str, dict[str, dict]] = {}
    for slug, evs in by_slug.items():
        meta = slug_meta(slug)
        assert meta is not None
        wstart, wclose = meta["wstart"], meta["wclose"]
        split_ev = dump_ev = redeem_ev = None
        for r in sorted(evs, key=lambda x: x["timestamp"]):
            typ = r.get("type")
            if typ == "SPLIT" and split_ev is None:
                split_ev = {
                    "ts": r["timestamp"],
                    "secs_from_open": r["timestamp"] - wstart,
                    "usd": float(r.get("usdcSize") or 0),
                    "sh": float(r.get("size") or 0),
                }
            elif typ == "REDEEM" and redeem_ev is None:
                redeem_ev = {
                    "ts": r["timestamp"],
                    "secs_after_close": r["timestamp"] - wclose,
                    "usd": float(r.get("usdcSize") or 0),
                    "sh": float(r.get("size") or 0),
                }
            elif typ == "TRADE" and r.get("side") == "SELL":
                px = float(r.get("price") or 0)
                if px <= 0.02 and dump_ev is None:
                    dump_ev = {
                        "ts": r["timestamp"],
                        "secs_to_close": wclose - r["timestamp"],
                        "price": px,
                        "usd": float(r.get("usdcSize") or 0),
                        "sh": float(r.get("size") or 0),
                        "outcome": r.get("outcome"),
                    }
        legs: dict[str, dict] = {}
        if split_ev:
            legs[SPLIT] = split_ev
        if dump_ev:
            legs[DUMP] = dump_ev
        if redeem_ev:
            legs[REDEEM] = redeem_ev
        if legs:
            out[slug] = legs
    return out


def slug_age_s(slug: str, leg: str) -> float | None:
    meta = slug_meta(slug)
    if not meta:
        return None
    now = time.time()
    if leg == SPLIT:
        return now - meta["wstart"]
    if leg == DUMP:
        return meta["wclose"] - now
    return now - meta["wclose"]


def compare_leg(
    leg: str,
    shadow_by_slug: dict[str, dict[str, dict]],
    wallet_by_slug: dict[str, dict[str, dict]],
    tol_s: float,
    slug_universe: set[str],
    grace_s: float,
    shadow_start: float,
) -> dict:
    slugs = sorted(slug_universe)
    matched = []
    missed_shadow = []
    missed_wallet = []
    skipped_grace = 0
    deltas = []

    for slug in slugs:
        s = shadow_by_slug.get(slug, {}).get(leg)
        w = wallet_by_slug.get(slug, {}).get(leg)
        if s and w:
            if leg == SPLIT:
                sd = s.get("secs_from_open", 0)
                wd = w.get("secs_from_open", 0)
            elif leg == DUMP:
                sd = s.get("secs_to_close", 0)
                wd = w.get("secs_to_close", 0)
            else:
                sd = s.get("secs_after_close", 0)
                wd = w.get("secs_after_close", 0)
            delta = sd - wd
            deltas.append(delta)
            matched.append({"slug": slug, "shadow": sd, "wallet": wd, "delta_s": delta})
        elif w and not s:
            meta = slug_meta(slug)
            if meta and meta["wclose"] < shadow_start:
                continue
            missed_shadow.append({"slug": slug, "wallet": w})
        elif s and not w:
            age = slug_age_s(slug, leg)
            if age is not None and age < grace_s:
                skipped_grace += 1
                continue
            missed_wallet.append({"slug": slug, "shadow": s})

    within = sum(1 for d in deltas if abs(d) <= tol_s)
    return {
        "leg": leg,
        "matched": len(matched),
        "within_tol": within,
        "missed_shadow": len(missed_shadow),
        "missed_wallet": len(missed_wallet),
        "deltas": deltas,
        "missed_shadow_slugs": [x["slug"] for x in missed_shadow[:10]],
        "missed_wallet_slugs": [x["slug"] for x in missed_wallet[:10]],
        "skipped_grace": skipped_grace,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--wallet", required=True)
    ap.add_argument("--wallet-jsonl", help="override data/runs/whales/<addr>.jsonl")
    ap.add_argument(
        "--live",
        action="store_true",
        help="fetch wallet /activity live (avoids stale cache for recent redeems)",
    )
    ap.add_argument("--since-hours", type=float, default=6.0)
    ap.add_argument("--tol-s", type=float, default=60.0, help="timing match tolerance")
    ap.add_argument(
        "--mode",
        choices=("overlap", "union"),
        default="overlap",
        help="overlap=only slugs shadow logged (soak); union=all wallet+shadow slugs",
    )
    ap.add_argument(
        "--require-dump",
        action="store_true",
        help="include penny-dump in PASS criteria (default: split+redeem only)",
    )
    ap.add_argument(
        "--grace-s",
        type=float,
        default=600.0,
        help="exclude recent windows from missed_wallet (API lag)",
    )
    args = ap.parse_args()

    since_epoch = time.time() - args.since_hours * 3600
    shadow_dir = Path(args.shadow_dir)
    wallet = args.wallet.lower()
    wallet_path = Path(args.wallet_jsonl or f"data/runs/whales/{wallet}.jsonl")

    shadow = load_shadow(shadow_dir, since_epoch)
    if args.live:
        wallet_rows = fetch_wallet_live(wallet, since_epoch)
    else:
        wallet_rows = load_wallet_rows(wallet_path, since_epoch)
    wallet_by_slug = wallet_legs(wallet_rows)

    print(
        f"# shadow_roller parity wallet={wallet[:10]}… "
        f"since={args.since_hours:.1f}h tol={args.tol_s:.0f}s"
    )
    shadow_start = since_epoch
    first_shadow: float | None = None
    for legs in shadow.values():
        for ev in legs.values():
            ts = ev.get("ts_utc", "")
            if ts:
                epoch = parse_ts(ts)
                first_shadow = epoch if first_shadow is None else min(first_shadow, epoch)
    if first_shadow is not None:
        shadow_start = first_shadow

    if args.mode == "overlap":
        slug_universe = set(shadow)
    else:
        slug_universe = set(shadow) | set(wallet_by_slug)

    print(
        f"shadow_slugs={len(shadow)} wallet_slugs={len(wallet_by_slug)} "
        f"wallet_rows={len(wallet_rows)} mode={args.mode} compare_slugs={len(slug_universe)} "
        f"shadow_start={datetime.fromtimestamp(shadow_start, tz=timezone.utc).isoformat()}"
    )
    print()

    totals = {"missed_shadow": 0, "missed_wallet": 0}
    required_legs = [SPLIT, REDEEM]
    if args.require_dump:
        required_legs.append(DUMP)

    for leg, label in (
        (SPLIT, "split (secs from open)"),
        (DUMP, "penny-dump (secs to close)"),
        (REDEEM, "redeem (secs after close)"),
    ):
        r = compare_leg(
            leg, shadow, wallet_by_slug, args.tol_s, slug_universe, args.grace_s, shadow_start
        )
        if leg in required_legs:
            totals["missed_shadow"] += r["missed_shadow"]
            totals["missed_wallet"] += r["missed_wallet"]
        print(f"## {label}")
        print(
            f"  matched={r['matched']} within_tol={r['within_tol']} "
            f"missed_shadow={r['missed_shadow']} missed_wallet={r['missed_wallet']} "
            f"grace_skip={r['skipped_grace']}"
        )
        if r["deltas"]:
            ds = sorted(r["deltas"])
            print(
                f"  delta_s: median={st.median(ds):+.1f} "
                f"p90={ds[int(0.9 * len(ds))]:+.1f} "
                f"min={min(ds):+.1f} max={max(ds):+.1f}"
            )
        if r["missed_shadow_slugs"]:
            print(f"  wallet-only slugs: {', '.join(r['missed_shadow_slugs'][:5])}")
        if r["missed_wallet_slugs"]:
            print(f"  shadow-only slugs: {', '.join(r['missed_wallet_slugs'][:5])}")
        print()

    req = "+".join(x.replace("roller_would_", "") for x in required_legs)
    print(
        f"SUMMARY required={req} missed_shadow={totals['missed_shadow']} "
        f"missed_wallet={totals['missed_wallet']}"
    )

    if totals["missed_shadow"] == 0 and totals["missed_wallet"] == 0:
        print("PASS")
        return 0
    print("FAIL")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())