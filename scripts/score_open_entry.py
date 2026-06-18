#!/usr/bin/env python3
"""Score open-entry sweep vs baseline on VERIFY/TUNE."""
from __future__ import annotations

import json
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / "data/runs/open-entry"


def worst_day(trades_path: Path) -> tuple[float, str]:
    by_day: dict[str, float] = defaultdict(float)
    if not trades_path.is_file():
        return 0.0, ""
    for line in trades_path.read_text().splitlines():
        if not line.strip():
            continue
        t = json.loads(line)
        ts = t.get("decision_ts_ns") or t.get("entry_ts_ns") or 0
        if not ts:
            continue
        day = datetime.fromtimestamp(ts / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")
        by_day[day] += float(t.get("pnl_net", t.get("pnl", 0)))
    if not by_day:
        return 0.0, ""
    worst_day, worst_pnl = min(by_day.items(), key=lambda kv: kv[1])
    return worst_pnl, worst_day


def open_mid_stats(trades_path: Path) -> dict:
    """First 5s entries at touch 0.45-0.55."""
    n = 0
    pnl = 0.0
    wins = 0
    if not trades_path.is_file():
        return {"n": 0, "net": 0.0, "hit": 0.0}
    for line in trades_path.read_text().splitlines():
        if not line.strip():
            continue
        t = json.loads(line)
        open_ns = t.get("open_ts_ns")
        ts = t.get("decision_ts_ns") or t.get("entry_ts_ns")
        if not open_ns or not ts:
            continue
        secs = (ts - open_ns) / 1e9
        px = float(t["avg_price"])
        if secs >= 5 or not (0.45 <= px <= 0.55):
            continue
        n += 1
        p = float(t["pnl"])
        pnl += p
        if p > 0:
            wins += 1
    return {"n": n, "net": round(pnl, 2), "hit": round(wins / n * 100, 1) if n else 0.0}


def main() -> None:
    out_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_OUT
    rows = []
    for path in sorted(out_dir.glob("*_summary.json")):
        row = json.loads(path.read_text())
        tag = row["variant"]
        trades = out_dir / f"{tag}.trades.jsonl"
        w, wd = worst_day(trades)
        row["worst_day"] = w
        row["worst_day_date"] = wd
        row["open_mid_0_5s"] = open_mid_stats(trades)
        rows.append(row)

    if not rows:
        print(f"No summaries in {out_dir}")
        sys.exit(0)

    base = next((r for r in rows if r["variant"] == "baseline"), rows[0])
    base_net = float(base["NET"])
    print(f"# Open-entry backtest — {out_dir}")
    print(f"# Baseline: NET=${base_net:,.2f} trades={base['trades']} hit={base['hit']*100:.1f}%")
    om = base.get("open_mid_0_5s", {})
    print(f"# Baseline 0-5s mid-touch: n={om.get('n',0)} NET=${om.get('net',0):,.2f} hit={om.get('hit',0)}%")
    print()
    hdr = (
        f"{'variant':<22} {'NET':>12} {'Δ%':>7} {'trades':>7} {'hit%':>6} "
        f"{'0-5s mid n':>10} {'0-5s $':>9} {'worst':>10}"
    )
    print(hdr)
    print("-" * len(hdr))
    rows.sort(key=lambda r: -float(r["NET"]))
    for r in rows:
        net = float(r["NET"])
        delta = (net - base_net) / abs(base_net) * 100 if base_net else 0
        om = r.get("open_mid_0_5s", {})
        print(
            f"{r['variant']:<22} ${net:>10,.2f} {delta:>+6.1f}% {int(r['trades']):>7} "
            f"{float(r['hit'])*100:>5.1f}% {om.get('n',0):>10} "
            f"${om.get('net',0):>8,.0f} ${float(r.get('worst_day',0)):>9,.0f}"
        )


if __name__ == "__main__":
    main()