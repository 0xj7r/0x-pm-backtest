#!/usr/bin/env python3
"""Build a market-level strategy-router dataset from overlapping artifacts.

The output JSONL has one row per shared market.  It carries per-strategy PnL
labels plus two feature groups:

* `diagnostic_fill_summary_features`: current-market features summarized from
  `fills_detail`.  These are useful for discovery, but are not deployable as a
  pre-route input unless the same fields are emitted by a shared market-state
  layer before strategy selection.
* `no_lookahead_features`: rolling prior performance features computed only
  from markets before the current row.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any

from strategy_regime_clusters import FEATURES, cluster_label, weighted_feature


DEFAULT_WINDOWS = (288, 864, 2016)


def parse_candidate(value: str) -> tuple[str, str, Path]:
    try:
        name, rest = value.split("=", 1)
        strategy, raw_path = rest.split(":", 1)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            "candidate must be NAME=STRATEGY:path/to/markets.jsonl"
        ) from exc
    if not name or not strategy or not raw_path:
        raise argparse.ArgumentTypeError(
            "candidate must be NAME=STRATEGY:path/to/markets.jsonl"
        )
    return name, strategy, Path(raw_path)


def close_ts(row: dict[str, Any]) -> int:
    value = row.get("close_ts")
    if value is not None:
        return int(value)
    slug = str(row.get("slug") or "")
    return int(slug.rsplit("-", 1)[1]) + 300


def finite_float(value: Any, default: float = 0.0) -> float:
    if isinstance(value, (int, float)) and math.isfinite(float(value)):
        return float(value)
    return default


def mean(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def fmt_usd(value: float) -> str:
    return f"${value:,.2f}"


def fmt_pct(value: float) -> str:
    return f"{value:.2f}%"


def load_candidate(name: str, strategy: str, path: Path) -> dict[str, dict[str, Any]]:
    rows: dict[str, dict[str, Any]] = {}
    with path.open() as file:
        for line in file:
            if not line.strip():
                continue
            row = json.loads(line)
            strat = (row.get("per_strategy") or {}).get(strategy) or {}
            fills = strat.get("fills_detail") or []
            features = {key: weighted_feature(fills, key) for key in FEATURES}
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
                "features": features,
            }
    return rows


def feature_source_rows(candidates: dict[str, dict[str, Any]], source: str) -> list[dict[str, Any]]:
    if source == "union":
        return list(candidates.values())
    row = candidates.get(source)
    return [row] if row is not None else []


def fill_summary_features(candidates: dict[str, dict[str, Any]], source: str) -> dict[str, float | None]:
    rows = [row for row in feature_source_rows(candidates, source) if row["fills"] > 0]
    out: dict[str, float | None] = {}
    for key in FEATURES:
        values = [
            float(row["features"][key])
            for row in rows
            if row["features"].get(key) is not None and math.isfinite(float(row["features"][key]))
        ]
        out[key] = mean(values)
    return out


def cluster_for(candidates: dict[str, dict[str, Any]], source: str) -> str:
    rows = [row for row in feature_source_rows(candidates, source) if row["fills"] > 0]
    if not rows:
        return "no_trade"
    return cluster_label(fill_summary_features(candidates, source))


def equity_stats(pnls: list[float], starting_cash: float) -> dict[str, float]:
    equity = starting_cash
    peak = starting_cash
    max_dd = 0.0
    wins = 0
    losses = 0
    worst = 0.0
    best = 0.0
    for pnl in pnls:
        wins += 1 if pnl > 0.0 else 0
        losses += 1 if pnl < 0.0 else 0
        worst = min(worst, pnl)
        best = max(best, pnl)
        equity += pnl
        peak = max(peak, equity)
        if peak > 0.0:
            max_dd = max(max_dd, (peak - equity) / peak)
    return {
        "markets": float(len(pnls)),
        "pnl": equity - starting_cash,
        "end_equity": equity,
        "max_dd_pct": max_dd * 100.0,
        "win_rate": wins / len(pnls) if pnls else 0.0,
        "loss_rate": losses / len(pnls) if pnls else 0.0,
        "worst": worst,
        "best": best,
        "mean": (equity - starting_cash) / len(pnls) if pnls else 0.0,
    }


def rolling_stats(history: list[dict[str, Any]], names: list[str], windows: list[int]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for window in windows:
        recent = history[-window:]
        out[f"prior_{window}_markets"] = len(recent)
        for name in names:
            pnls = [float(row["labels"]["candidate_pnl_usdc"][name]) for row in recent]
            fills = [int(row["candidates"][name]["fills"]) for row in recent]
            wins = sum(1 for pnl in pnls if pnl > 0.0)
            losses = sum(1 for pnl in pnls if pnl < 0.0)
            traded = sum(1 for fill_count in fills if fill_count > 0)
            out[f"prior_{window}_{name}_pnl_usdc"] = sum(pnls)
            out[f"prior_{window}_{name}_mean_pnl_usdc"] = sum(pnls) / len(pnls) if pnls else 0.0
            out[f"prior_{window}_{name}_win_rate"] = wins / len(pnls) if pnls else 0.0
            out[f"prior_{window}_{name}_loss_rate"] = losses / len(pnls) if pnls else 0.0
            out[f"prior_{window}_{name}_trade_rate"] = traded / len(fills) if fills else 0.0
    return out


def add_rolling_features(rows: list[dict[str, Any]], names: list[str], windows: list[int]) -> None:
    history: list[dict[str, Any]] = []
    cluster_history: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        features = rolling_stats(history, names, windows)
        same_cluster_history = cluster_history[row["diagnostic_cluster"]]
        for window in windows:
            recent = same_cluster_history[-window:]
            features[f"prior_same_cluster_{window}_markets"] = len(recent)
            for name in names:
                pnls = [float(prior["labels"]["candidate_pnl_usdc"][name]) for prior in recent]
                features[f"prior_same_cluster_{window}_{name}_pnl_usdc"] = sum(pnls)
                features[f"prior_same_cluster_{window}_{name}_mean_pnl_usdc"] = (
                    sum(pnls) / len(pnls) if pnls else 0.0
                )
        row["no_lookahead_features"] = features
        history.append(row)
        cluster_history[row["diagnostic_cluster"]].append(row)


def make_rows(
    candidates_by_name: dict[str, dict[str, dict[str, Any]]],
    feature_source: str,
    windows: list[int],
) -> list[dict[str, Any]]:
    names = sorted(candidates_by_name)
    shared_slugs = set.intersection(*(set(rows) for rows in candidates_by_name.values()))
    rows: list[dict[str, Any]] = []
    for slug in shared_slugs:
        candidates = {name: candidates_by_name[name][slug] for name in names}
        ts = max(row["close_ts"] for row in candidates.values())
        date = dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat()
        pnls = {name: candidates[name]["pnl_usdc"] for name in names}
        best_candidate = max(names, key=lambda name: pnls[name])
        sorted_pnls = sorted(pnls.values(), reverse=True)
        features = fill_summary_features(candidates, feature_source)
        row = {
            "schema_version": 1,
            "slug": slug,
            "close_ts": ts,
            "date": date,
            "feature_source": feature_source,
            "feature_source_is_diagnostic": True,
            "diagnostic_cluster": cluster_for(candidates, feature_source),
            "diagnostic_fill_summary_features": features,
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


def policy_cluster_train_mean(
    train: list[dict[str, Any]],
    test: list[dict[str, Any]],
    names: list[str],
    min_train_markets: int,
) -> tuple[dict[str, str], list[float]]:
    global_pnl = {name: sum(row["labels"]["candidate_pnl_usdc"][name] for row in train) for name in names}
    default = max(names, key=global_pnl.get)
    per_cluster: dict[str, dict[str, list[float]]] = defaultdict(lambda: defaultdict(list))
    for row in train:
        for name in names:
            per_cluster[row["diagnostic_cluster"]][name].append(row["labels"]["candidate_pnl_usdc"][name])

    policy: dict[str, str] = {}
    for cluster, by_name in per_cluster.items():
        if len(next(iter(by_name.values()))) < min_train_markets:
            policy[cluster] = default
        else:
            policy[cluster] = max(names, key=lambda name: sum(by_name[name]))

    pnls = [
        row["labels"]["candidate_pnl_usdc"][policy.get(row["diagnostic_cluster"], default)]
        for row in test
    ]
    return policy, pnls


def policy_rolling_mean(
    rows: list[dict[str, Any]],
    split_idx: int,
    names: list[str],
    window: int,
    risk_off_when_all_negative: bool,
) -> list[float]:
    pnls: list[float] = []
    for row in rows[split_idx:]:
        features = row["no_lookahead_features"]
        means = {
            name: float(features.get(f"prior_{window}_{name}_mean_pnl_usdc") or 0.0)
            for name in names
        }
        if risk_off_when_all_negative and max(means.values()) <= 0.0:
            pnls.append(0.0)
            continue
        route = max(names, key=lambda name: means[name])
        pnls.append(row["labels"]["candidate_pnl_usdc"][route])
    return pnls


def policy_same_cluster_rolling_mean(
    rows: list[dict[str, Any]],
    split_idx: int,
    names: list[str],
    window: int,
    min_prior_cluster_markets: int,
    risk_off_when_all_negative: bool,
) -> list[float]:
    pnls: list[float] = []
    for row in rows[split_idx:]:
        features = row["no_lookahead_features"]
        if int(features.get(f"prior_same_cluster_{window}_markets") or 0) < min_prior_cluster_markets:
            pnls.append(0.0 if risk_off_when_all_negative else row["labels"]["candidate_pnl_usdc"][names[0]])
            continue
        means = {
            name: float(features.get(f"prior_same_cluster_{window}_{name}_mean_pnl_usdc") or 0.0)
            for name in names
        }
        if risk_off_when_all_negative and max(means.values()) <= 0.0:
            pnls.append(0.0)
            continue
        route = max(names, key=lambda name: means[name])
        pnls.append(row["labels"]["candidate_pnl_usdc"][route])
    return pnls


def write_report(
    path: Path,
    rows: list[dict[str, Any]],
    names: list[str],
    split_idx: int,
    starting_cash: float,
    windows: list[int],
    policy: dict[str, str],
    cluster_pnls: list[float],
) -> None:
    train = rows[:split_idx]
    test = rows[split_idx:]
    lines = [
        "# Router Market Dataset",
        "",
        f"Rows: `{len(rows)}`",
        f"Train rows: `{len(train)}`",
        f"Test rows: `{len(test)}`",
        f"Date range: `{rows[0]['date']}` to `{rows[-1]['date']}`",
        f"Feature source: `{rows[0]['feature_source']}`",
        "",
        "`diagnostic_fill_summary_features` are fill-derived from current-market artifacts.",
        "They are useful for discovery but are not a deployable pre-route feature source yet.",
        "`no_lookahead_features` are computed only from prior markets.",
        "",
        "## Test Policies",
        "",
        "| Policy | Test PnL | End Equity | Max DD | Mean / Market | Win Rate | Worst | Best |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]

    summaries: dict[str, dict[str, float]] = {}
    for name in names:
        summaries[name] = equity_stats(
            [row["labels"]["candidate_pnl_usdc"][name] for row in test],
            starting_cash,
        )
    summaries["diagnostic_cluster_train_mean"] = equity_stats(cluster_pnls, starting_cash)
    summaries["oracle_best_per_market"] = equity_stats(
        [max(row["labels"]["candidate_pnl_usdc"][name] for name in names) for row in test],
        starting_cash,
    )
    summaries["risk_off"] = equity_stats([0.0 for _ in test], starting_cash)
    for window in windows:
        summaries[f"prior_{window}_rolling_mean"] = equity_stats(
            policy_rolling_mean(rows, split_idx, names, window, False),
            starting_cash,
        )
        summaries[f"prior_{window}_rolling_mean_risk_off"] = equity_stats(
            policy_rolling_mean(rows, split_idx, names, window, True),
            starting_cash,
        )
        summaries[f"prior_same_cluster_{window}_mean_risk_off"] = equity_stats(
            policy_same_cluster_rolling_mean(rows, split_idx, names, window, 20, True),
            starting_cash,
        )

    for name, stats in summaries.items():
        lines.append(
            f"| {name} | {fmt_usd(stats['pnl'])} | {fmt_usd(stats['end_equity'])} | "
            f"{fmt_pct(stats['max_dd_pct'])} | {fmt_usd(stats['mean'])} | "
            f"{stats['win_rate']:.1%} | {fmt_usd(stats['worst'])} | {fmt_usd(stats['best'])} |"
        )

    lines.extend(["", "## Learned Diagnostic Cluster Policy", ""])
    lines.append("| Cluster | Route | Train Markets | Train PnL By Candidate | Test Markets |")
    lines.append("|---|---|---:|---|---:|")
    by_cluster_train: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    count_train: dict[str, int] = defaultdict(int)
    count_test: dict[str, int] = defaultdict(int)
    for row in train:
        count_train[row["diagnostic_cluster"]] += 1
        for name in names:
            by_cluster_train[row["diagnostic_cluster"]][name] += row["labels"]["candidate_pnl_usdc"][name]
    for row in test:
        count_test[row["diagnostic_cluster"]] += 1
    for cluster in sorted(set(count_train) | set(count_test)):
        pnl_bits = ", ".join(
            f"{name}={fmt_usd(by_cluster_train[cluster][name])}" for name in names
        )
        lines.append(
            f"| {cluster} | {policy.get(cluster, '-')} | {count_train[cluster]} | "
            f"{pnl_bits} | {count_test[cluster]} |"
        )

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
    parser.add_argument("--feature-source", default="union", help="candidate name or 'union'")
    parser.add_argument("--train-frac", type=float, default=0.60)
    parser.add_argument("--min-train-markets", type=int, default=20)
    parser.add_argument("--starting-cash", type=float, default=2700.0)
    parser.add_argument("--rolling-window", action="append", type=int, dest="rolling_windows")
    parser.add_argument("--out-jsonl", required=True)
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    windows = sorted(set(args.rolling_windows or DEFAULT_WINDOWS))
    loaded = {
        name: load_candidate(name, strategy, path)
        for name, strategy, path in args.candidate
    }
    if len(loaded) < 2:
        raise RuntimeError("need at least two candidates")
    rows = make_rows(loaded, args.feature_source, windows)
    if len(rows) < 100:
        raise RuntimeError(f"not enough overlapping markets: {len(rows)}")

    split_idx = max(1, min(len(rows) - 1, int(len(rows) * args.train_frac)))
    names = sorted(loaded)
    policy, cluster_pnls = policy_cluster_train_mean(
        rows[:split_idx],
        rows[split_idx:],
        names,
        args.min_train_markets,
    )

    out_jsonl = Path(args.out_jsonl)
    out_jsonl.parent.mkdir(parents=True, exist_ok=True)
    with out_jsonl.open("w") as file:
        for row in rows:
            file.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")

    write_report(
        Path(args.out_md),
        rows,
        names,
        split_idx,
        args.starting_cash,
        windows,
        policy,
        cluster_pnls,
    )
    print(f"wrote {len(rows)} rows to {out_jsonl}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
