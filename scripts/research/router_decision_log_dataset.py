#!/usr/bin/env python3
"""Build a router dataset from pre-route decision-log market-state rows.

This is the deploy-validation counterpart to `router_market_dataset.py`.  It
uses per-market PnL labels from candidate `markets.jsonl` artifacts, but takes
current-market features from runner decision logs instead of `fills_detail`.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from bisect import bisect_right
from collections import defaultdict
from pathlib import Path
from typing import Any

from router_market_dataset import (
    DEFAULT_WINDOWS,
    add_rolling_features,
    close_ts,
    equity_stats,
    finite_float,
    fmt_usd,
)
from strategy_regime_clusters import cluster_label


DECISION_FEATURES = (
    "market_yes_range_so_far",
    "seconds_since_open",
    "seconds_to_close",
    "regime_whipsaw_score",
    "regime_path_efficiency",
    "regime_reversal_pressure",
    "regime_sign_flip_rate",
    "regime_realized_vol_180s_bps",
    "regime_cluster",
    "binance_flow_imbal_30s",
    "binance_adverse_vol_30s",
    "prior_market_range_1d",
    "prior_market_range_3d",
    "prior_market_range_7d",
    "direction_score",
    "confidence_score",
    "calibrated_p",
    "risk_score",
    "edge",
    "feature_observed_yes_range_so_far",
    "feature_whipsaw",
    "feature_path_risk",
    "feature_volatility_regime",
    "feature_dir_flip_rate_8",
    "feature_side_p_pre_meta",
    "feature_side_p_post_meta",
)

CLUSTER_FEATURE_ALIASES = {
    "market_yes_range_so_far": "market_yes_range_so_far",
    "regime_whipsaw_score": "regime_whipsaw_score",
    "regime_path_efficiency": "regime_path_efficiency",
    "regime_reversal_pressure": "regime_reversal_pressure",
    "regime_sign_flip_rate": "regime_sign_flip_rate",
    "regime_realized_vol_180s_bps": "regime_realized_vol_180s_bps",
    "binance_adverse_vol_30s": "binance_adverse_vol_30s",
}

LEGACY_ALIASES = {
    "market_yes_range_so_far": "feature_observed_yes_range_so_far",
    "regime_whipsaw_score": "feature_whipsaw",
    "regime_path_efficiency": "feature_path_risk",
    "regime_reversal_pressure": "feature_markov_reversal_risk",
    "regime_sign_flip_rate": "feature_dir_flip_rate_8",
    "regime_realized_vol_180s_bps": "feature_volatility_regime",
}


def parse_candidate(value: str) -> tuple[str, str, Path]:
    try:
        name, rest = value.split("=", 1)
        strategy, raw_path = rest.split(":", 1)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            "candidate must be NAME=STRATEGY:path/to/markets.jsonl"
        ) from exc
    return name, strategy, Path(raw_path)


def numeric(value: Any) -> float | None:
    if isinstance(value, (int, float)) and math.isfinite(float(value)):
        return float(value)
    return None


def mean(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def percentile(values: list[float], q: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, int(round((len(ordered) - 1) * q))))
    return ordered[idx]


def load_candidate(name: str, strategy: str, path: Path) -> dict[str, dict[str, Any]]:
    rows: dict[str, dict[str, Any]] = {}
    with path.open() as file:
        for line in file:
            if not line.strip():
                continue
            row = json.loads(line)
            strat = (row.get("per_strategy") or {}).get(strategy) or {}
            slug = str(row.get("slug") or "")
            rows[slug] = {
                "name": name,
                "strategy": strategy,
                "path": str(path),
                "slug": slug,
                "close_ts": close_ts(row),
                "pnl_usdc": finite_float(strat.get("pnl_usdc")),
                "fills": int(strat.get("fills") or 0),
                "orders_filled": int(strat.get("orders_filled") or 0),
                "orders_submitted": int(strat.get("orders_submitted") or 0),
                "filled_notional_usdc": finite_float(strat.get("filled_notional_usdc")),
                "requested_notional_usdc": finite_float(strat.get("requested_notional_usdc")),
                "start_equity_usdc": finite_float(strat.get("start_equity_usdc")),
                "end_equity_usdc": finite_float(strat.get("end_equity_usdc")),
            }
    return rows


def market_open_ts(slug: str, close_ts_value: int) -> int:
    lower = slug.lower()
    if "-updown-15m-" in lower:
        return close_ts_value - 900
    if "-updown-4h-" in lower:
        return close_ts_value - 14_400
    return close_ts_value - 300


def ts_to_slug_map(markets: list[dict[str, Any]]) -> tuple[list[int], list[str]]:
    # BTC/ETH same-window runs should prefer market-id mapping.  Timestamp mapping
    # is a fallback for legacy or single-asset logs.
    by_close = sorted((int(row["close_ts"]), str(row["slug"])) for row in markets)
    return [item[0] for item in by_close], [item[1] for item in by_close]


def slug_for_decision_row(
    row: dict[str, Any],
    markets_by_ordinal: list[str],
    close_values: list[int],
    close_slugs: list[str],
    market_id_base: int,
    use_ts_mapping: bool,
) -> str | None:
    market_id = int(row.get("market_id") or 0)
    idx = market_id - market_id_base
    if 0 <= idx < len(markets_by_ordinal):
        return markets_by_ordinal[idx]
    if not use_ts_mapping:
        return None
    ts_ns = int(row.get("ts_ns") or 0)
    ts = ts_ns // 1_000_000_000
    pos = bisect_right(close_values, ts)
    if pos >= len(close_values):
        return None
    return close_slugs[pos]


def summarize_decision_rows(rows: list[dict[str, Any]], allow_legacy: bool) -> dict[str, Any]:
    out: dict[str, Any] = {"decision_rows": len(rows)}
    if not rows:
        return out

    for key in DECISION_FEATURES:
        if key == "regime_cluster":
            labels = [
                str(value)
                for row in rows
                if (value := row.get(key)) is not None and str(value)
            ]
            if labels:
                out[key] = labels[-1]
            continue
        values = [value for row in rows if (value := numeric(row.get(key))) is not None]
        if values:
            out[f"{key}_mean"] = mean(values)
            out[f"{key}_last"] = values[-1]
            out[f"{key}_p90"] = percentile(values, 0.90)

    cluster_features: dict[str, float | None] = {}
    missing_new: list[str] = []
    for cluster_key, row_key in CLUSTER_FEATURE_ALIASES.items():
        values = [value for row in rows if (value := numeric(row.get(row_key))) is not None]
        if not values:
            missing_new.append(row_key)
            if allow_legacy:
                legacy_key = LEGACY_ALIASES[row_key]
                values = [
                    value
                    for row in rows
                    if (value := numeric(row.get(legacy_key))) is not None
                ]
        cluster_features[cluster_key] = values[-1] if values else None

    out["cluster_features"] = cluster_features
    out["feature_source_has_new_regime_fields"] = not missing_new
    if missing_new:
        out["missing_new_regime_fields"] = missing_new
    return out


def load_decision_features(
    path: Path,
    markets_by_ordinal: list[str],
    close_values: list[int],
    close_slugs: list[str],
    market_id_base: int,
    use_ts_mapping: bool,
    allow_legacy: bool,
    decision_strategy: str | None,
) -> dict[str, dict[str, Any]]:
    by_slug: dict[str, list[dict[str, Any]]] = defaultdict(list)
    with path.open() as file:
        for line in file:
            if not line.strip():
                continue
            row = json.loads(line)
            if decision_strategy is not None and row.get("strategy") != decision_strategy:
                continue
            slug = slug_for_decision_row(
                row,
                markets_by_ordinal,
                close_values,
                close_slugs,
                market_id_base,
                use_ts_mapping,
            )
            if slug is not None:
                by_slug[slug].append(row)

    return {
        slug: summarize_decision_rows(sorted(rows, key=lambda row: int(row.get("ts_ns") or 0)), allow_legacy)
        for slug, rows in by_slug.items()
    }


def load_market_order(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open() as file:
        for line in file:
            if not line.strip():
                continue
            row = json.loads(line)
            rows.append({"slug": str(row["slug"]), "close_ts": close_ts(row)})
    return rows


def make_rows(
    candidates_by_name: dict[str, dict[str, dict[str, Any]]],
    decision_features: dict[str, dict[str, Any]],
    windows: list[int],
) -> list[dict[str, Any]]:
    names = sorted(candidates_by_name)
    shared_slugs = set.intersection(*(set(rows) for rows in candidates_by_name.values()))
    shared_slugs &= set(decision_features)
    rows: list[dict[str, Any]] = []
    for slug in shared_slugs:
        candidates = {name: candidates_by_name[name][slug] for name in names}
        ts = max(candidate["close_ts"] for candidate in candidates.values())
        pnls = {name: candidates[name]["pnl_usdc"] for name in names}
        best_candidate = max(names, key=lambda name: pnls[name])
        sorted_pnls = sorted(pnls.values(), reverse=True)
        current_features = decision_features[slug]
        cluster = current_features.get("regime_cluster") or cluster_label(
            current_features.get("cluster_features") or {}
        )
        row = {
            "schema_version": 2,
            "slug": slug,
            "close_ts": ts,
            "date": dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat(),
            "feature_source": "decision_log",
            "feature_source_is_diagnostic": False,
            "diagnostic_cluster": cluster,
            "pre_route_decision_features": current_features,
            "candidates": candidates,
            "labels": {
                "candidate_pnl_usdc": pnls,
                "best_candidate": best_candidate,
                "best_pnl_usdc": pnls[best_candidate],
                "runner_up_pnl_usdc": sorted_pnls[1] if len(sorted_pnls) > 1 else 0.0,
                "best_minus_runner_up_pnl_usdc": (
                    sorted_pnls[0] - sorted_pnls[1] if len(sorted_pnls) > 1 else sorted_pnls[0]
                ),
            },
        }
        rows.append(row)
    rows.sort(key=lambda row: row["close_ts"])
    add_rolling_features(rows, names, windows)
    return rows


def write_decision_report(
    path: Path,
    rows: list[dict[str, Any]],
    names: list[str],
    split_idx: int,
    starting_cash: float,
) -> None:
    train = rows[:split_idx]
    test = rows[split_idx:]
    new_field_rows = sum(
        1
        for row in rows
        if row["pre_route_decision_features"].get("feature_source_has_new_regime_fields")
    )
    lines = [
        "# Router Decision-Log Dataset",
        "",
        f"Rows: `{len(rows)}`",
        f"Train rows: `{len(train)}`",
        f"Test rows: `{len(test)}`",
        f"Date range: `{rows[0]['date']}` to `{rows[-1]['date']}`",
        f"Feature source: `{rows[0]['feature_source']}`",
        f"Rows with new regime fields: `{new_field_rows}/{len(rows)}`",
        "",
        "`pre_route_decision_features` are summarized from runner decision-log rows",
        "emitted before strategy order submission. This is the feature source to use",
        "for deploy validation. If `Rows with new regime fields` is incomplete, the",
        "dataset is a legacy smoke artifact only.",
        "",
        "## Test Labels",
        "",
        "| Candidate | Test PnL | End Equity | Max DD | Mean / Market | Win Rate | Worst | Best |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for name in names:
        stats = equity_stats([row["labels"]["candidate_pnl_usdc"][name] for row in test], starting_cash)
        lines.append(
            f"| {name} | {fmt_usd(stats['pnl'])} | {fmt_usd(stats['end_equity'])} | "
            f"{stats['max_dd_pct']:.2f}% | {fmt_usd(stats['mean'])} | "
            f"{stats['win_rate']:.1%} | {fmt_usd(stats['worst'])} | {fmt_usd(stats['best'])} |"
        )

    by_cluster: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    counts: dict[str, int] = defaultdict(int)
    for row in rows:
        cluster = row["diagnostic_cluster"]
        counts[cluster] += 1
        for name in names:
            by_cluster[cluster][name] += row["labels"]["candidate_pnl_usdc"][name]
    lines.extend(["", "## Rows By Pre-Route Cluster", ""])
    lines.append("| Cluster | Rows | PnL By Candidate |")
    lines.append("|---|---:|---|")
    for cluster in sorted(counts):
        pnl_bits = ", ".join(f"{name}={fmt_usd(by_cluster[cluster][name])}" for name in names)
        lines.append(f"| {cluster} | {counts[cluster]} | {pnl_bits} |")

    lines.extend(["", "## Label Distribution", ""])
    lines.append("| Best Candidate | Train Markets | Test Markets |")
    lines.append("|---|---:|---:|")
    for name in names:
        train_count = sum(1 for row in train if row["labels"]["best_candidate"] == name)
        test_count = sum(1 for row in test if row["labels"]["best_candidate"] == name)
        lines.append(f"| {name} | {train_count} | {test_count} |")

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", action="append", required=True, type=parse_candidate)
    parser.add_argument(
        "--feature-candidate",
        required=True,
        help="candidate name whose markets/log order should be used for decision features",
    )
    parser.add_argument("--decision-log", required=True, type=Path)
    parser.add_argument(
        "--decision-strategy",
        help="optional strategy name to keep from a multi-strategy decision log",
    )
    parser.add_argument("--market-id-base", type=int, default=1)
    parser.add_argument("--use-ts-mapping", action="store_true")
    parser.add_argument(
        "--allow-legacy-decision-log",
        action="store_true",
        help="fall back to older feature_* fields when new regime fields are absent",
    )
    parser.add_argument("--train-frac", type=float, default=0.60)
    parser.add_argument("--starting-cash", type=float, default=2700.0)
    parser.add_argument("--rolling-window", action="append", type=int, dest="rolling_windows")
    parser.add_argument("--out-jsonl", required=True)
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    windows = sorted(set(args.rolling_windows or DEFAULT_WINDOWS))
    loaded: dict[str, dict[str, dict[str, Any]]] = {}
    candidate_paths: dict[str, Path] = {}
    for name, strategy, path in args.candidate:
        loaded[name] = load_candidate(name, strategy, path)
        candidate_paths[name] = path
    if args.feature_candidate not in loaded:
        raise RuntimeError(f"unknown feature candidate: {args.feature_candidate}")

    market_order = load_market_order(candidate_paths[args.feature_candidate])
    markets_by_ordinal = [row["slug"] for row in market_order]
    close_values, close_slugs = ts_to_slug_map(market_order)
    decision_features = load_decision_features(
        args.decision_log,
        markets_by_ordinal,
        close_values,
        close_slugs,
        args.market_id_base,
        args.use_ts_mapping,
        args.allow_legacy_decision_log,
        args.decision_strategy,
    )

    rows = make_rows(loaded, decision_features, windows)
    if len(rows) < 1:
        raise RuntimeError("no overlapping rows with decision features")
    new_field_rows = sum(
        1
        for row in rows
        if row["pre_route_decision_features"].get("feature_source_has_new_regime_fields")
    )
    if new_field_rows != len(rows) and not args.allow_legacy_decision_log:
        raise RuntimeError(
            f"only {new_field_rows}/{len(rows)} rows have new regime fields; "
            "rerun with updated pm-app or pass --allow-legacy-decision-log for smoke tests"
        )

    out_jsonl = Path(args.out_jsonl)
    out_jsonl.parent.mkdir(parents=True, exist_ok=True)
    with out_jsonl.open("w") as file:
        for row in rows:
            file.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")

    split_idx = max(1, min(len(rows) - 1, int(len(rows) * args.train_frac)))
    write_decision_report(
        Path(args.out_md),
        rows,
        sorted(loaded),
        split_idx,
        args.starting_cash,
    )
    print(
        f"wrote {len(rows)} rows to {out_jsonl}; "
        f"new_regime_field_rows={new_field_rows}/{len(rows)}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
