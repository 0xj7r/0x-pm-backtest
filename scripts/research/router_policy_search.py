#!/usr/bin/env python3
"""Search chronological router policies over a market-level router dataset."""

from __future__ import annotations

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any


def fmt_usd(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{value:.2f}%"


def load_rows(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open() as file:
        for line in file:
            if line.strip():
                rows.append(json.loads(line))
    rows.sort(key=lambda row: int(row["close_ts"]))
    return rows


def candidate_names(rows: list[dict[str, Any]]) -> list[str]:
    return sorted(rows[0]["labels"]["candidate_pnl_usdc"])


def pnl(row: dict[str, Any], candidate: str) -> float:
    return float(row["labels"]["candidate_pnl_usdc"][candidate])


def equity_stats(pnls: list[float], starting_cash: float) -> dict[str, float]:
    equity = starting_cash
    peak = starting_cash
    max_dd = 0.0
    worst = 0.0
    best = 0.0
    wins = 0
    losses = 0
    for value in pnls:
        wins += value > 0.0
        losses += value < 0.0
        worst = min(worst, value)
        best = max(best, value)
        equity += value
        peak = max(peak, equity)
        if peak > 0.0:
            max_dd = max(max_dd, (peak - equity) / peak)
    sorted_pnls = sorted(pnls)
    q05 = sorted_pnls[max(0, int(len(sorted_pnls) * 0.05) - 1)] if sorted_pnls else 0.0
    tail = [value for value in pnls if value <= q05]
    cvar05 = sum(tail) / len(tail) if tail else 0.0
    return {
        "markets": float(len(pnls)),
        "pnl": equity - starting_cash,
        "end_equity": equity,
        "max_dd_pct": max_dd * 100.0,
        "mean": (equity - starting_cash) / len(pnls) if pnls else 0.0,
        "win_rate": wins / len(pnls) if pnls else 0.0,
        "loss_rate": losses / len(pnls) if pnls else 0.0,
        "worst": worst,
        "best": best,
        "q05": q05,
        "cvar05": cvar05,
    }


def objective(stats: dict[str, float], dd_penalty: float, cvar_penalty: float) -> float:
    return stats["pnl"] - dd_penalty * stats["max_dd_pct"] + cvar_penalty * stats["cvar05"]


def train_cluster_policy(
    train: list[dict[str, Any]],
    names: list[str],
    min_train_markets: int,
) -> tuple[dict[str, str], str]:
    global_pnl = {name: sum(pnl(row, name) for row in train) for name in names}
    default_route = max(names, key=global_pnl.get)
    by_cluster: dict[str, dict[str, list[float]]] = defaultdict(lambda: defaultdict(list))
    for row in train:
        cluster = row["diagnostic_cluster"]
        for name in names:
            by_cluster[cluster][name].append(pnl(row, name))
    policy: dict[str, str] = {}
    for cluster, values_by_name in by_cluster.items():
        count = len(next(iter(values_by_name.values())))
        if count < min_train_markets:
            policy[cluster] = default_route
        else:
            policy[cluster] = max(names, key=lambda name: sum(values_by_name[name]))
    return policy, default_route


def prior_mean(row: dict[str, Any], candidate: str, window: int, same_cluster: bool) -> float:
    prefix = "prior_same_cluster" if same_cluster else "prior"
    key = f"{prefix}_{window}_{candidate}_mean_pnl_usdc"
    return float(row["no_lookahead_features"].get(key) or 0.0)


def prior_count(row: dict[str, Any], window: int, same_cluster: bool) -> int:
    prefix = "prior_same_cluster" if same_cluster else "prior"
    return int(row["no_lookahead_features"].get(f"{prefix}_{window}_markets") or 0)


def route_pnls(
    test: list[dict[str, Any]],
    names: list[str],
    family: str,
    cluster_policy: dict[str, str],
    default_route: str,
    window: int,
    same_cluster: bool,
    risk_off_threshold: float,
    min_prior_markets: int,
    cluster_weight: float,
    rolling_weight: float,
) -> tuple[list[float], dict[str, int]]:
    counts: dict[str, int] = defaultdict(int)
    out: list[float] = []
    for row in test:
        if family == "fixed":
            route = default_route
            score = 1.0
        elif family == "cluster":
            route = cluster_policy.get(row["diagnostic_cluster"], default_route)
            score = 1.0
        elif family in {"rolling", "same_cluster_rolling", "hybrid"}:
            use_same_cluster = same_cluster or family == "same_cluster_rolling"
            enough = prior_count(row, window, use_same_cluster) >= min_prior_markets
            if not enough:
                route = default_route if risk_off_threshold < 0.0 else "risk_off"
                score = -math.inf if route == "risk_off" else 0.0
            else:
                scores: dict[str, float] = {}
                for name in names:
                    rolling = prior_mean(row, name, window, use_same_cluster)
                    cluster_route_bonus = 1.0 if cluster_policy.get(row["diagnostic_cluster"], default_route) == name else 0.0
                    scores[name] = rolling_weight * rolling + cluster_weight * cluster_route_bonus
                route = max(names, key=lambda name: scores[name])
                score = scores[route]
        else:
            raise ValueError(f"unknown family {family}")

        if route == "risk_off" or score <= risk_off_threshold:
            out.append(0.0)
            counts["risk_off"] += 1
        else:
            out.append(pnl(row, route))
            counts[route] += 1
    return out, counts


def fold_slices(rows: list[dict[str, Any]], train_size: int, test_size: int, step_size: int) -> list[tuple[int, int, int]]:
    slices: list[tuple[int, int, int]] = []
    start = 0
    while start + train_size + test_size <= len(rows):
        train_end = start + train_size
        test_end = train_end + test_size
        slices.append((start, train_end, test_end))
        start += step_size
    if not slices:
        raise RuntimeError("not enough rows for requested fold sizes")
    return slices


def evaluate_policy(
    rows: list[dict[str, Any]],
    names: list[str],
    folds: list[tuple[int, int, int]],
    params: dict[str, Any],
    starting_cash: float,
) -> dict[str, Any]:
    all_pnls: list[float] = []
    route_counts: dict[str, int] = defaultdict(int)
    fold_rows: list[dict[str, Any]] = []
    for start, train_end, test_end in folds:
        train = rows[start:train_end]
        test = rows[train_end:test_end]
        cluster_policy, default_route = train_cluster_policy(
            train,
            names,
            params["min_train_markets"],
        )
        pnls, counts = route_pnls(
            test,
            names,
            params["family"],
            cluster_policy,
            default_route,
            params["window"],
            params["same_cluster"],
            params["risk_off_threshold"],
            params["min_prior_markets"],
            params["cluster_weight"],
            params["rolling_weight"],
        )
        for key, value in counts.items():
            route_counts[key] += value
        all_pnls.extend(pnls)
        stats = equity_stats(pnls, starting_cash)
        fold_rows.append(
            {
                "train_start": rows[start]["date"],
                "train_end": rows[train_end - 1]["date"],
                "test_start": rows[train_end]["date"],
                "test_end": rows[test_end - 1]["date"],
                "pnl": stats["pnl"],
                "max_dd_pct": stats["max_dd_pct"],
                "cvar05": stats["cvar05"],
            }
        )
    stats = equity_stats(all_pnls, starting_cash)
    return {
        "params": params,
        "stats": stats,
        "route_counts": dict(route_counts),
        "folds": fold_rows,
    }


def params_grid(windows: list[int], names: list[str]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for name in names:
        out.append(
            {
                "label": f"fixed_{name}",
                "family": "fixed",
                "window": windows[0],
                "same_cluster": False,
                "risk_off_threshold": -1.0,
                "min_prior_markets": 0,
                "min_train_markets": 20,
                "cluster_weight": 0.0,
                "rolling_weight": 0.0,
                "fixed_route": name,
            }
        )
    for min_train in (20, 50, 100):
        out.append(
            {
                "label": f"cluster_min{min_train}",
                "family": "cluster",
                "window": windows[0],
                "same_cluster": False,
                "risk_off_threshold": -1.0,
                "min_prior_markets": 0,
                "min_train_markets": min_train,
                "cluster_weight": 0.0,
                "rolling_weight": 0.0,
            }
        )
    for window in windows:
        for family, same_cluster, min_prior_options in (
            ("rolling", False, (20, 100, 288)),
            ("same_cluster_rolling", True, (20, 50, 100)),
            ("hybrid", True, (20, 50, 100)),
        ):
            for min_prior in min_prior_options:
                for risk_off_threshold in (-1.0, -0.05, 0.0, 0.05, 0.10):
                    weights = [(0.0, 1.0)]
                    if family == "hybrid":
                        weights = [(0.10, 1.0), (0.25, 1.0), (0.50, 1.0)]
                    for cluster_weight, rolling_weight in weights:
                        out.append(
                            {
                                "label": (
                                    f"{family}_w{window}_min{min_prior}_"
                                    f"risk{risk_off_threshold:g}_cw{cluster_weight:g}"
                                ),
                                "family": family,
                                "window": window,
                                "same_cluster": same_cluster,
                                "risk_off_threshold": risk_off_threshold,
                                "min_prior_markets": min_prior,
                                "min_train_markets": 20,
                                "cluster_weight": cluster_weight,
                                "rolling_weight": rolling_weight,
                            }
                        )
    return out


def write_report(path: Path, results: list[dict[str, Any]], rows: list[dict[str, Any]], folds: list[tuple[int, int, int]], objective_name: str) -> None:
    lines = [
        "# Router Policy Search",
        "",
        f"Dataset rows: `{len(rows)}`",
        f"Date range: `{rows[0]['date']}` to `{rows[-1]['date']}`",
        f"Folds: `{len(folds)}`",
        f"Objective: `{objective_name}`",
        "",
        "Policies are trained only on each fold's chronological train window and scored on the following test window.",
        "`diagnostic_cluster` is still fill-derived in the current dataset; rolling policies use only prior labels.",
        "",
        "## Top Policies",
        "",
        "| Rank | Policy | Objective | PnL | Max DD | CVaR 5% | Mean | Win Rate | Worst | Routes |",
        "|---:|---|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for idx, result in enumerate(results[:25], start=1):
        stats = result["stats"]
        routes = ", ".join(f"{key}={value}" for key, value in sorted(result["route_counts"].items()))
        lines.append(
            f"| {idx} | {result['params']['label']} | {result['objective']:.2f} | "
            f"{fmt_usd(stats['pnl'])} | {pct(stats['max_dd_pct'])} | {fmt_usd(stats['cvar05'])} | "
            f"{fmt_usd(stats['mean'])} | {stats['win_rate']:.1%} | {fmt_usd(stats['worst'])} | {routes} |"
        )

    best = results[0]
    lines.extend(["", "## Best Policy Folds", ""])
    lines.append("| Fold | Train Range | Test Range | PnL | Max DD | CVaR 5% |")
    lines.append("|---:|---|---|---:|---:|---:|")
    for idx, fold in enumerate(best["folds"], start=1):
        lines.append(
            f"| {idx} | {fold['train_start']} to {fold['train_end']} | "
            f"{fold['test_start']} to {fold['test_end']} | {fmt_usd(fold['pnl'])} | "
            f"{pct(fold['max_dd_pct'])} | {fmt_usd(fold['cvar05'])} |"
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dataset_jsonl")
    parser.add_argument("--train-size", type=int, default=2600)
    parser.add_argument("--test-size", type=int, default=650)
    parser.add_argument("--step-size", type=int, default=650)
    parser.add_argument("--starting-cash", type=float, default=2700.0)
    parser.add_argument("--dd-penalty", type=float, default=30.0)
    parser.add_argument("--cvar-penalty", type=float, default=10.0)
    parser.add_argument("--window", action="append", type=int, dest="windows")
    parser.add_argument("--out-md", required=True)
    parser.add_argument("--out-json", required=True)
    args = parser.parse_args()

    rows = load_rows(Path(args.dataset_jsonl))
    names = candidate_names(rows)
    windows = sorted(set(args.windows or [288, 864, 2016]))
    folds = fold_slices(rows, args.train_size, args.test_size, args.step_size)
    results: list[dict[str, Any]] = []
    for params in params_grid(windows, names):
        if params["family"] == "fixed":
            # Preserve the fixed route by ordering the default training winner.
            fixed = params["fixed_route"]
            fixed_rows = []
            for start, train_end, test_end in folds:
                test = rows[train_end:test_end]
                fixed_rows.extend(pnl(row, fixed) for row in test)
            stats = equity_stats(fixed_rows, args.starting_cash)
            result = {
                "params": params,
                "stats": stats,
                "route_counts": {fixed: len(fixed_rows)},
                "folds": [],
            }
        else:
            result = evaluate_policy(rows, names, folds, params, args.starting_cash)
        result["objective"] = objective(result["stats"], args.dd_penalty, args.cvar_penalty)
        results.append(result)

    results.sort(key=lambda result: result["objective"], reverse=True)
    write_report(
        Path(args.out_md),
        results,
        rows,
        folds,
        f"pnl - {args.dd_penalty:g}*max_dd_pct + {args.cvar_penalty:g}*cvar05",
    )
    Path(args.out_json).write_text(json.dumps(results[:50], indent=2, sort_keys=True) + "\n")
    print(f"searched {len(results)} policies across {len(folds)} folds")
    print(f"best: {results[0]['params']['label']} objective={results[0]['objective']:.2f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
