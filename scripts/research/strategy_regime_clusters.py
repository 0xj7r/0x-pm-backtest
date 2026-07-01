#!/usr/bin/env python3
"""Compare strategy performance by live-safe market regime clusters.

The input is one or more walk-forward `markets.jsonl` files.  Regime labels are
derived only from fill-time fields emitted in `fills_detail`, not from resolved
outcome or final market range.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any


FEATURES = (
    "market_yes_range_so_far",
    "regime_whipsaw_score",
    "regime_path_efficiency",
    "regime_reversal_pressure",
    "regime_sign_flip_rate",
    "regime_realized_vol_180s_bps",
    "binance_adverse_vol_30s",
    "binance_flow_imbal_30s",
    "seconds_to_close",
    "side_edge_vs_fill",
    "confidence_score",
    "risk_score",
)


def parse_input(value: str) -> tuple[str, Path]:
    if "=" not in value:
        path = Path(value)
        return path.stem, path
    label, raw_path = value.split("=", 1)
    return label, Path(raw_path)


def close_ts(row: dict[str, Any]) -> int:
    value = row.get("close_ts")
    if value is not None:
        return int(value)
    slug = str(row.get("slug") or "")
    return int(slug.rsplit("-", 1)[1]) + 300


def avg(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def fmt_usd(value: float) -> str:
    return f"${value:,.2f}"


def fmt_num(value: float | None, digits: int = 3) -> str:
    if value is None or not math.isfinite(value):
        return "-"
    return f"{value:.{digits}f}"


def numeric(value: Any) -> float | None:
    if isinstance(value, (int, float)) and math.isfinite(float(value)):
        return float(value)
    return None


def weighted_feature(fill_rows: list[dict[str, Any]], key: str) -> float | None:
    pairs: list[tuple[float, float]] = []
    for fill in fill_rows:
        value = numeric(fill.get(key))
        if value is None:
            continue
        weight = numeric(fill.get("notional")) or 1.0
        pairs.append((value, max(weight, 0.0)))
    total_weight = sum(weight for _, weight in pairs)
    if total_weight <= 0.0:
        return avg([value for value, _ in pairs])
    return sum(value * weight for value, weight in pairs) / total_weight


def cluster_label(features: dict[str, float | None]) -> str:
    observed_range = features.get("market_yes_range_so_far") or 0.0
    path_eff = features.get("regime_path_efficiency") or 0.0
    reversal = features.get("regime_reversal_pressure") or 0.0
    sign_flip = features.get("regime_sign_flip_rate") or 0.0
    realized_vol = features.get("regime_realized_vol_180s_bps") or 0.0
    adverse_vol = features.get("binance_adverse_vol_30s") or 0.0

    if observed_range >= 0.20 and sign_flip >= 0.50:
        return "expanded_high_flip"
    if observed_range >= 0.20 and reversal >= 0.30:
        return "expanded_reversal_pressure"
    if 2.0 <= realized_vol <= 8.0 and 1.0 <= adverse_vol <= 4.0:
        return "flow_adverse_vol_cluster"
    if path_eff < 0.10 and reversal < 0.30:
        return "low_efficiency_nonreversal"
    if path_eff >= 0.35 and sign_flip < 0.50 and reversal < 0.30:
        return "clean_directional_path"
    if realized_vol < 1.0 and adverse_vol < 1.0 and reversal < 0.30:
        return "calm_low_vol"
    if observed_range < 0.10:
        return "early_tight_range"
    return "mixed_neutral"


def bucket_label(key: str, value: float | None) -> str:
    if value is None:
        return f"{key}:missing"
    if key == "market_yes_range_so_far":
        cuts = (0.10, 0.20, 0.30)
    elif key == "regime_path_efficiency":
        cuts = (0.10, 0.20, 0.35, 0.50)
    elif key == "regime_sign_flip_rate":
        cuts = (0.35, 0.50)
    elif key == "regime_reversal_pressure":
        cuts = (0.30, 0.45, 0.60)
    elif key == "regime_realized_vol_180s_bps":
        cuts = (1.0, 2.0, 4.0, 8.0)
    elif key == "binance_adverse_vol_30s":
        cuts = (0.50, 1.0, 2.0, 4.0, 8.0)
    else:
        cuts = ()
    lower = "-inf"
    for cut in cuts:
        if value <= cut:
            return f"{key}:({lower},{cut:g}]"
        lower = f"{cut:g}"
    return f"{key}:({lower},inf]"


def add(acc: dict[str, float], row: dict[str, Any]) -> None:
    pnl = float(row["pnl"])
    acc["markets"] += 1
    acc["traded_markets"] += 1 if row["fills"] > 0 else 0
    acc["fills"] += row["fills"]
    acc["pnl"] += pnl
    acc["wins"] += 1 if pnl > 0.0 else 0
    acc["losses"] += 1 if pnl < 0.0 else 0
    acc["filled_notional"] += row["filled_notional"]
    acc["worst"] = min(acc.get("worst", 0.0), pnl)
    acc["best"] = max(acc.get("best", 0.0), pnl)


def iter_strategy_rows(label: str, path: Path, strategies: set[str] | None) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    with path.open() as file:
        for line in file:
            if not line.strip():
                continue
            row = json.loads(line)
            ts = close_ts(row)
            day = dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat()
            per_strategy = row.get("per_strategy") or {}
            names = sorted(per_strategy) if strategies is None else sorted(strategies)
            for strategy in names:
                strat = per_strategy.get(strategy) or {}
                fills = strat.get("fills_detail") or []
                features = {key: weighted_feature(fills, key) for key in FEATURES}
                pnl = float(strat.get("pnl_usdc") or 0.0)
                fill_count = int(strat.get("fills") or 0)
                out.append(
                    {
                        "input": label,
                        "path": str(path),
                        "strategy": strategy,
                        "day": day,
                        "slug": row.get("slug"),
                        "pnl": pnl,
                        "fills": fill_count,
                        "filled_notional": float(strat.get("filled_notional_usdc") or 0.0),
                        "cluster": cluster_label(features) if fill_count > 0 else "no_trade",
                        "features": features,
                    }
                )
    return out


def write_table(lines: list[str], title: str, rows: list[tuple[tuple[str, ...], dict[str, float]]]) -> None:
    lines.extend([f"## {title}", ""])
    lines.append("| Input | Strategy | Bucket | Markets | Traded | Fills | PnL | Mean / Traded | Win Rate | Worst | Best | Filled |")
    lines.append("|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for key, acc in rows:
        input_label, strategy, bucket = key
        traded = int(acc["traded_markets"])
        mean = acc["pnl"] / traded if traded else 0.0
        win_rate = acc["wins"] / traded if traded else 0.0
        lines.append(
            f"| {input_label} | {strategy} | {bucket} | {int(acc['markets'])} | {traded} | "
            f"{int(acc['fills'])} | {fmt_usd(acc['pnl'])} | {fmt_usd(mean)} | "
            f"{win_rate:.1%} | {fmt_usd(acc['worst'])} | {fmt_usd(acc['best'])} | "
            f"{fmt_usd(acc['filled_notional'])} |"
        )
    lines.append("")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", action="append", required=True, help="LABEL=path/to/markets.jsonl")
    parser.add_argument("--strategy", action="append", help="strategy name; defaults to all present")
    parser.add_argument("--out-md", required=True)
    parser.add_argument("--out-json")
    args = parser.parse_args()

    strategies = set(args.strategy) if args.strategy else None
    rows: list[dict[str, Any]] = []
    for raw_input in args.input:
        label, path = parse_input(raw_input)
        rows.extend(iter_strategy_rows(label, path, strategies))

    by_cluster: dict[tuple[str, str, str], dict[str, float]] = defaultdict(lambda: defaultdict(float))
    by_day_cluster: dict[tuple[str, str, str], dict[str, float]] = defaultdict(lambda: defaultdict(float))
    by_bucket: dict[str, dict[tuple[str, str, str], dict[str, float]]] = {
        key: defaultdict(lambda: defaultdict(float))
        for key in (
            "market_yes_range_so_far",
            "regime_path_efficiency",
            "regime_sign_flip_rate",
            "regime_reversal_pressure",
            "regime_realized_vol_180s_bps",
            "binance_adverse_vol_30s",
        )
    }

    for row in rows:
        add(by_cluster[(row["input"], row["strategy"], row["cluster"])], row)
        add(by_day_cluster[(row["input"], row["strategy"], row["day"])], row)
        if row["fills"] <= 0:
            continue
        for feature in by_bucket:
            bucket = bucket_label(feature, row["features"].get(feature))
            add(by_bucket[feature][(row["input"], row["strategy"], bucket)], row)

    lines: list[str] = [
        "# Strategy Regime Cluster Report",
        "",
        "Clusters are live-safe: they use only fill-time `fills_detail` fields.",
        "",
        "Primary labels:",
        "",
        "- `expanded_high_flip`: observed range >= 0.20 and sign flip >= 0.50.",
        "- `expanded_reversal_pressure`: observed range >= 0.20 and reversal pressure >= 0.30.",
        "- `flow_adverse_vol_cluster`: realized vol 2-8 bps and 30s adverse vol 1-4.",
        "- `low_efficiency_nonreversal`: path efficiency < 0.10 with low reversal pressure.",
        "- `clean_directional_path`: path efficiency >= 0.35, sign flip < 0.50, reversal < 0.30.",
        "- `calm_low_vol`: realized vol < 1, adverse vol < 1, reversal < 0.30.",
        "- `early_tight_range`: observed range < 0.10.",
        "- `mixed_neutral`: none of the above.",
        "",
    ]

    cluster_rows = sorted(by_cluster.items(), key=lambda item: (item[0][0], item[0][1], item[1]["pnl"]))
    write_table(lines, "Primary Clusters", cluster_rows)

    day_rows = sorted(by_day_cluster.items(), key=lambda item: item[1]["pnl"])[:30]
    write_table(lines, "Worst Daily Windows", day_rows)

    for feature, table in by_bucket.items():
        feature_rows = sorted(table.items(), key=lambda item: (item[0][0], item[0][1], item[0][2]))
        write_table(lines, f"Feature Buckets: {feature}", feature_rows)

    out_md = Path(args.out_md)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_md.write_text("\n".join(lines) + "\n")

    if args.out_json:
        serializable_rows = []
        for row in rows:
            item = dict(row)
            item["features"] = {k: v for k, v in row["features"].items() if v is not None}
            serializable_rows.append(item)
        out_json = Path(args.out_json)
        out_json.parent.mkdir(parents=True, exist_ok=True)
        out_json.write_text(json.dumps(serializable_rows, indent=2, sort_keys=True) + "\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
