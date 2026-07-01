#!/usr/bin/env python3
"""Contrast failed late breaks with profitable late breaks using fill-time features."""

from __future__ import annotations

import argparse
import datetime as dt
import statistics
from collections import defaultdict
from pathlib import Path
from typing import Any

import postfill_reversal_model as prm


LATE_LANES = {"br2_late_confirm", "br2_late_favourite_load"}

FEATURES = [
    "price",
    "side_model_p",
    "side_edge_vs_fill",
    "confidence_score",
    "risk_score",
    "market_yes_range_so_far",
    "seconds_to_close",
    "regime_whipsaw_score",
    "regime_path_efficiency",
    "regime_reversal_pressure",
    "regime_sign_flip_rate",
    "regime_realized_vol_180s_bps",
    "prior_market_range_1d",
    "prior_market_range_3d",
    "prior_market_range_7d",
]


def money(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{value:.2%}"


def val(fill: dict[str, Any], key: str) -> float:
    return float(fill.get(key) or 0.0)


def mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0


def std(values: list[float]) -> float:
    if len(values) < 2:
        return 0.0
    return statistics.pstdev(values)


def feature_contrast(fills: list[dict[str, Any]]) -> list[tuple[str, float, float, float, float, int, int]]:
    toxic = [fill for fill in fills if fill["target"]]
    good = [fill for fill in fills if not fill["target"] and float(fill["pnl"]) > 0.0]
    rows = []
    for feature in FEATURES:
        toxic_values = [val(fill, feature) for fill in toxic]
        good_values = [val(fill, feature) for fill in good]
        toxic_mean = mean(toxic_values)
        good_mean = mean(good_values)
        pooled = (std(toxic_values) + std(good_values)) / 2.0
        smd = (toxic_mean - good_mean) / pooled if pooled > 1e-9 else 0.0
        rows.append((feature, toxic_mean, good_mean, toxic_mean - good_mean, smd, len(toxic), len(good)))
    return sorted(rows, key=lambda row: abs(row[4]), reverse=True)


def acc() -> dict[str, float]:
    return defaultdict(float)


def add(accum: dict[str, float], fill: dict[str, Any]) -> None:
    accum["fills"] += 1
    accum["pnl"] += float(fill["pnl"])
    accum["cost"] += float(fill["notional"])
    accum["toxic"] += 1 if fill["target"] else 0
    accum["cross_mid"] += 1 if fill["post_crossed_mid"] else 0
    accum["wins"] += 1 if fill["won"] else 0


def render_bucket_table(lines: list[str], title: str, rows: list[tuple[str, dict[str, float]]]) -> None:
    lines.extend(
        [
            f"## {title}",
            "",
            "| Bucket | Fills | PnL | Cost | Win Rate | Toxic Rate | Cross-Mid Rate |",
            "|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for label, row in rows:
        fills = int(row["fills"])
        if fills == 0:
            continue
        lines.append(
            f"| {label} | {fills} | {money(row['pnl'])} | {money(row['cost'])} | "
            f"{pct(row['wins'] / fills)} | {pct(row['toxic'] / fills)} | "
            f"{pct(row['cross_mid'] / fills)} |"
        )
    lines.append("")


def quantile_bucket(fill: dict[str, Any], feature: str, thresholds: list[float]) -> str:
    value = val(fill, feature)
    if value < thresholds[0]:
        return f"{feature}:q1"
    if value < thresholds[1]:
        return f"{feature}:q2"
    if value < thresholds[2]:
        return f"{feature}:q3"
    return f"{feature}:q4"


def threshold_scan(fills: list[dict[str, Any]]) -> list[dict[str, Any]]:
    candidates: list[dict[str, Any]] = []
    for feature in FEATURES:
        values = sorted({val(fill, feature) for fill in fills})
        if len(values) < 8:
            continue
        thresholds = [
            values[int((len(values) - 1) * q)]
            for q in (0.20, 0.30, 0.40, 0.50, 0.60, 0.70, 0.80)
        ]
        for threshold in thresholds:
            for direction in ("ge", "le"):
                if direction == "ge":
                    removed = [fill for fill in fills if val(fill, feature) >= threshold]
                else:
                    removed = [fill for fill in fills if val(fill, feature) <= threshold]
                if len(removed) < 15:
                    continue
                removed_pnl = sum(float(fill["pnl"]) for fill in removed)
                removed_toxic = sum(1 for fill in removed if fill["target"])
                removed_cross = sum(1 for fill in removed if fill["post_crossed_mid"])
                candidates.append(
                    {
                        "feature": feature,
                        "direction": direction,
                        "threshold": threshold,
                        "removed_fills": len(removed),
                        "removed_pnl": removed_pnl,
                        "removed_cost": sum(float(fill["notional"]) for fill in removed),
                        "toxic_rate": removed_toxic / len(removed),
                        "cross_rate": removed_cross / len(removed),
                    }
                )
    return sorted(candidates, key=lambda row: row["removed_pnl"])[:30]


def pair_scan(fills: list[dict[str, Any]]) -> list[dict[str, Any]]:
    specs = [
        (
            "high_signflip_low_eff",
            lambda fill: val(fill, "regime_sign_flip_rate") >= 0.35
            and val(fill, "regime_path_efficiency") <= 0.25,
        ),
        (
            "high_reversal_low_eff",
            lambda fill: val(fill, "regime_reversal_pressure") >= 0.30
            and val(fill, "regime_path_efficiency") <= 0.25,
        ),
        (
            "obs_mid_high_signflip",
            lambda fill: 0.40 <= val(fill, "market_yes_range_so_far") < 0.65
            and val(fill, "regime_sign_flip_rate") >= 0.35,
        ),
        (
            "price_high_edge_low",
            lambda fill: val(fill, "price") >= 0.78 and val(fill, "side_edge_vs_fill") <= 0.10,
        ),
        (
            "price_high_reversal",
            lambda fill: val(fill, "price") >= 0.78 and val(fill, "regime_reversal_pressure") >= 0.30,
        ),
        (
            "fav_high_price_chop",
            lambda fill: fill["tag"] == "br2_late_favourite_load"
            and val(fill, "price") >= 0.78
            and (val(fill, "regime_sign_flip_rate") >= 0.35 or val(fill, "regime_path_efficiency") <= 0.25),
        ),
        (
            "confirm_low_edge_reversal",
            lambda fill: fill["tag"] == "br2_late_confirm"
            and val(fill, "side_edge_vs_fill") <= 0.08
            and val(fill, "regime_reversal_pressure") >= 0.30,
        ),
    ]
    rows = []
    for name, predicate in specs:
        removed = [fill for fill in fills if predicate(fill)]
        if not removed:
            continue
        rows.append(
            {
                "name": name,
                "removed_fills": len(removed),
                "removed_pnl": sum(float(fill["pnl"]) for fill in removed),
                "removed_cost": sum(float(fill["notional"]) for fill in removed),
                "toxic_rate": sum(1 for fill in removed if fill["target"]) / len(removed),
                "cross_rate": sum(1 for fill in removed if fill["post_crossed_mid"]) / len(removed),
            }
        )
    return sorted(rows, key=lambda row: row["removed_pnl"])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--source-label")
    parser.add_argument("--last-fills", type=int, default=0)
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    fills = prm.load_fills(args.markets_jsonl, args.strategy, "toxic_reversal_path")
    fills = [fill for fill in fills if fill["tag"] in LATE_LANES]
    fills.sort(key=lambda fill: (int(fill["ts"]), str(fill["tag"])))
    if args.last_fills:
        fills = fills[-args.last_fills :]
    if not fills:
        raise RuntimeError("no late break fills found")

    source = args.source_label or args.markets_jsonl
    first_ts = dt.datetime.fromtimestamp(int(fills[0]["ts"]), tz=dt.timezone.utc)
    last_ts = dt.datetime.fromtimestamp(int(fills[-1]["ts"]), tz=dt.timezone.utc)
    total_pnl = sum(float(fill["pnl"]) for fill in fills)
    toxic = [fill for fill in fills if fill["target"]]
    non_toxic = [fill for fill in fills if not fill["target"]]

    by_lane: dict[str, dict[str, float]] = defaultdict(acc)
    by_path: dict[str, dict[str, float]] = defaultdict(acc)
    for fill in fills:
        add(by_lane[fill["tag"]], fill)
        if fill["post_crossed_mid"]:
            path = "crossed_mid_after_fill"
        elif val(fill, "post_adverse_excursion") >= 0.10:
            path = "moderate_adverse_no_cross"
        else:
            path = "held_side"
        add(by_path[path], fill)

    lines = [
        "# BTC5m Late-Break Feature Contrast",
        "",
        f"Source: `{source}`",
        f"Fills: `{len(fills)}` late-confirm/favourite fills",
        f"Calendar: `{first_ts.isoformat()}` to `{last_ts.isoformat()}`",
        f"PnL: `{money(total_pnl)}`",
        f"Toxic fills: `{len(toxic)}` (`{pct(len(toxic) / len(fills))}`)",
        "",
        "This diagnostic contrasts failed late breaks against profitable late breaks using fill-time features only. Post-fill labels are used only to define the offline target.",
        "",
    ]

    render_bucket_table(lines, "By Lane", sorted(by_lane.items()))
    render_bucket_table(lines, "By Post-Fill Path", sorted(by_path.items()))

    lines.extend(
        [
            "## Feature Contrast: Toxic vs Profitable Non-Toxic Late Breaks",
            "",
            "| Feature | Toxic Mean | Profitable Non-Toxic Mean | Difference | Std Diff | Toxic N | Profitable N |",
            "|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for feature, toxic_mean, good_mean, diff, smd, toxic_n, good_n in feature_contrast(fills):
        lines.append(
            f"| {feature} | {toxic_mean:.4f} | {good_mean:.4f} | {diff:.4f} | "
            f"{smd:.3f} | {toxic_n} | {good_n} |"
        )
    lines.append("")

    top_features = [row[0] for row in feature_contrast(fills)[:6]]
    for feature in top_features:
        values = sorted(val(fill, feature) for fill in fills)
        thresholds = [values[int((len(values) - 1) * q)] for q in (0.25, 0.50, 0.75)]
        buckets: dict[str, dict[str, float]] = defaultdict(acc)
        for fill in fills:
            add(buckets[quantile_bucket(fill, feature, thresholds)], fill)
        render_bucket_table(lines, f"Quartiles: {feature}", sorted(buckets.items()))

    lines.extend(
        [
            "## Single-Feature Removal Scan",
            "",
            "Positive removed PnL means a gate would remove profitable fills. Negative removed PnL is the interesting direction.",
            "",
            "| Feature | Direction | Threshold | Removed Fills | Removed Cost | Removed PnL | Full-Removal Improvement | Toxic Rate | Cross-Mid Rate |",
            "|---|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for row in threshold_scan(fills):
        lines.append(
            f"| {row['feature']} | {row['direction']} | {row['threshold']:.4f} | "
            f"{row['removed_fills']} | {money(row['removed_cost'])} | {money(row['removed_pnl'])} | "
            f"{money(-row['removed_pnl'])} | {pct(row['toxic_rate'])} | {pct(row['cross_rate'])} |"
        )
    lines.append("")

    lines.extend(
        [
            "## Two-Feature Candidate Scan",
            "",
            "| Candidate | Removed Fills | Removed Cost | Removed PnL | Full-Removal Improvement | Toxic Rate | Cross-Mid Rate |",
            "|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for row in pair_scan(fills):
        lines.append(
            f"| {row['name']} | {row['removed_fills']} | {money(row['removed_cost'])} | "
            f"{money(row['removed_pnl'])} | {money(-row['removed_pnl'])} | "
            f"{pct(row['toxic_rate'])} | {pct(row['cross_rate'])} |"
        )
    lines.append("")

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
