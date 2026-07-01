#!/usr/bin/env python3
"""Report BTC5m regime evolution using post-fill path labels."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any, Iterable


LANES = (
    "br2_late_favourite_load",
    "br2_late_confirm",
    "br2_high_skew_load",
    "br2_convex_tail",
)


def close_ts(row: dict[str, Any]) -> int:
    if row.get("close_ts") is not None:
        return int(row["close_ts"])
    return int(str(row.get("slug") or "").rsplit("-", 1)[1]) + 300


def strategy(row: dict[str, Any], name: str) -> dict[str, Any]:
    return ((row.get("per_strategy") or {}).get(name)) or {}


def yes_resolved(row: dict[str, Any], strat: dict[str, Any]) -> bool:
    if "yes_resolved" in strat:
        return bool(strat["yes_resolved"])
    return str(row.get("outcome_label") or "").lower() in ("yes", "up")


def fill_won(fill: dict[str, Any], resolved_yes: bool) -> bool:
    side = str(fill.get("side") or "")
    return (side == "BuyYes" and resolved_yes) or (side == "BuyNo" and not resolved_yes)


def fill_pnl(fill: dict[str, Any], resolved_yes: bool) -> float:
    won = fill_won(fill, resolved_yes)
    shares = float(fill.get("shares") or 0.0)
    notional = float(fill.get("notional") or 0.0)
    rebate = float(fill.get("rebate_usdc") or 0.0)
    return (shares if won else 0.0) - notional + rebate


def final_range_bucket(value: float) -> str:
    if value < 0.50:
        return "range_lt_050"
    if value < 0.78:
        return "range_050_078"
    if value < 0.93:
        return "range_078_093_midwide"
    if value < 0.97:
        return "range_093_097"
    return "range_ge_097_extreme"


def observed_range_bucket(value: float) -> str:
    if value < 0.30:
        return "obs_lt_030"
    if value < 0.40:
        return "obs_030_040"
    if value < 0.50:
        return "obs_040_050"
    if value < 0.65:
        return "obs_050_065"
    return "obs_ge_065"


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


def postfill_bucket(fill: dict[str, Any]) -> str:
    path = fill.get("post_fill_path")
    if not isinstance(path, dict):
        return "path_missing"
    adverse = float(path.get("adverse_excursion") or 0.0)
    final_side_mid = float(path.get("final_side_mid") or 0.0)
    if bool(path.get("crossed_mid_after_fill")):
        return "crossed_mid_after_fill"
    if adverse >= 0.20 and final_side_mid < 0.60:
        return "large_adverse_soft_finish"
    if adverse >= 0.10:
        return "moderate_adverse_excursion"
    return "held_side"


def path_value(fill: dict[str, Any], key: str) -> float:
    path = fill.get("post_fill_path")
    if not isinstance(path, dict):
        return 0.0
    return float(path.get(key) or 0.0)


def new_acc() -> dict[str, float]:
    return defaultdict(float)


def add_market(acc: dict[str, float], pnl: float, fills: list[dict[str, Any]]) -> None:
    acc["markets"] += 1
    acc["active_markets"] += 1 if fills else 0
    acc["pnl"] += pnl


def add_fill(acc: dict[str, float], fill: dict[str, Any], pnl: float, won: bool) -> None:
    acc["fills"] += 1
    acc["pnl"] += pnl
    acc["cost"] += float(fill.get("notional") or 0.0)
    acc["wins"] += 1 if won else 0
    acc["crossed_mid"] += 1 if postfill_bucket(fill) == "crossed_mid_after_fill" else 0
    acc["adverse_sum"] += path_value(fill, "adverse_excursion")
    acc["final_side_mid_sum"] += path_value(fill, "final_side_mid")
    model_p = fill.get("side_model_p")
    if model_p is not None:
        p = min(max(float(model_p), 1e-5), 1.0 - 1e-5)
        acc["model_samples"] += 1
        acc["model_p_sum"] += p
        acc["brier_sum"] += (p - (1.0 if won else 0.0)) ** 2
        if won:
            acc["logloss_sum"] += -math_log(p)
        else:
            acc["logloss_sum"] += -math_log(1.0 - p)


def add_daily_path_fill(acc: dict[str, float], fill: dict[str, Any], pnl: float, row: dict[str, Any]) -> None:
    post_bucket = postfill_bucket(fill)
    final_range = float(row.get("volatility_range") or 0.0)
    observed_range = float(fill.get("market_yes_range_so_far") or 0.0)
    model_p = fill.get("side_model_p")

    acc["fills"] += 1
    acc["pnl"] += pnl
    acc["cost"] += float(fill.get("notional") or 0.0)
    acc["observed_range_sum"] += observed_range
    acc["final_range_sum"] += final_range
    acc["adverse_sum"] += path_value(fill, "adverse_excursion")
    if model_p is not None:
        acc["model_samples"] += 1
        acc["model_p_sum"] += float(model_p)
    if post_bucket == "crossed_mid_after_fill":
        acc["crossed_mid"] += 1
        acc["crossed_mid_pnl"] += pnl
        acc["crossed_mid_cost"] += float(fill.get("notional") or 0.0)
    else:
        acc["non_crossed_pnl"] += pnl
    if 0.78 <= final_range < 0.93:
        acc["midwide_fills"] += 1
        acc["midwide_pnl"] += pnl
    if live_regime_label(fill) == "expanded_not_decisive":
        acc["expanded_not_decisive_fills"] += 1
        acc["expanded_not_decisive_pnl"] += pnl


def math_log(value: float) -> float:
    return math.log(value)


def fmt_money(value: float) -> str:
    return f"${value:,.2f}"


def fmt_pct(value: float) -> str:
    return f"{value:.2%}"


def render_fill_table(lines: list[str], title: str, rows: Iterable[tuple[str, dict[str, float]]]) -> None:
    lines.extend(
        [
            f"## {title}",
            "",
            "| Bucket | Fills | PnL | Cost | Win Rate | Cross-Mid Rate | Avg Adverse | Log Loss | Brier |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for label, acc in rows:
        fills = int(acc["fills"])
        if fills == 0:
            continue
        model_samples = int(acc["model_samples"])
        lines.append(
            f"| {label} | {fills} | {fmt_money(acc['pnl'])} | {fmt_money(acc['cost'])} | "
            f"{fmt_pct(acc['wins'] / fills)} | {fmt_pct(acc['crossed_mid'] / fills)} | "
            f"{acc['adverse_sum'] / fills:.4f} | "
            f"{acc['logloss_sum'] / model_samples if model_samples else 0.0:.4f} | "
            f"{acc['brier_sum'] / model_samples if model_samples else 0.0:.4f} |"
        )
    lines.append("")


def render_market_table(lines: list[str], title: str, rows: Iterable[tuple[str, dict[str, float]]]) -> None:
    lines.extend(
        [
            f"## {title}",
            "",
            "| Bucket | Markets | Active | PnL | Active Rate |",
            "|---|---:|---:|---:|---:|",
        ]
    )
    for label, acc in rows:
        markets = int(acc["markets"])
        if markets == 0:
            continue
        lines.append(
            f"| {label} | {markets} | {int(acc['active_markets'])} | "
            f"{fmt_money(acc['pnl'])} | {fmt_pct(acc['active_markets'] / markets)} |"
        )
    lines.append("")


def render_daily_path_table(lines: list[str], title: str, rows: Iterable[tuple[str, dict[str, float]]]) -> None:
    lines.extend(
        [
            f"## {title}",
            "",
            "| Day | Fills | PnL | Cross-Mid Fills | Cross-Mid Rate | Cross-Mid PnL | Non-Cross PnL | Mid-Wide Fills | Mid-Wide PnL | Expanded Not-Decisive Fills | Avg Obs Range | Avg Final Range | Avg Adverse | Avg Model P |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for label, acc in rows:
        fills = int(acc["fills"])
        if fills == 0:
            continue
        model_samples = int(acc["model_samples"])
        lines.append(
            f"| {label} | {fills} | {fmt_money(acc['pnl'])} | {int(acc['crossed_mid'])} | "
            f"{fmt_pct(acc['crossed_mid'] / fills)} | {fmt_money(acc['crossed_mid_pnl'])} | "
            f"{fmt_money(acc['non_crossed_pnl'])} | {int(acc['midwide_fills'])} | "
            f"{fmt_money(acc['midwide_pnl'])} | {int(acc['expanded_not_decisive_fills'])} | "
            f"{acc['observed_range_sum'] / fills:.3f} | {acc['final_range_sum'] / fills:.3f} | "
            f"{acc['adverse_sum'] / fills:.3f} | "
            f"{acc['model_p_sum'] / model_samples if model_samples else 0.0:.3f} |"
        )
    lines.append("")


def period_label(index: int, total: int) -> str:
    if index < total / 3:
        return "first_third"
    if index < 2 * total / 3:
        return "middle_third"
    return "last_third"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--source-label")
    parser.add_argument("--recent-days", type=int, default=30)
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    rows = [json.loads(line) for line in Path(args.markets_jsonl).open() if line.strip()]
    rows.sort(key=close_ts)
    if not rows:
        raise RuntimeError("no markets found")

    max_ts = close_ts(rows[-1])
    recent_start = max_ts - args.recent_days * 86400 + 300

    by_period_market: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_day_market: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_lane: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_period_lane: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_postfill: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_range: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_live: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_observed: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_recent: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_day_path: dict[str, dict[str, float]] = defaultdict(new_acc)
    by_day_lane_path: dict[str, dict[str, float]] = defaultdict(new_acc)
    worst_crossed: list[dict[str, Any]] = []
    missing_paths = 0

    for i, row in enumerate(rows):
        strat = strategy(row, args.strategy)
        fills = strat.get("fills_detail") or []
        pnl = float(strat.get("pnl_usdc") or 0.0)
        ts = close_ts(row)
        day = dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat()
        period = period_label(i, len(rows))
        recent = f"last_{args.recent_days}d" if ts >= recent_start else f"pre_last_{args.recent_days}d"
        add_market(by_period_market[period], pnl, fills)
        add_market(by_day_market[day], pnl, fills)
        add_market(by_recent[recent], pnl, fills)

        resolved_yes = yes_resolved(row, strat)
        range_bucket = final_range_bucket(float(row.get("volatility_range") or 0.0))
        for fill in fills:
            tag = str(fill.get("tag") or "unknown")
            if tag not in LANES:
                continue
            if not isinstance(fill.get("post_fill_path"), dict):
                missing_paths += 1
            fpnl = fill_pnl(fill, resolved_yes)
            won = fill_won(fill, resolved_yes)
            post_bucket = postfill_bucket(fill)
            add_fill(by_lane[tag], fill, fpnl, won)
            add_fill(by_period_lane[f"{period}:{tag}"], fill, fpnl, won)
            add_fill(by_postfill[f"{tag}:{post_bucket}"], fill, fpnl, won)
            add_fill(by_range[f"{tag}:{range_bucket}"], fill, fpnl, won)
            add_fill(by_live[f"{tag}:{live_regime_label(fill)}"], fill, fpnl, won)
            add_fill(by_observed[f"{tag}:{observed_range_bucket(float(fill.get('market_yes_range_so_far') or 0.0))}"], fill, fpnl, won)
            add_daily_path_fill(by_day_path[day], fill, fpnl, row)
            add_daily_path_fill(by_day_lane_path[f"{day}:{tag}"], fill, fpnl, row)
            if post_bucket == "crossed_mid_after_fill":
                worst_crossed.append(
                    {
                        "slug": row.get("slug"),
                        "date": day,
                        "tag": tag,
                        "pnl": fpnl,
                        "cost": float(fill.get("notional") or 0.0),
                        "price": float(fill.get("price") or 0.0),
                        "observed_range": float(fill.get("market_yes_range_so_far") or 0.0),
                        "final_range": float(row.get("volatility_range") or 0.0),
                        "side_model_p": float(fill.get("side_model_p") or 0.0),
                        "edge": float(fill.get("side_edge_vs_fill") or 0.0),
                    }
                )

    source = args.source_label or args.markets_jsonl
    first_ts = close_ts(rows[0])
    lines = [
        "# BTC5m Post-Fill Regime Evolution",
        "",
        f"Source: `{source}`",
        f"Markets: `{len(rows)}`",
        f"Calendar: `{dt.datetime.fromtimestamp(first_ts, tz=dt.timezone.utc).isoformat()}` to `{dt.datetime.fromtimestamp(max_ts, tz=dt.timezone.utc).isoformat()}`",
        f"Missing post-fill paths on tracked lane fills: `{missing_paths}`",
        "",
        "This report uses post-fill labels only as diagnostics/training targets. Candidate gates still need to use replay-safe fill-time features.",
        "",
    ]

    render_market_table(lines, "Market Periods", sorted(by_period_market.items()))
    render_market_table(lines, f"Recent Split ({args.recent_days}d)", sorted(by_recent.items()))

    daily_rows = sorted(by_day_market.items())[-45:]
    render_market_table(lines, "Daily PnL (Last 45 Calendar Rows In Artifact)", daily_rows)
    daily_path_rows = sorted(by_day_path.items())[-60:]
    render_daily_path_table(lines, "Daily Toxic Path Evolution (Last 60 Calendar Rows In Artifact)", daily_path_rows)

    worst_daily_crossed = sorted(
        ((label, acc) for label, acc in by_day_path.items() if acc["crossed_mid"]),
        key=lambda item: item[1]["crossed_mid_pnl"],
    )[:20]
    render_daily_path_table(lines, "Worst Daily Cross-Mid Contribution", worst_daily_crossed)

    recent_lane_path_rows = sorted(
        (item for item in by_day_lane_path.items() if item[0].split(":", 1)[0] in {label for label, _ in daily_path_rows}),
        key=lambda item: (item[0].split(":", 1)[0], item[0].split(":", 1)[1]),
    )
    render_daily_path_table(lines, "Daily Toxic Path Evolution By Lane (Recent Days)", recent_lane_path_rows)
    render_fill_table(lines, "By Lane", sorted(by_lane.items()))
    render_fill_table(lines, "By Period And Lane", sorted(by_period_lane.items()))
    render_fill_table(lines, "By Post-Fill Path", sorted(by_postfill.items()))
    render_fill_table(lines, "By Final Range", sorted(by_range.items()))
    render_fill_table(lines, "By Live Regime Label", sorted(by_live.items()))
    render_fill_table(lines, "By Observed Range At Entry", sorted(by_observed.items()))

    lines.extend(
        [
            "## Worst Cross-Mid Fills",
            "",
            "| Rank | Date | Slug | Lane | PnL | Cost | Price | Observed Range | Final Range | Model P | Edge |",
            "|---:|---|---|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for rank, item in enumerate(sorted(worst_crossed, key=lambda x: x["pnl"])[:24], 1):
        lines.append(
            f"| {rank} | {item['date']} | {item['slug']} | {item['tag']} | "
            f"{fmt_money(item['pnl'])} | {fmt_money(item['cost'])} | {item['price']:.4f} | "
            f"{item['observed_range']:.3f} | {item['final_range']:.3f} | "
            f"{item['side_model_p']:.3f} | {item['edge']:.3f} |"
        )
    lines.append("")

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
