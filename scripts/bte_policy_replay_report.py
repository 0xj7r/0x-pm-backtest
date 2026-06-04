#!/usr/bin/env python3
"""Summarize BTE fixed-vs-policy replay artifacts by regime cluster.

The policy search emits a scale JSONL with the precomputed diagnostic cluster
for each held-out market. The real engine replay emits market PnL rows. This
script joins the two so we can validate whether the policy helped for the
intended clusters after a fresh replay.
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any


STRATEGY = "back_to_explore"


@dataclass
class Bucket:
    markets: int = 0
    traded: int = 0
    orders: int = 0
    pnl: float = 0.0
    worst: float | None = None
    best: float | None = None

    def add(self, pnl: float, orders: int) -> None:
        self.markets += 1
        if orders > 0:
            self.traded += 1
        self.orders += orders
        self.pnl += pnl
        self.worst = pnl if self.worst is None else min(self.worst, pnl)
        self.best = pnl if self.best is None else max(self.best, pnl)


def load_scales(path: Path) -> dict[tuple[str, int], dict[str, Any]]:
    out: dict[tuple[str, int], dict[str, Any]] = {}
    with path.open() as file:
        for line_no, line in enumerate(file, 1):
            if not line.strip():
                continue
            row = json.loads(line)
            key = (row["slug"], int(row["close_ts"]))
            if key in out:
                raise ValueError(f"duplicate scale key on line {line_no}: {key}")
            out[key] = row
    return out


def summarize_markets(
    path: Path,
    scales: dict[tuple[str, int], dict[str, Any]],
) -> tuple[Bucket, dict[str, Bucket], dict[float, Bucket], int]:
    total = Bucket()
    by_cluster: dict[str, Bucket] = defaultdict(Bucket)
    by_scale: dict[float, Bucket] = defaultdict(Bucket)
    missing_scales = 0
    with path.open() as file:
        for line in file:
            if not line.strip():
                continue
            row = json.loads(line)
            strat = row.get("per_strategy", {}).get(STRATEGY, {})
            pnl = float(strat.get("pnl_usdc", 0.0))
            orders = int(strat.get("orders_filled", 0))
            total.add(pnl, orders)

            scale_row = scales.get((row["slug"], int(row["close_ts"])))
            if scale_row is None:
                missing_scales += 1
                cluster = "missing_scale"
                scale = -1.0
            else:
                cluster = str(scale_row["diagnostic_cluster"])
                scale = float(scale_row["scale"])
            by_cluster[cluster].add(pnl, orders)
            by_scale[scale].add(pnl, orders)
    return total, dict(by_cluster), dict(by_scale), missing_scales


def fmt_money(value: float) -> str:
    return f"${value:,.2f}"


def bucket_row(name: str, bucket: Bucket) -> str:
    mean = bucket.pnl / bucket.markets if bucket.markets else 0.0
    worst = bucket.worst if bucket.worst is not None else 0.0
    best = bucket.best if bucket.best is not None else 0.0
    return (
        f"| {name} | {bucket.markets} | {bucket.traded} | {bucket.orders} | "
        f"{fmt_money(bucket.pnl)} | {fmt_money(mean)} | "
        f"{fmt_money(worst)} | {fmt_money(best)} |"
    )


def write_section(lines: list[str], title: str, buckets: dict[str, Bucket]) -> None:
    lines.append(f"## {title}")
    lines.append("")
    lines.append("| Bucket | Markets | Traded | Orders | PnL | Mean | Worst | Best |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|")
    for name, bucket in sorted(buckets.items(), key=lambda item: item[1].pnl, reverse=True):
        lines.append(bucket_row(name, bucket))
    lines.append("")


def build_report(args: argparse.Namespace) -> str:
    scales = load_scales(args.scales)
    lines = ["# BTE Policy Replay Report", ""]
    for label, path in (("fixed", args.fixed_markets), ("policy", args.policy_markets)):
        if path is None:
            continue
        total, by_cluster, by_scale, missing = summarize_markets(path, scales)
        lines.append(f"## {label.title()} Summary")
        lines.append("")
        lines.append(bucket_row("total", total))
        lines.append("")
        if missing:
            lines.append(f"Missing scale rows: `{missing}`")
            lines.append("")
        write_section(lines, f"{label.title()} By Cluster", by_cluster)
        write_section(
            lines,
            f"{label.title()} By Policy Scale",
            {str(scale): bucket for scale, bucket in by_scale.items()},
        )
    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--scales", required=True, type=Path)
    parser.add_argument("--fixed-markets", type=Path)
    parser.add_argument("--policy-markets", type=Path)
    parser.add_argument("--out-md", type=Path)
    args = parser.parse_args()
    if args.fixed_markets is None and args.policy_markets is None:
        parser.error("provide --fixed-markets and/or --policy-markets")
    report = build_report(args)
    if args.out_md is None:
        print(report, end="")
    else:
        args.out_md.parent.mkdir(parents=True, exist_ok=True)
        args.out_md.write_text(report)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
