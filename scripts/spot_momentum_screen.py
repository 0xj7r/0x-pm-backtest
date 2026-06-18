#!/usr/bin/env python3
"""Screen multi-horizon spot momentum at shadow would_enter vs resolution outcome.

Horizons: 60, 300, 600, 900s (1/5/10/15 min) from Binance 1m klines.
"""
from __future__ import annotations

import argparse
import glob
import json
import urllib.request
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

HORIZONS = (60, 300, 600, 900)


def load_jsonl(path: Path) -> list[dict]:
    return [json.loads(l) for l in path.read_text().splitlines() if l.strip()]


def build_res_index(shadow_dir: Path) -> dict[tuple[str, str, int], dict]:
    entries, resolutions = [], []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for ev in load_jsonl(Path(fp)):
            if ev.get("type") == "would_enter":
                entries.append(ev)
            elif ev.get("type") == "resolution":
                resolutions.append(ev)
    by_ent: dict[tuple[str, str], list] = defaultdict(list)
    for e in entries:
        by_ent[(e["slug"], e["side"])].append(e)
    for ents in by_ent.values():
        ents.sort(key=lambda x: x["ts_utc"])
    by_res: dict[tuple[str, str], list] = defaultdict(list)
    for r in resolutions:
        by_res[(r["slug"], r["side"])].append(r)
    for ress in by_res.values():
        ress.sort(key=lambda x: x["ts_utc"])
    out = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = res
    return out


def fetch_klines(start_ms: int, end_ms: int) -> list[tuple[int, float]]:
    out: list[tuple[int, float]] = []
    cursor = start_ms
    while cursor < end_ms:
        url = (
            "https://api.binance.com/api/v3/klines?"
            f"symbol=BTCUSDT&interval=1m&startTime={cursor}&endTime={end_ms}&limit=1000"
        )
        with urllib.request.urlopen(url, timeout=20) as resp:
            kl = json.loads(resp.read())
        if not kl:
            break
        for k in kl:
            out.append((int(k[0]), float(k[4])))
        cursor = int(kl[-1][0]) + 60_000
        if len(kl) < 1000:
            break
    return out


def ret_bps(klines: list[tuple[int, float]], end_ms: int, lookback_s: int) -> float | None:
    if not klines:
        return None
    target = end_ms - lookback_s * 1000
    end_px = None
    start_px = None
    for ts, px in klines:
        if ts <= end_ms:
            end_px = px
        if ts <= target:
            start_px = px
    if end_px is None or start_px is None or start_px <= 0:
        return None
    return (end_px / start_px - 1.0) * 1e4


def aligned(side: str, bps: float | None) -> bool | None:
    if bps is None:
        return None
    return bps > 0 if side == "up" else bps < 0


def conv_label(rets: dict[int, float | None], side: str) -> str | None:
    vals = [rets.get(h) for h in HORIZONS]
    if any(v is None for v in vals):
        return None
    ok = []
    for v in vals:
        bullish = v > 0
        ok.append(bullish if side == "up" else not bullish)
    if all(ok):
        return "conv_1_5_10_15m"
    if not any(ok):
        return "against_all"
    return "mixed"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--shadow-dir", default="/home/ubuntu/data/pm-alpha/shadow-final")
    ap.add_argument("--since", default="2026-06-16T12:10:00Z")
    ap.add_argument("--clip-usd", type=float, default=25.0)
    args = ap.parse_args()

    cutoff = datetime.fromisoformat(args.since.replace("Z", "+00:00"))
    res_idx = build_res_index(Path(args.shadow_dir))

    entries = []
    for fp in sorted(glob.glob(str(Path(args.shadow_dir) / "shadow-*.jsonl"))):
        for ev in load_jsonl(Path(fp)):
            if ev.get("type") != "would_enter":
                continue
            ts = datetime.fromisoformat(ev["ts_utc"].replace("Z", "+00:00"))
            if ts < cutoff:
                continue
            key = (ev["slug"], ev["side"], int(ev.get("clip", 1)))
            res = res_idx.get(key)
            if not res:
                continue
            entries.append((int(ts.timestamp() * 1000), ev, res))

    if not entries:
        print("no resolved entries")
        return 0

    start_ms = min(e[0] for e in entries) - 900_000
    end_ms = max(e[0] for e in entries) + 60_000
    klines = fetch_klines(start_ms, end_ms)
    print(f"# spot momentum — {len(entries)} entries, {len(klines)} 1m klines\n")

    enriched = []
    for end_ms_e, ev, res in entries:
        rets = {h: ret_bps(klines, end_ms_e, h) for h in HORIZONS}
        side = ev["side"]
        touch = float(ev.get("touch_price") or 0.5)
        won = bool(res.get("won"))
        gross = args.clip_usd / touch - args.clip_usd if won else -args.clip_usd
        enriched.append(
            {
                "side": side,
                "won": won,
                "gross": gross,
                "rets": rets,
                "align_60": aligned(side, rets.get(60)),
                "conv": conv_label(rets, side),
            }
        )

    def summarize(rows: list[dict], label: str) -> None:
        if not rows:
            return
        n = len(rows)
        w = sum(1 for r in rows if r["won"])
        g = sum(r["gross"] for r in rows)
        print(f"  {label:36s} n={n:4d} {w}W/{n-w}L  gross=${g:+.0f}  $/tr=${g/n:+.2f}")

    print("## 1/5/10/15m all agree with entry side")
    for label in ("conv_1_5_10_15m", "mixed", "against_all"):
        summarize([r for r in enriched if r["conv"] == label], label)

    print("\n## Per-horizon alignment")
    for h, name in [(60, "1m"), (300, "5m"), (600, "10m"), (900, "15m")]:
        for tag, pred in [("align", True), ("against", False)]:
            sub = [r for r in enriched if aligned(r["side"], r["rets"].get(h)) is pred]
            summarize(sub, f"{name} {tag}")

    print("\n## 1m against + longer horizons converging (fade the dip)")
    sub = [
        r
        for r in enriched
        if r["align_60"] is False
        and r["conv"] in ("conv_1_5_10_15m", "mixed")
        and all(
            aligned(r["side"], r["rets"].get(h)) is True
            for h in (300, 600, 900)
            if r["rets"].get(h) is not None
        )
    ]
    summarize(sub, "1m against + 5/10/15m conv")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())