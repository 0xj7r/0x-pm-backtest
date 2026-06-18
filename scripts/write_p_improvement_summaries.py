#!/usr/bin/env python3
"""Write summaries for p-improvement sweep variants and print comparison."""
import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "data/runs/p-improvement"
BASELINE_PATH = ROOT / "data/runs/analysis/shadow_baseline_summary.json"
VARIANTS = ["edge_014", "edge_016", "tail_010_25pct", "tail_005_25pct"]


def write_summary(tag: str) -> dict:
    report_path = OUT / f"{tag}.json"
    summary_path = OUT / f"{tag}_summary.json"
    with open(report_path) as f:
        r = json.load(f)
    entry = r["sweep"][-1]
    agg = entry["report"]["aggregate"]
    hc = r["harness_cfg"]
    out = {
        "variant": tag,
        "window": "VERIFY",
        "market": "btc5m",
        "date_start": r.get("date_start"),
        "date_end": r.get("date_end"),
        "latency_ms": entry["latency_ms"],
        "edge_threshold": entry["edge_threshold"],
        "notional_usdc": hc["notional_usdc"],
        "perp_price_weight": r["model_cfg"].get("perp_price_weight", 0.0),
        "tail_max_price": hc.get("tail_max_price", 0.0),
        "tail_frac": hc.get("tail_frac", 0.0),
        "NET": round(agg["total_pnl"], 2),
        "trades": agg["n_trades"],
        "hit": round(agg["hit_rate"], 4),
        "n_markets": agg["n_markets"],
    }
    with open(summary_path, "w") as f:
        json.dump(out, f, indent=2)
        f.write("\n")
    print(f"wrote {summary_path}: NET=${out['NET']:,.2f}")
    return out


def main() -> None:
    rows = [write_summary(tag) for tag in VARIANTS]
    with open(BASELINE_PATH) as f:
        baseline = json.load(f)
    base_net = baseline["NET"]

    print()
    print(
        f"{'variant':<18} {'edge':>6} {'tail':>8} {'NET':>12} "
        f"{'Δbase':>10} {'trades':>8} {'hit%':>7}"
    )
    print("-" * 72)
    for r in rows:
        tail = f"{r.get('tail_max_price', 0):.2f}" if r.get("tail_max_price", 0) else "off"
        delta = r["NET"] - base_net
        print(
            f"{r['variant']:<18} {r['edge_threshold']:>6.2f} {tail:>8} "
            f"${r['NET']:>10,.2f} {delta:>+10,.2f} {r['trades']:>8} {r['hit']*100:>6.1f}%"
        )
    print("-" * 72)
    print(
        f"{'shadow_baseline':<18} {baseline['edge_threshold']:>6.2f} {'off':>8} "
        f"${baseline['NET']:>10,.2f} {'+0.00':>10} {baseline['trades']:>8} "
        f"{baseline['hit']*100:>6.1f}%"
    )


if __name__ == "__main__":
    main()