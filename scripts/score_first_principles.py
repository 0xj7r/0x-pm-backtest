#!/usr/bin/env python3
"""Score first-principles discovery JSONs by strategy class."""

from __future__ import annotations

import json
import sys
from pathlib import Path


def load_pnl(path: Path) -> dict:
    d = json.loads(path.read_text())
    sweep = d.get("sweep") or []
    if not sweep:
        return {"net": 0.0, "trades": 0, "hit": 0.0}
    # pick best threshold cell by total_pnl
    best = max(sweep, key=lambda s: s.get("report", {}).get("cells", {}).get("BTC-300s", {}).get("total_pnl", 0))
    cell = best.get("report", {}).get("cells", {}).get("BTC-300s", {})
    return {
        "net": cell.get("total_pnl", 0.0),
        "trades": cell.get("n_trades", 0),
        "hit": cell.get("hit_rate", 0.0) * 100,
        "thr": best.get("edge_threshold"),
    }


def main() -> None:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else "data/runs/first_principles")
    rows = []
    for p in sorted(root.glob("*.json")):
        name = p.stem
        parts = name.split("_", 1)
        if len(parts) < 2:
            continue
        win, rest = parts[0], parts[1]
        s = load_pnl(p)
        verdict = "PASS" if s["net"] > 0 and s["hit"] > 50 else ("MARGINAL" if s["net"] > 0 else "FAIL")
        rows.append((win, rest, s["net"], s["trades"], s["hit"], verdict))

    print(f"# First-principles scorecard — {root}\n")
    print("| window | class | NET $ | trades | hit% | verdict |")
    print("|---|---|---:|---:|---:|---|")
    for win, rest, net, tr, hit, verdict in rows:
        print(f"| {win} | {rest} | {net:,.0f} | {tr} | {hit:.1f} | {verdict} |")


if __name__ == "__main__":
    main()