#!/usr/bin/env python3
"""Audit live execution quality vs shadow-final REF touch (fill realization proxy).

Pairs shadow SUBMITTED accepted=true with the preceding LIVE ENTER touch, matches
to shadow-final would_enter touch_price + marketable_limit_price.

Pairs venue avg_fill_price (when logged on SUBMITTED) with REF touch for true
fill realization. Also reports decision-stream touch parity and submit reliability.

Usage (Dublin):
  python3 scripts/ops/fill_realization_audit.py
  python3 scripts/ops/fill_realization_audit.py --since-hours 24

From Mac:
  ./scripts/ops/fill_realization_audit.sh
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import statistics
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
ISO_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")
SUBMITTED_RE = re.compile(
    r"shadow SUBMITTED.*slug=(?P<slug>\S+).*accepted=(?P<acc>true|false)"
    r"(?:.*avg_fill_price=(?P<fill>[\d.]+))?"
    r"(?:.*filled_qty=(?P<qty>[\d.]+))?"
)
MISS_RE = re.compile(r"submit miss slug=(?P<slug>\S+)")
LIVE_ENTER_RE = re.compile(
    r"LIVE ENTER\s+(?P<side>UP|DOWN)\s+(?P<slug>\S+)\s+"
    r"(?:p_up=(?P<p_up>[\d.]+)\s+p_side=(?P<p_side>[\d.]+)|p=(?P<p>[\d.]+))\s+"
    r"touch=(?P<touch>[\d.]+).*clip=(?P<clip>\d+)"
)


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def line_epoch(line: str) -> float | None:
    iso = ISO_RE.search(line)
    if iso:
        return parse_ts(iso.group(1)).timestamp()
    return None


def load_shadow_entries(shadow_dir: Path) -> dict[str, list[dict]]:
    by_slug: dict[str, list[dict]] = defaultdict(list)
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        with open(fp, encoding="utf-8") as f:
            for line in f:
                try:
                    e = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if e.get("type") == "would_enter":
                    by_slug[e["slug"]].append(e)
    for ents in by_slug.values():
        ents.sort(key=lambda x: x["ts_utc"])
    return by_slug


def best_ref_entry(slug: str, epoch: float, by_slug: dict[str, list[dict]], window_s: float) -> dict | None:
    best = None
    best_dt = 1e18
    for ent in by_slug.get(slug, []):
        dt = abs(parse_ts(ent["ts_utc"]).timestamp() - epoch)
        if dt <= window_s and dt < best_dt:
            best_dt = dt
            best = ent
    return best


def touch_band(touch: float) -> str:
    if touch < 0.15:
        return "0.00-0.15"
    if touch < 0.30:
        return "0.15-0.30"
    if touch < 0.45:
        return "0.30-0.45"
    if touch < 0.55:
        return "0.45-0.55"
    if touch < 0.70:
        return "0.55-0.70"
    return "0.70-1.00"


def pct(xs: list[float], p: float) -> float:
    if not xs:
        return 0.0
    xs = sorted(xs)
    i = int(p * (len(xs) - 1))
    return xs[i]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", default=os.path.expanduser("~/data/pm-alpha/shadow-final"))
    ap.add_argument("--live-log", default=os.path.expanduser("~/data/pm-alpha/shadow_exec_tail.log"))
    ap.add_argument("--since-hours", type=float, default=0.0, help="0 = all log history")
    ap.add_argument("--match-window-s", type=float, default=120.0)
    ap.add_argument("--out-dir", default=os.path.expanduser("~/data/pm-alpha/week_monitor"))
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    cutoff = 0.0
    if args.since_hours > 0:
        cutoff = datetime.now(timezone.utc).timestamp() - args.since_hours * 3600

    by_slug = load_shadow_entries(Path(args.shadow_dir))
    live_log = Path(args.live_log)
    if not live_log.is_file():
        print(f"live log missing: {live_log}", file=__import__("sys").stderr)
        return 1

    # Pending LIVE ENTER by (slug, clip) for pairing with SUBMITTED
    pending_enter: dict[tuple[str, int], dict] = {}
    submits: list[dict] = []
    misses = 0
    miss_slugs: set[str] = set()

    for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
        line = strip_ansi(raw)
        epoch = line_epoch(line)
        if epoch is None or (cutoff and epoch < cutoff):
            continue

        m = LIVE_ENTER_RE.search(line)
        if m:
            clip = int(m.group("clip"))
            pending_enter[(m.group("slug"), clip)] = {
                "epoch": epoch,
                "slug": m.group("slug"),
                "side": m.group("side").lower(),
                "touch": float(m.group("touch")),
                "clip": clip,
            }
            continue

        mm = MISS_RE.search(line)
        if mm:
            misses += 1
            miss_slugs.add(mm.group("slug"))
            continue

        sm = SUBMITTED_RE.search(line)
        if sm and sm.group("acc") == "true":
            slug = sm.group("slug")
            fill_g = sm.group("fill")
            qty_g = sm.group("qty")
            avg_fill = float(fill_g) if fill_g else None
            filled_qty = float(qty_g) if qty_g else None
            # Pair with most recent LIVE ENTER for this slug (any clip: rearm)
            cand = None
            for (s, _clip), ent in pending_enter.items():
                if s != slug:
                    continue
                if cand is None or ent["epoch"] > cand["epoch"]:
                    cand = ent
            if cand and epoch - cand["epoch"] <= 30:
                submits.append(
                    {
                        **cand,
                        "submit_epoch": epoch,
                        "avg_fill_price": avg_fill,
                        "filled_qty": filled_qty,
                    }
                )
            else:
                submits.append(
                    {
                        "epoch": epoch,
                        "submit_epoch": epoch,
                        "slug": slug,
                        "side": "",
                        "touch": None,
                        "clip": 0,
                        "avg_fill_price": avg_fill,
                        "filled_qty": filled_qty,
                    }
                )

    ratios: list[float] = []
    venue_ratios: list[float] = []
    venue_over_live: list[float] = []
    limit_ratios: list[float] = []
    live_touch_ratios: list[float] = []
    by_band: dict[str, list[float]] = defaultdict(list)
    unmatched = 0
    rows: list[dict] = []

    for sub in submits:
        ref = best_ref_entry(sub["slug"], sub["submit_epoch"], by_slug, args.match_window_s)
        if not ref:
            unmatched += 1
            continue
        ref_touch = float(ref.get("touch_price") or 0)
        if ref_touch <= 0:
            unmatched += 1
            continue
        live_touch = sub.get("touch")
        avg_fill = sub.get("avg_fill_price")
        mlp = float(ref.get("marketable_limit_price") or 0)
        ratio = (live_touch / ref_touch) if live_touch else None
        venue_ratio = (avg_fill / ref_touch) if avg_fill else None
        if ratio is not None:
            ratios.append(ratio)
            live_touch_ratios.append(ratio)
            band = touch_band(ref_touch)
            by_band[band].append(ratio)
        if venue_ratio is not None:
            venue_ratios.append(venue_ratio)
            if live_touch:
                venue_over_live.append(avg_fill / live_touch)
        if mlp > 0:
            limit_ratios.append(mlp / ref_touch)
        rows.append(
            {
                "slug": sub["slug"],
                "ref_touch": ref_touch,
                "live_touch": live_touch,
                "avg_fill_price": avg_fill,
                "filled_qty": sub.get("filled_qty"),
                "marketable_limit": mlp,
                "ratio_live_ref": ratio,
                "ratio_venue_ref": venue_ratio,
                "limit_over_touch": mlp / ref_touch if mlp > 0 else None,
            }
        )

    attempts = len(submits) + misses
    hit_rate_submit = 100.0 * len(submits) / attempts if attempts else 0.0

    summary = {
        "ts_utc": datetime.now(timezone.utc).isoformat(),
        "since_hours": args.since_hours,
        "submitted": len(submits),
        "submit_misses": misses,
        "submit_attempts": attempts,
        "submit_success_pct": hit_rate_submit,
        "matched_ref": len(rows),
        "unmatched_ref": unmatched,
        "realization_live_over_ref": {
            "n": len(ratios),
            "median": statistics.median(ratios) if ratios else None,
            "mean": statistics.mean(ratios) if ratios else None,
            "p10": pct(ratios, 0.10) if ratios else None,
            "p90": pct(ratios, 0.90) if ratios else None,
        },
        "realization_venue_over_ref": {
            "n": len(venue_ratios),
            "median": statistics.median(venue_ratios) if venue_ratios else None,
            "mean": statistics.mean(venue_ratios) if venue_ratios else None,
            "p10": pct(venue_ratios, 0.10) if venue_ratios else None,
            "p90": pct(venue_ratios, 0.90) if venue_ratios else None,
        },
        "realization_venue_over_live_touch": {
            "n": len(venue_over_live),
            "median": statistics.median(venue_over_live) if venue_over_live else None,
            "mean": statistics.mean(venue_over_live) if venue_over_live else None,
        },
        "venue_fill_logged": sum(1 for s in submits if s.get("avg_fill_price")),
        "marketable_limit_over_touch": {
            "n": len(limit_ratios),
            "median": statistics.median(limit_ratios) if limit_ratios else None,
            "mean": statistics.mean(limit_ratios) if limit_ratios else None,
        },
        "by_touch_band": {
            band: {
                "n": len(v),
                "median": statistics.median(v),
            }
            for band, v in sorted(by_band.items())
        },
    }

    if args.json:
        print(json.dumps(summary, indent=2))
        return 0

    lines = [
        "=" * 60,
        "FILL REALIZATION AUDIT",
        f"Window: {'all history' if not cutoff else f'last {args.since_hours:g}h'}",
        "=" * 60,
        f"Submit attempts: {attempts}  accepted: {len(submits)}  misses: {misses}  "
        f"({hit_rate_submit:.1f}% fill rate)",
        f"Matched to REF would_enter: {len(rows)}  unmatched: {unmatched}",
        "",
    ]
    logged = summary["venue_fill_logged"]
    lines.append(f"Venue avg_fill_price logged: {logged}/{len(submits)} submits")
    if ratios:
        med = statistics.median(ratios)
        lines.append(
            f"LIVE touch / REF touch: median={med:.4f}  mean={statistics.mean(ratios):.4f}  "
            f"p10={pct(ratios, 0.1):.4f}  p90={pct(ratios, 0.9):.4f}  (target ~1.0)"
        )
    if venue_ratios:
        lines.append(
            f"Venue fill / REF touch: median={statistics.median(venue_ratios):.4f}  "
            f"mean={statistics.mean(venue_ratios):.4f}  "
            f"p10={pct(venue_ratios, 0.1):.4f}  p90={pct(venue_ratios, 0.9):.4f}  "
            f"(true slippage; target ~1.0)"
        )
    if venue_over_live:
        lines.append(
            f"Venue fill / LIVE touch: median={statistics.median(venue_over_live):.4f}  "
            f"mean={statistics.mean(venue_over_live):.4f}"
        )
    if limit_ratios:
        lines.append(
            f"REF marketable_limit / touch: median={statistics.median(limit_ratios):.4f}  "
            f"(sim book-walk ceiling)"
        )
    if by_band:
        lines.append("")
        lines.append("By REF touch band (median live/ref):")
        for band, v in sorted(by_band.items()):
            lines.append(f"  {band}: n={len(v)} median={statistics.median(v):.4f}")
    text = "\n".join(lines)
    print(text)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "fill_realization_audit.log").open("a").write(text + "\n\n")
    with (out_dir / "fill_realization_audit.jsonl").open("a") as f:
        f.write(json.dumps(summary) + "\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())