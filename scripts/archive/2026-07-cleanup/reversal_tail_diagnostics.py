#!/usr/bin/env python3
"""Diagnose recent reversal losses and convex-tail coverage from market JSONL."""

from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path
from typing import Any


TAGS = (
    "br2_late_favourite_load",
    "br2_late_confirm",
    "br2_high_skew_load",
    "br2_convex_tail",
)


def fill_pnl(fill: dict[str, Any], yes_resolved: bool) -> float:
    side = fill.get("side")
    wins = (side == "BuyYes" and yes_resolved) or (side == "BuyNo" and not yes_resolved)
    payout = float(fill.get("shares") or 0.0) if wins else 0.0
    return payout - float(fill.get("notional") or 0.0) + float(fill.get("rebate_usdc") or 0.0)


def fill_won(fill: dict[str, Any], yes_resolved: bool) -> bool:
    side = fill.get("side")
    return (side == "BuyYes" and yes_resolved) or (side == "BuyNo" and not yes_resolved)


def final_range_bucket(volatility_range: float) -> str:
    if volatility_range < 0.78:
        return "final_range_lt_078"
    if volatility_range < 0.93:
        return "final_range_078_093_mid_wide"
    if volatility_range < 0.97:
        return "final_range_093_097"
    return "final_range_ge_097"


def live_regime_label(fill: dict[str, Any]) -> str:
    observed_range = float(fill.get("market_yes_range_so_far") or 0.0)
    whipsaw = float(fill.get("regime_whipsaw_score") or 0.0)
    path_eff = float(fill.get("regime_path_efficiency") or 0.0)
    reversal = float(fill.get("regime_reversal_pressure") or 0.0)
    sign_flip = float(fill.get("regime_sign_flip_rate") or 0.0)

    if 0.40 <= observed_range < 0.65 and (sign_flip >= 0.40 or path_eff <= 0.15):
        return "expanded_not_decisive"
    if observed_range >= 0.65 and whipsaw >= 0.30:
        return "expanded_chop"
    if reversal >= 0.34 or sign_flip >= 0.45:
        return "reversal_pressure"
    if observed_range >= 0.65:
        return "expanded_continuation"
    return "neutral"


def post_fill_path(fill: dict[str, Any]) -> dict[str, Any] | None:
    path = fill.get("post_fill_path")
    return path if isinstance(path, dict) else None


def post_fill_bucket(fill: dict[str, Any]) -> str:
    path = post_fill_path(fill)
    if path is None:
        return "post_fill_path_unavailable"
    adverse = float(path.get("adverse_excursion") or 0.0)
    final_side_mid = float(path.get("final_side_mid") or 0.0)
    crossed_mid = bool(path.get("crossed_mid_after_fill"))
    if crossed_mid:
        return "crossed_mid_after_fill"
    if adverse >= 0.20 and final_side_mid < 0.60:
        return "large_adverse_then_soft_finish"
    if adverse >= 0.10:
        return "moderate_adverse_excursion"
    return "held_side"


def add(acc: dict[str, float], pnl: float, fill: dict[str, Any], won: bool) -> None:
    acc["fills"] += 1
    acc["pnl"] += pnl
    acc["cost"] += float(fill.get("notional") or 0.0)
    acc["wins"] += 1 if won else 0


def fmt_usd(value: float) -> str:
    return f"${value:,.2f}"


def write_table(lines: list[str], title: str, rows: list[tuple[str, str, dict[str, float]]]) -> None:
    lines.append(f"## {title}")
    lines.append("")
    lines.append("| Tag | Bucket | Fills | PnL | Cost | Win Rate |")
    lines.append("|---|---|---:|---:|---:|---:|")
    for tag, bucket, acc in rows:
        fills = int(acc["fills"])
        win_rate = acc["wins"] / fills if fills else 0.0
        lines.append(
            f"| {tag} | {bucket} | {fills} | {fmt_usd(acc['pnl'])} | "
            f"{fmt_usd(acc['cost'])} | {win_rate:.2%} |"
        )
    lines.append("")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--markets", required=True, help="market results JSONL")
    parser.add_argument("--last-markets", type=int, default=8633)
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    rows: list[dict[str, Any]] = []
    with Path(args.markets).open() as file:
        for line in file:
            if line.strip():
                rows.append(json.loads(line))
    rows.sort(key=lambda row: int(row.get("close_ts") or 0))
    window = rows[-args.last_markets :] if args.last_markets > 0 else rows

    by_final_range: dict[tuple[str, str], dict[str, float]] = defaultdict(lambda: defaultdict(float))
    by_live_regime: dict[tuple[str, str], dict[str, float]] = defaultdict(lambda: defaultdict(float))
    by_post_fill: dict[tuple[str, str], dict[str, float]] = defaultdict(lambda: defaultdict(float))
    market_rows: list[dict[str, Any]] = []

    for row in window:
        strat = (row.get("per_strategy") or {}).get("bonereaper_v2") or {}
        yes_resolved = bool(strat.get("yes_resolved"))
        fills = strat.get("fills_detail") or []
        market = {
            "slug": row.get("slug"),
            "volatility_range": float(row.get("volatility_range") or 0.0),
            "total_pnl": 0.0,
            "late_fav_pnl": 0.0,
            "late_confirm_pnl": 0.0,
            "high_skew_pnl": 0.0,
            "tail_pnl": 0.0,
            "late_fav_cost": 0.0,
            "tail_cost": 0.0,
            "late_fav_fills": 0,
            "late_fav_wins": 0,
            "tail_wins": 0,
        }
        for fill in fills:
            tag = fill.get("tag")
            pnl = fill_pnl(fill, yes_resolved)
            won = fill_won(fill, yes_resolved)
            market["total_pnl"] += pnl
            if tag in TAGS:
                add(
                    by_final_range[(tag, final_range_bucket(market["volatility_range"]))],
                    pnl,
                    fill,
                    won,
                )
                add(by_live_regime[(tag, live_regime_label(fill))], pnl, fill, won)
                add(by_post_fill[(tag, post_fill_bucket(fill))], pnl, fill, won)
            if tag == "br2_late_favourite_load":
                market["late_fav_pnl"] += pnl
                market["late_fav_cost"] += float(fill.get("notional") or 0.0)
                market["late_fav_fills"] += 1
                market["late_fav_wins"] += 1 if won else 0
            elif tag == "br2_late_confirm":
                market["late_confirm_pnl"] += pnl
            elif tag == "br2_high_skew_load":
                market["high_skew_pnl"] += pnl
            elif tag == "br2_convex_tail":
                market["tail_pnl"] += pnl
                market["tail_cost"] += float(fill.get("notional") or 0.0)
                market["tail_wins"] += 1 if won else 0
        if fills:
            market_rows.append(market)

    losing_late_fav = [row for row in market_rows if row["late_fav_pnl"] < 0.0]
    tail_cost = sum(row["tail_cost"] for row in market_rows)
    late_fav_loss_cost = sum(row["late_fav_cost"] for row in losing_late_fav)

    lines: list[str] = [
        "# Reversal And Tail Diagnostics",
        "",
        f"Source: `{args.markets}`",
        f"Window: last `{len(window)}` markets.",
        "",
        "Definitions:",
        "",
        "- `final_range_078_093_mid_wide` is a post-hoc bucket using full resolved-market YES-mid range.",
        "- `expanded_not_decisive` is live-safe: observed range `0.40..0.65` plus sign flips or low path efficiency.",
        "",
        "Summary:",
        "",
        f"- Active markets: `{len(market_rows)}`",
        f"- Total PnL: `{fmt_usd(sum(row['total_pnl'] for row in market_rows))}`",
        f"- Late-favourite losing markets: `{len(losing_late_fav)}`",
        f"- Late-favourite losing-market cost: `{fmt_usd(late_fav_loss_cost)}`",
        f"- Tail premium in late-favourite losing markets: `{fmt_usd(sum(row['tail_cost'] for row in losing_late_fav))}`",
        f"- Tail premium / late-favourite losing cost: `{(sum(row['tail_cost'] for row in losing_late_fav) / late_fav_loss_cost if late_fav_loss_cost else 0.0):.2%}`",
        f"- Total tail premium: `{fmt_usd(tail_cost)}`",
        f"- Total tail PnL: `{fmt_usd(sum(row['tail_pnl'] for row in market_rows))}`",
        "",
    ]

    write_table(lines, "Final Range Buckets", [(k[0], k[1], v) for k, v in sorted(by_final_range.items())])
    write_table(lines, "Live-Safe Regime Labels", [(k[0], k[1], v) for k, v in sorted(by_live_regime.items())])
    write_table(lines, "Post-Fill Path Labels", [(k[0], k[1], v) for k, v in sorted(by_post_fill.items())])

    lines.append("## Worst Markets")
    lines.append("")
    lines.append("| Rank | Slug | PnL | Range | Late Fav | Late Confirm | High Skew | Tail | Tail/Fav Cost | Late Fav Wins |")
    lines.append("|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for rank, row in enumerate(sorted(market_rows, key=lambda item: item["total_pnl"])[:12], 1):
        coverage = row["tail_cost"] / row["late_fav_cost"] if row["late_fav_cost"] else 0.0
        lines.append(
            f"| {rank} | {row['slug']} | {fmt_usd(row['total_pnl'])} | {row['volatility_range']:.3f} | "
            f"{fmt_usd(row['late_fav_pnl'])} | {fmt_usd(row['late_confirm_pnl'])} | "
            f"{fmt_usd(row['high_skew_pnl'])} | {fmt_usd(row['tail_pnl'])} | "
            f"{coverage:.2%} | {int(row['late_fav_wins'])}/{int(row['late_fav_fills'])} |"
        )
    lines.append("")

    out_path = Path(args.out_md)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
