#!/usr/bin/env python3
"""Estimate portfolio/day impact of a replay-safe late-break throttle."""

from __future__ import annotations

import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path
from typing import Any, Callable

import late_break_gate_search as search
import postfill_reversal_model as prm


def money(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{value:.2%}"


def range_bucket(value: float) -> str:
    if value < 0.30:
        return "narrow_lt30"
    if value < 0.50:
        return "mid_30_50"
    if value < 0.75:
        return "mid_wide_50_75"
    if value < 0.95:
        return "wide_75_95"
    return "extreme_gte95"


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


def iter_rows(path: Path) -> list[dict[str, Any]]:
    rows = [json.loads(line) for line in path.open() if line.strip()]
    rows.sort(key=close_ts)
    return rows


def f(fill: dict[str, Any], key: str) -> float:
    return float(fill.get(key) or 0.0)


def build_predicate(name: str, train: list[dict[str, Any]]) -> Callable[[dict[str, Any]], bool]:
    parts = name.split("&")
    predicates: list[Callable[[dict[str, Any]], bool]] = []
    for part in parts:
        lane_name = ""
        feature_part = part
        if part.startswith("late_confirm:"):
            lane_name = "br2_late_confirm"
            feature_part = part.split(":", 1)[1]
        elif part.startswith("late_fav:"):
            lane_name = "br2_late_favourite_load"
            feature_part = part.split(":", 1)[1]
        feature, bucket = feature_part.rsplit(":", 1)
        qs = search.quantiles(train, feature)
        base_predicate = search.quartile_predicate(feature, bucket, qs)
        if lane_name:
            predicates.append(
                lambda fill, bp=base_predicate, ln=lane_name: fill.get("tag") == ln and bp(fill)
            )
        else:
            predicates.append(base_predicate)
    return lambda fill: all(predicate(fill) for predicate in predicates)


def market_key(row: dict[str, Any]) -> str:
    return str(row.get("slug") or close_ts(row))


def acc() -> dict[str, float]:
    return defaultdict(float)


def add(row: dict[str, float], base_pnl: float, adjustment: float, removed_fill: bool) -> None:
    row["markets"] += 1
    row["base_pnl"] += base_pnl
    row["adjustment"] += adjustment
    row["adjusted_pnl"] += base_pnl + adjustment
    row["throttled_markets"] += 1 if removed_fill else 0


def max_drawdown(equity_curve: list[float]) -> float:
    peak = equity_curve[0] if equity_curve else 0.0
    worst = 0.0
    for equity in equity_curve:
        peak = max(peak, equity)
        if peak > 0.0:
            worst = max(worst, (peak - equity) / peak)
    return worst


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--candidate", default="side_model_p:q1&side_edge_vs_fill:q2")
    parser.add_argument("--min-train-fills", type=int, default=600)
    parser.add_argument("--test-fills", type=int, default=200)
    parser.add_argument("--step-fills", type=int, default=200)
    parser.add_argument("--throttle-frac", type=float, default=0.5)
    parser.add_argument("--starting-capital", type=float, default=1000.0)
    parser.add_argument("--source-label")
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    rows = iter_rows(Path(args.markets_jsonl))
    fills = prm.load_fills(args.markets_jsonl, args.strategy, "toxic_reversal_path")
    fills = [fill for fill in fills if fill["tag"] in search.LATE_LANES]
    fills.sort(key=lambda fill: (int(fill["ts"]), str(fill["tag"])))
    if len(fills) < args.min_train_fills + args.test_fills:
        raise RuntimeError("not enough late fills for portfolio sim")

    fill_adjustments: dict[tuple[int, str, int], float] = defaultdict(float)
    fold_lines: list[str] = []
    start = args.min_train_fills
    fold = 0
    while start + args.test_fills <= len(fills):
        train = fills[:start]
        test = fills[start : start + args.test_fills]
        predicate = build_predicate(args.candidate, train)
        fold += 1
        fold_removed = [fill for fill in test if predicate(fill)]
        fold_removed_pnl = sum(float(fill["pnl"]) for fill in fold_removed)
        for fill in fold_removed:
            key = (int(fill["ts"]), str(fill["tag"]), int(fill.get("fill_index") or fill.get("order_index") or 0))
            # If no stable fill index exists, fall back to a lossy key later by assigning
            # through market/tag/price matching. Most current artifacts do not expose a
            # globally unique fill id, so we also store an ordinal below.
            fill_adjustments[key] += -args.throttle_frac * float(fill["pnl"])
        fold_lines.append(
            f"| {fold} | {len(train)} | {len(test)} | "
            f"{dt.datetime.fromtimestamp(int(test[0]['ts']), tz=dt.timezone.utc).date()} | "
            f"{dt.datetime.fromtimestamp(int(test[-1]['ts']), tz=dt.timezone.utc).date()} | "
            f"{len(fold_removed)} | {money(fold_removed_pnl)} | {money(-args.throttle_frac * fold_removed_pnl)} |"
        )
        start += args.step_fills

    # Rebuild the same test-window predicates directly against rows so the market
    # adjustments are deterministic even without a unique fill id in the JSON.
    row_adjustments: dict[str, float] = defaultdict(float)
    row_throttled: dict[str, int] = defaultdict(int)
    start = args.min_train_fills
    while start + args.test_fills <= len(fills):
        train = fills[:start]
        test = fills[start : start + args.test_fills]
        predicate = build_predicate(args.candidate, train)
        test_ts = {int(fill["ts"]) for fill in test}
        for row in rows:
            ts = close_ts(row)
            if ts not in test_ts:
                continue
            strat = strategy(row, args.strategy)
            resolved_yes = yes_resolved(row, strat)
            key = market_key(row)
            for fill in strat.get("fills_detail") or []:
                if str(fill.get("tag") or "") not in search.LATE_LANES:
                    continue
                # Match the features used by the gate; these are fill-time values.
                if predicate(fill):
                    pnl = fill_pnl(fill, resolved_yes)
                    row_adjustments[key] += -args.throttle_frac * pnl
                    row_throttled[key] += 1
        start += args.step_fills

    by_day: dict[str, dict[str, float]] = defaultdict(acc)
    by_final_range: dict[str, dict[str, float]] = defaultdict(acc)
    by_day_range: dict[str, dict[str, float]] = defaultdict(acc)
    base_equity = args.starting_capital
    adjusted_equity = args.starting_capital
    base_curve = [base_equity]
    adjusted_curve = [adjusted_equity]
    base_pnl_total = 0.0
    adjusted_pnl_total = 0.0
    adjustment_total = 0.0
    throttled_markets = 0
    for row in rows:
        strat = strategy(row, args.strategy)
        pnl = float(strat.get("pnl_usdc") or 0.0)
        adjustment = row_adjustments[market_key(row)]
        day = dt.datetime.fromtimestamp(close_ts(row), tz=dt.timezone.utc).date().isoformat()
        throttled = row_throttled[market_key(row)] > 0
        final_range = range_bucket(float(row.get("volatility_range") or 0.0))
        add(by_day[day], pnl, adjustment, throttled)
        add(by_final_range[final_range], pnl, adjustment, throttled)
        add(by_day_range[f"{day} {final_range}"], pnl, adjustment, throttled)
        base_pnl_total += pnl
        adjusted_pnl_total += pnl + adjustment
        adjustment_total += adjustment
        throttled_markets += 1 if throttled else 0
        base_equity += pnl
        adjusted_equity += pnl + adjustment
        base_curve.append(base_equity)
        adjusted_curve.append(adjusted_equity)

    source = args.source_label or args.markets_jsonl
    lines = [
        "# BTC5m Late-Break Gate Portfolio Simulation",
        "",
        f"Source: `{source}`",
        f"Candidate: `{args.candidate}`",
        f"Throttle fraction: `{args.throttle_frac:.2f}`",
        f"Markets: `{len(rows)}`",
        f"Base PnL: `{money(base_pnl_total)}`",
        f"Adjusted PnL: `{money(adjusted_pnl_total)}`",
        f"Adjustment: `{money(adjustment_total)}`",
        f"Throttled markets: `{throttled_markets}`",
        f"Base max DD: `{pct(max_drawdown(base_curve))}`",
        f"Adjusted max DD: `{pct(max_drawdown(adjusted_curve))}`",
        "",
        "This is an offline what-if. Thresholds are fit on previous late-break fills only, then applied to later test windows. It assumes PnL contribution scales linearly with fill size.",
        "",
        "## Folds",
        "",
        "| Fold | Train Fills | Test Fills | Test Start | Test End | Throttled Fills | Removed PnL | Half-Throttle Adjustment |",
        "|---:|---:|---:|---|---|---:|---:|---:|",
        *fold_lines,
        "",
        "## Daily Impact",
        "",
        "| Day | Markets | Base PnL | Adjustment | Adjusted PnL | Throttled Markets |",
        "|---|---:|---:|---:|---:|---:|",
    ]
    for day, row in sorted(by_day.items()):
        if row["markets"] == 0:
            continue
        lines.append(
            f"| {day} | {int(row['markets'])} | {money(row['base_pnl'])} | "
            f"{money(row['adjustment'])} | {money(row['adjusted_pnl'])} | "
            f"{int(row['throttled_markets'])} |"
        )
    lines.extend(
        [
            "",
            "## Final Range Bucket Impact",
            "",
            "Final range uses the resolved whole-market path, so this section is post-hoc diagnostics only. It should not be used directly as a live gate.",
            "",
            "| Final Range Bucket | Markets | Base PnL | Adjustment | Adjusted PnL | Throttled Markets |",
            "|---|---:|---:|---:|---:|---:|",
        ]
    )
    for bucket in ["narrow_lt30", "mid_30_50", "mid_wide_50_75", "wide_75_95", "extreme_gte95"]:
        row = by_final_range.get(bucket)
        if not row:
            continue
        lines.append(
            f"| {bucket} | {int(row['markets'])} | {money(row['base_pnl'])} | "
            f"{money(row['adjustment'])} | {money(row['adjusted_pnl'])} | "
            f"{int(row['throttled_markets'])} |"
        )
    worst_day_ranges = sorted(by_day_range.items(), key=lambda item: item[1]["adjustment"])[:12]
    best_day_ranges = sorted(by_day_range.items(), key=lambda item: item[1]["adjustment"], reverse=True)[:12]
    lines.extend(
        [
            "",
            "## Worst Day-Range Adjustments",
            "",
            "| Day + Final Range Bucket | Markets | Base PnL | Adjustment | Adjusted PnL | Throttled Markets |",
            "|---|---:|---:|---:|---:|---:|",
        ]
    )
    for label, row in worst_day_ranges:
        lines.append(
            f"| {label} | {int(row['markets'])} | {money(row['base_pnl'])} | "
            f"{money(row['adjustment'])} | {money(row['adjusted_pnl'])} | "
            f"{int(row['throttled_markets'])} |"
        )
    lines.extend(
        [
            "",
            "## Best Day-Range Adjustments",
            "",
            "| Day + Final Range Bucket | Markets | Base PnL | Adjustment | Adjusted PnL | Throttled Markets |",
            "|---|---:|---:|---:|---:|---:|",
        ]
    )
    for label, row in best_day_ranges:
        lines.append(
            f"| {label} | {int(row['markets'])} | {money(row['base_pnl'])} | "
            f"{money(row['adjustment'])} | {money(row['adjusted_pnl'])} | "
            f"{int(row['throttled_markets'])} |"
        )
    lines.append("")

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
