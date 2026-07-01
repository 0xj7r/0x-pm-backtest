#!/usr/bin/env python3
"""Classify fade backtest trades into thesis buckets and emit a markdown report.

Buckets (mutually exclusive, priority order):
  D_lottery  — p_side < 0.20 AND entry ask < 0.10
  C_fav_value — A-band favourite: 0.15 <= ask <= 0.85, p_side >= 0.25, ask >= 0.50
  A_lead     — A-band underdog:  0.15 <= ask <= 0.85, p_side >= 0.25, ask < 0.50
  B_tail     — ask < 0.15 OR p_side < 0.25 (excluding D_lottery)
  Other      — remainder (typically ask > 0.85 with p_side >= 0.25)

Edge decomposition per trade (chosen side):
  p_mid   = p_side - mid_side
  mid_ask = mid_side - entry_ask
  edge    = p_side - entry_ask = p_mid + mid_ask

Usage:
  python3 scripts/thesis_classify.py \\
    data/runs/analysis/shadow_match.trades.jsonl \\
    data/runs/analysis/champion_f1.trades.jsonl \\
    -o docs/research/strategy-hunt/06-thesis-decomposition.md
"""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
BUCKET_ORDER = ["A_lead", "B_tail", "C_fav_value", "D_lottery", "Other"]


def p_side(t: dict) -> float:
    p = float(t["p_exo"])
    return p if t["side"] == "Yes" else 1.0 - p


def mid_side(t: dict) -> float:
    m = float(t["mid_at_decision"])
    return m if t["side"] == "Yes" else 1.0 - m


def entry_ask(t: dict) -> float:
    return float(t.get("side_ask_at_decision") or t["avg_price"])


def classify_thesis(t: dict) -> str:
    ps = p_side(t)
    ask = entry_ask(t)

    if ps < 0.20 and ask < 0.10:
        return "D_lottery"
    if 0.15 <= ask <= 0.85 and ps >= 0.25:
        if ask >= 0.50:
            return "C_fav_value"
        return "A_lead"
    if ask < 0.15 or ps < 0.25:
        return "B_tail"
    return "Other"


def edge_parts(t: dict) -> tuple[float, float, float]:
    ps = p_side(t)
    ms = mid_side(t)
    ask = entry_ask(t)
    pm = ps - ms
    ma = ms - ask
    return pm, ma, ps - ask


@dataclass
class BucketStats:
    n: int = 0
    net: float = 0.0
    hits: int = 0
    p_mid_sum: float = 0.0
    mid_ask_sum: float = 0.0
    edge_sum: float = 0.0

    @property
    def hit_pct(self) -> float:
        return 100.0 * self.hits / self.n if self.n else 0.0

    @property
    def per_trade(self) -> float:
        return self.net / self.n if self.n else 0.0

    @property
    def mean_p_mid(self) -> float:
        return self.p_mid_sum / self.n if self.n else 0.0

    @property
    def mean_mid_ask(self) -> float:
        return self.mid_ask_sum / self.n if self.n else 0.0

    @property
    def mean_edge(self) -> float:
        return self.edge_sum / self.n if self.n else 0.0


def load_trades(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def accumulate(trades: list[dict]) -> dict[str, BucketStats]:
    stats: dict[str, BucketStats] = {b: BucketStats() for b in BUCKET_ORDER}
    for t in trades:
        bucket = classify_thesis(t)
        s = stats[bucket]
        s.n += 1
        s.net += float(t["pnl"])
        if t.get("won"):
            s.hits += 1
        pm, ma, edge = edge_parts(t)
        s.p_mid_sum += pm
        s.mid_ask_sum += ma
        s.edge_sum += edge
    return stats


def summarize_all(trades: list[dict]) -> BucketStats:
    s = BucketStats()
    for t in trades:
        s.n += 1
        s.net += float(t["pnl"])
        if t.get("won"):
            s.hits += 1
        pm, ma, edge = edge_parts(t)
        s.p_mid_sum += pm
        s.mid_ask_sum += ma
        s.edge_sum += edge
    return s


def fmt_money(x: float) -> str:
    return f"${x:,.0f}" if abs(x) >= 100 else f"${x:,.2f}"


def render_run_section(path: Path, trades: list[dict]) -> list[str]:
    stats = accumulate(trades)
    total = summarize_all(trades)
    lines = [f"## {path.name}", "", f"Source: `{path}` — **{len(trades):,}** trades.", ""]
    lines.append("| bucket | trades | NET | hit% | $/trade | mean p−mid | mean mid−ask | mean edge |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|")
    for bucket in BUCKET_ORDER:
        s = stats[bucket]
        if s.n == 0:
            continue
        share = 100.0 * s.n / total.n if total.n else 0.0
        lines.append(
            f"| {bucket} | {s.n:,} ({share:.1f}%) | {fmt_money(s.net)} | {s.hit_pct:.1f} | "
            f"{s.per_trade:+.2f} | {s.mean_p_mid:+.4f} | {s.mean_mid_ask:+.4f} | "
            f"{s.mean_edge:+.4f} |"
        )
    lines.append(
        f"| **ALL** | {total.n:,} | {fmt_money(total.net)} | {total.hit_pct:.1f} | "
        f"{total.per_trade:+.2f} | {total.mean_p_mid:+.4f} | {total.mean_mid_ask:+.4f} | "
        f"{total.mean_edge:+.4f} |"
    )
    lines.append("")
    lines.append(
        "_Edge decomposition: `edge = p_side − ask = (p_side − mid_side) + (mid_side − ask)`._"
    )
    lines.append(
        "_`p−mid` is belief vs book mid; `mid−ask` is the spread/slippage cushion at entry._"
    )
    lines.append("")
    return lines


def render_summary_table(runs: list[tuple[Path, list[dict]]]) -> list[str]:
    lines = [
        "## Summary (all runs)",
        "",
        "| run | bucket | trades | NET | hit% | $/trade | mean p−mid | mean mid−ask |",
        "|---|---|---:|---:|---:|---:|---:|---:|",
    ]
    for path, trades in runs:
        stats = accumulate(trades)
        label = path.stem.replace(".trades", "")
        for bucket in BUCKET_ORDER:
            s = stats[bucket]
            if s.n == 0:
                continue
            lines.append(
                f"| {label} | {bucket} | {s.n:,} | {fmt_money(s.net)} | {s.hit_pct:.1f} | "
                f"{s.per_trade:+.2f} | {s.mean_p_mid:+.4f} | {s.mean_mid_ask:+.4f} |"
            )
        total = summarize_all(trades)
        lines.append(
            f"| {label} | **ALL** | {total.n:,} | {fmt_money(total.net)} | {total.hit_pct:.1f} | "
            f"{total.per_trade:+.2f} | {total.mean_p_mid:+.4f} | {total.mean_mid_ask:+.4f} |"
        )
    lines.append("")
    return lines


def render_report(paths: list[Path], out: Path) -> str:
    runs = [(p, load_trades(p)) for p in paths]
    lines = [
        "# Thesis Decomposition — Fade Backtest Trades",
        "",
        "Classifies each fade trade into mechanistic thesis buckets and decomposes",
        "entry edge into belief-vs-mid (`p−mid`) and mid-vs-ask (`mid−ask`) components.",
        "",
        "## Bucket definitions",
        "",
        "| ID | Rule | Interpretation |",
        "|---|---|---|",
        "| **A_lead** | `0.15 ≤ ask ≤ 0.85`, `p_side ≥ 0.25`, `ask < 0.50` | Core fade on underdog side — stale book vs exo belief |",
        "| **B_tail** | `ask < 0.15` OR `p_side < 0.25` (not D) | Cheap / low-probability entries outside core band |",
        "| **C_fav_value** | A-band + `ask ≥ 0.50` | Favourite-side fade: book underprices likely winner |",
        "| **D_lottery** | `p_side < 0.20` AND `ask < 0.10` | Deep OOTM lottery tickets |",
        "| **Other** | Remainder | Typically heavy favourite (`ask > 0.85`) with adequate `p_side` |",
        "",
        f"Generated by `scripts/thesis_classify.py` on {len(paths)} trade logs.",
        "",
    ]
    lines.extend(render_summary_table(runs))
    for path, trades in runs:
        lines.extend(render_run_section(path, trades))

    # Cross-run bucket comparison (pooled edge means weighted by trade count)
    lines.append("## Pooled edge mix by bucket")
    lines.append("")
    pooled: dict[str, list[dict]] = defaultdict(list)
    for _, trades in runs:
        for t in trades:
            pooled[classify_thesis(t)].append(t)
    lines.append("| bucket | pooled trades | NET | hit% | $/trade | mean p−mid | mean mid−ask | p−mid share of edge |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|")
    for bucket in BUCKET_ORDER:
        trades = pooled[bucket]
        if not trades:
            continue
        s = summarize_all(trades)
        edge = s.mean_edge
        pm_share = 100.0 * s.mean_p_mid / edge if abs(edge) > 1e-9 else 0.0
        lines.append(
            f"| {bucket} | {s.n:,} | {fmt_money(s.net)} | {s.hit_pct:.1f} | "
            f"{s.per_trade:+.2f} | {s.mean_p_mid:+.4f} | {s.mean_mid_ask:+.4f} | {pm_share:.1f}% |"
        )
    lines.append("")

    text = "\n".join(lines) + "\n"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text)
    return text


def print_console_summary(paths: list[Path]) -> None:
    for path in paths:
        trades = load_trades(path)
        stats = accumulate(trades)
        total = summarize_all(trades)
        print(f"\n# {path.name}  n={len(trades):,}  NET={fmt_money(total.net)}  hit={total.hit_pct:.1f}%")
        print(f"{'bucket':<14} {'n':>6} {'NET':>12} {'hit%':>7} {'$/tr':>8} {'p-mid':>8} {'mid-ask':>8}")
        for bucket in BUCKET_ORDER:
            s = stats[bucket]
            if s.n == 0:
                continue
            print(
                f"{bucket:<14} {s.n:6d} {fmt_money(s.net):>12} {s.hit_pct:6.1f}% "
                f"{s.per_trade:+8.2f} {s.mean_p_mid:+8.4f} {s.mean_mid_ask:+8.4f}"
            )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trade_files", nargs="+", type=Path, help="*.trades.jsonl inputs")
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        default=Path("docs/research/strategy-hunt/06-thesis-decomposition.md"),
        help="Markdown report path",
    )
    args = parser.parse_args()

    for p in args.trade_files:
        if not p.exists():
            raise SystemExit(f"missing trade file: {p}")

    render_report(args.trade_files, args.output)
    print_console_summary(args.trade_files)
    print(f"\nWrote {args.output}")


if __name__ == "__main__":
    main()