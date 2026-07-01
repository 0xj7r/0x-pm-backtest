#!/usr/bin/env python3
"""Decompose fade backtest PnL by entry price and model p_side.

Answers: how much VERIFY PnL comes from tail entries (low p, cheap ask)?
Simulates min_p_side / max_entry_ask gates without re-running alpha.

Usage:
  python3 scripts/fade_entry_decompose.py data/runs/analysis/shadow_match.trades.jsonl
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from pathlib import Path


def p_side(t: dict) -> float:
    p = float(t["p_exo"])
    return p if t["side"] == "Yes" else 1.0 - p


def load(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def bucket_price(px: float) -> str:
    if px < 0.10:
        return "tail_<0.10"
    if px < 0.25:
        return "low_0.10-0.25"
    if px < 0.45:
        return "mid_0.25-0.45"
    if px < 0.70:
        return "core_0.45-0.70"
    if px < 0.85:
        return "fav_0.70-0.85"
    return "heavy_>=0.85"


def bucket_p(p: float) -> str:
    if p < 0.20:
        return "p_<0.20"
    if p < 0.35:
        return "p_0.20-0.35"
    if p < 0.50:
        return "p_0.35-0.50"
    if p < 0.65:
        return "p_0.50-0.65"
    return "p_>=0.65"


def summarize(trades: list[dict], label: str = "") -> None:
    n = len(trades)
    pnl = sum(t["pnl"] for t in trades)
    hit = sum(1 for t in trades if t["pnl"] > 0) / n if n else 0.0
    prefix = f"[{label}] " if label else ""
    print(f"{prefix}n={n:5d}  NET=${pnl:10.2f}  hit={hit*100:5.1f}%  $/tr={pnl/n if n else 0:6.2f}")


def secs_from_open(t: dict) -> float | None:
    open_ns = t.get("open_ts_ns")
    ts = t.get("decision_ts_ns") or t.get("entry_ts_ns")
    if not open_ns or not ts:
        return None
    return (ts - open_ns) / 1e9


def main() -> None:
    path = Path(sys.argv[1])
    trades = load(path)
    print(f"# Fade entry decomposition — {path}\n")
    summarize(trades, "ALL")

    print("\n## By seconds-from-open × entry price")
    heat: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for t in trades:
        s = secs_from_open(t)
        if s is None:
            continue
        if s < 5:
            tb = "0-5s"
        elif s < 30:
            tb = "5-30s"
        elif s < 60:
            tb = "30-60s"
        else:
            tb = "60s+"
        heat[(tb, bucket_price(float(t["avg_price"])))].append(t)
    print(f"{'offset':<10} {'price':<16} {'n':>6} {'NET':>11} {'hit%':>6} {'$/tr':>7}")
    for tb in ["0-5s", "5-30s", "30-60s", "60s+"]:
        for pb in [
            "tail_<0.10",
            "low_0.10-0.25",
            "mid_0.25-0.45",
            "core_0.45-0.70",
            "fav_0.70-0.85",
            "heavy_>=0.85",
        ]:
            sub = heat.get((tb, pb), [])
            if not sub:
                continue
            n = len(sub)
            pnl = sum(t["pnl"] for t in sub)
            hit = sum(1 for t in sub if t["pnl"] > 0) / n * 100
            print(f"{tb:<10} {pb:<16} {n:>6} ${pnl:>9,.0f} {hit:>5.1f}% {pnl/n:>7.2f}")

    print("\n## By entry price (avg_price)")
    by_px = defaultdict(list)
    for t in trades:
        by_px[bucket_price(float(t["avg_price"]))].append(t)
    for k in sorted(by_px, key=lambda x: ["tail_<0.10", "low_0.10-0.25", "mid_0.25-0.45",
                                           "core_0.45-0.70", "fav_0.70-0.85", "heavy_>=0.85"].index(x)):
        summarize(by_px[k], k)

    print("\n## By model p_side")
    by_p = defaultdict(list)
    for t in trades:
        by_p[bucket_p(p_side(t))].append(t)
    for k in sorted(by_p, key=lambda x: ["p_<0.20", "p_0.20-0.35", "p_0.35-0.50",
                                          "p_0.50-0.65", "p_>=0.65"].index(x)):
        summarize(by_p[k], k)

    print("\n## Tail flag: p_side < 0.35 OR entry < 0.15")
    tail = [t for t in trades if p_side(t) < 0.35 or float(t["avg_price"]) < 0.15]
    core = [t for t in trades if t not in tail]
    summarize(tail, "TAIL (would gate)")
    summarize(core, "CORE (would keep)")

    print("\n## Simulated gates (counterfactual NET)")
    gates = [
        ("baseline", lambda t: True),
        ("min_p_side>=0.25", lambda t: p_side(t) >= 0.25),
        ("min_p_side>=0.35", lambda t: p_side(t) >= 0.35),
        ("max_entry_ask<=0.85", lambda t: float(t["avg_price"]) <= 0.85),
        ("entry>=0.15", lambda t: float(t["avg_price"]) >= 0.15),
        ("entry>=0.15 & p>=0.25", lambda t: float(t["avg_price"]) >= 0.15 and p_side(t) >= 0.25),
        ("entry>=0.15 & p>=0.35", lambda t: float(t["avg_price"]) >= 0.15 and p_side(t) >= 0.35),
        ("fav_mid: entry>=0.45", lambda t: float(t["avg_price"]) >= 0.45),
    ]
    print("| gate | trades | NET $ | hit% | $/tr |")
    print("|---|---:|---:|---:|---:|")
    for name, pred in gates:
        sub = [t for t in trades if pred(t)]
        n = len(sub)
        pnl = sum(t["pnl"] for t in sub)
        hit = sum(1 for t in sub if t["pnl"] > 0) / n * 100 if n else 0
        per = pnl / n if n else 0
        print(f"| {name} | {n} | {pnl:,.0f} | {hit:.1f} | {per:.2f} |")

    print("\n## Examples: tail entries with positive edge at decision")
    examples = sorted(
        [t for t in trades if float(t["avg_price"]) < 0.15 and p_side(t) < 0.35],
        key=lambda t: -t["pnl"],
    )[:8]
    for t in examples:
        ps = p_side(t)
        edge = ps - float(t["avg_price"])
        print(
            f"  p_side={ps:.3f} px={t['avg_price']:.3f} edge={edge:.3f} "
            f"pnl=${t['pnl']:+.2f} won={t['won']} sigma={t.get('sigma_bar_bps', 0):.1f}"
        )


if __name__ == "__main__":
    main()