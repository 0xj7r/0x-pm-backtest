#!/usr/bin/env python3
"""Search synthetic BackToExplore cluster throttle/sizing policies.

This is an offline policy-layer experiment. It does not claim that scaled PnL is
an exact engine rerun; it tests whether cluster + no-lookahead BTE performance
signals are strong enough to justify a real BTE specialist implementation.
"""

from __future__ import annotations

import argparse
import hashlib
import json
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np


def fmt_usd(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{value:.2f}%"


def compact_stats(stats: dict[str, float]) -> dict[str, float]:
    return {
        "markets": stats["markets"],
        "pnl_usdc": stats["pnl"],
        "end_equity_usdc": stats["end_equity"],
        "max_dd_pct": stats["max_dd_pct"],
        "mean_pnl_usdc": stats["mean"],
        "win_rate": stats["win_rate"],
        "loss_rate": stats["loss_rate"],
        "worst_market_pnl_usdc": stats["worst"],
        "best_market_pnl_usdc": stats["best"],
        "q05_market_pnl_usdc": stats["q05"],
        "cvar05_market_pnl_usdc": stats["cvar05"],
    }


def load_rows(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    with path.open() as file:
        for line in file:
            if line.strip():
                rows.append(json.loads(line))
    rows.sort(key=lambda row: int(row["close_ts"]))
    return rows


def bte_pnl(row: dict[str, Any], candidate: str) -> float:
    return float(row["labels"]["candidate_pnl_usdc"].get(candidate, 0.0))


def equity_stats(pnls: np.ndarray, starting_cash: float) -> dict[str, float]:
    if pnls.size == 0:
        return {
            "markets": 0.0,
            "pnl": 0.0,
            "end_equity": starting_cash,
            "max_dd_pct": 0.0,
            "mean": 0.0,
            "win_rate": 0.0,
            "loss_rate": 0.0,
            "worst": 0.0,
            "best": 0.0,
            "q05": 0.0,
            "cvar05": 0.0,
        }
    equity_curve = starting_cash + np.cumsum(pnls)
    peaks = np.maximum.accumulate(np.concatenate(([starting_cash], equity_curve)))[:-1]
    dd = np.where(peaks > 0.0, (peaks - equity_curve) / peaks, 0.0)
    q05 = float(np.quantile(pnls, 0.05, method="lower"))
    tail = pnls[pnls <= q05]
    return {
        "markets": float(pnls.size),
        "pnl": float(pnls.sum()),
        "end_equity": float(starting_cash + pnls.sum()),
        "max_dd_pct": float(dd.max() * 100.0),
        "mean": float(pnls.mean()),
        "win_rate": float((pnls > 0.0).mean()),
        "loss_rate": float((pnls < 0.0).mean()),
        "worst": float(pnls.min()),
        "best": float(pnls.max()),
        "q05": q05,
        "cvar05": float(tail.mean()) if tail.size else 0.0,
    }


def objective(stats: dict[str, float], dd_penalty: float, cvar_penalty: float) -> float:
    return stats["pnl"] - dd_penalty * stats["max_dd_pct"] + cvar_penalty * stats["cvar05"]


def fold_slices(rows: list[dict[str, Any]], train_size: int, test_size: int, step_size: int) -> list[tuple[int, int, int]]:
    out: list[tuple[int, int, int]] = []
    start = 0
    while start + train_size + test_size <= len(rows):
        train_end = start + train_size
        test_end = train_end + test_size
        out.append((start, train_end, test_end))
        start += step_size
    if not out:
        raise RuntimeError("not enough rows for requested fold sizes")
    return out


def train_cluster_means(train: list[dict[str, Any]], candidate: str) -> dict[str, float]:
    by_cluster: dict[str, list[float]] = defaultdict(list)
    for row in train:
        by_cluster[str(row["diagnostic_cluster"])].append(bte_pnl(row, candidate))
    return {
        cluster: sum(values) / len(values)
        for cluster, values in by_cluster.items()
        if values
    }


def prior_mean(row: dict[str, Any], candidate: str, window: int, same_cluster: bool) -> float:
    prefix = "prior_same_cluster" if same_cluster else "prior"
    return float(row["no_lookahead_features"].get(f"{prefix}_{window}_{candidate}_mean_pnl_usdc") or 0.0)


def prior_count(row: dict[str, Any], window: int, same_cluster: bool) -> int:
    prefix = "prior_same_cluster" if same_cluster else "prior"
    return int(row["no_lookahead_features"].get(f"{prefix}_{window}_markets") or 0)


@dataclass(frozen=True)
class FoldData:
    train_start: str
    train_end: str
    test_start: str
    test_end: str
    test_rows: list[dict[str, Any]]
    raw_pnl: np.ndarray
    cluster_mean: np.ndarray
    clusters: list[str]
    prior_mean_by_key: dict[tuple[int, bool], np.ndarray]
    prior_count_by_key: dict[tuple[int, bool], np.ndarray]


def prepare_folds(
    rows: list[dict[str, Any]],
    folds: list[tuple[int, int, int]],
    candidate: str,
    windows: list[int],
) -> list[FoldData]:
    out: list[FoldData] = []
    for start, train_end, test_end in folds:
        train = rows[start:train_end]
        test = rows[train_end:test_end]
        cluster_means = train_cluster_means(train, candidate)
        prior_mean_by_key: dict[tuple[int, bool], np.ndarray] = {}
        prior_count_by_key: dict[tuple[int, bool], np.ndarray] = {}
        for window in windows:
            for same_cluster in (False, True):
                prior_mean_by_key[(window, same_cluster)] = np.asarray(
                    [prior_mean(row, candidate, window, same_cluster) for row in test],
                    dtype=np.float64,
                )
                prior_count_by_key[(window, same_cluster)] = np.asarray(
                    [prior_count(row, window, same_cluster) for row in test],
                    dtype=np.int32,
                )
        out.append(
            FoldData(
                train_start=rows[start]["date"],
                train_end=rows[train_end - 1]["date"],
                test_start=rows[train_end]["date"],
                test_end=rows[test_end - 1]["date"],
                test_rows=test,
                raw_pnl=np.asarray([bte_pnl(row, candidate) for row in test], dtype=np.float64),
                cluster_mean=np.asarray(
                    [cluster_means.get(str(row["diagnostic_cluster"]), 0.0) for row in test],
                    dtype=np.float64,
                ),
                clusters=[str(row["diagnostic_cluster"]) for row in test],
                prior_mean_by_key=prior_mean_by_key,
                prior_count_by_key=prior_count_by_key,
            )
        )
    return out


def scales_for_fold(fold: FoldData, params: dict[str, Any]) -> np.ndarray:
    scale = np.ones(fold.raw_pnl.size, dtype=np.float64)
    scale = np.where(fold.cluster_mean <= params["cluster_off_mean"], params["cluster_off_scale"], scale)
    scale = np.where(
        (fold.cluster_mean > params["cluster_off_mean"])
        & (fold.cluster_mean <= params["cluster_throttle_mean"]),
        np.minimum(scale, params["throttle_scale"]),
        scale,
    )
    scale = np.where(fold.cluster_mean >= params["cluster_boost_mean"], np.maximum(scale, params["boost_scale"]), scale)

    key = (params["window"], bool(params["same_cluster"]))
    prior_count_arr = fold.prior_count_by_key[key]
    prior_mean_arr = fold.prior_mean_by_key[key]
    cold = prior_count_arr < params["min_prior_markets"]
    if params["cold_start_risk_on"]:
        eligible_scale = scale
    else:
        eligible_scale = np.where(cold, 0.0, scale)
    eligible_scale = np.where((~cold) & (prior_mean_arr <= params["recent_off_mean"]), 0.0, eligible_scale)
    eligible_scale = np.where(
        (~cold)
        & (prior_mean_arr > params["recent_off_mean"])
        & (prior_mean_arr <= params["recent_throttle_mean"]),
        np.minimum(eligible_scale, params["throttle_scale"]),
        eligible_scale,
    )
    eligible_scale = np.where(
        (~cold) & (prior_mean_arr >= params["recent_boost_mean"]),
        np.maximum(eligible_scale, params["boost_scale"]),
        eligible_scale,
    )
    return eligible_scale


def evaluate(
    fold_data: list[FoldData],
    params: dict[str, Any],
    starting_cash: float,
) -> dict[str, Any]:
    pnls: list[np.ndarray] = []
    scales: Counter[str] = Counter()
    folds_out: list[dict[str, Any]] = []
    by_cluster: dict[str, float] = defaultdict(float)
    scale_chunks: list[bytes] = []

    for fold in fold_data:
        scale = scales_for_fold(fold, params)
        scale_chunks.append(np.round(scale, 6).astype(np.float32).tobytes())
        fold_pnls = fold.raw_pnl * scale
        for scale_value, count in zip(*np.unique(np.round(scale, 6), return_counts=True), strict=True):
            scale_key = f"{float(scale_value):.2f}".rstrip("0").rstrip(".")
            scales[scale_key] += int(count)
        for cluster in sorted(set(fold.clusters)):
            mask = np.asarray([value == cluster for value in fold.clusters])
            by_cluster[cluster] += float(fold_pnls[mask].sum())
        pnls.append(fold_pnls)
        stats = equity_stats(fold_pnls, starting_cash)
        folds_out.append(
            {
                "train_start": fold.train_start,
                "train_end": fold.train_end,
                "test_start": fold.test_start,
                "test_end": fold.test_end,
                "pnl": stats["pnl"],
                "max_dd_pct": stats["max_dd_pct"],
                "cvar05": stats["cvar05"],
            }
        )

    return {
        "params": params,
        "stats": equity_stats(np.concatenate(pnls) if pnls else np.asarray([], dtype=np.float64), starting_cash),
        "scale_counts": dict(sorted(scales.items(), key=lambda item: float(item[0]))),
        "scale_signature": hashlib.sha256(b"".join(scale_chunks)).hexdigest(),
        "cluster_pnl": dict(sorted(by_cluster.items())),
        "folds": folds_out,
    }


def params_grid(windows: list[int]) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = [
        {
            "label": "fixed_bte",
            "window": windows[0],
            "same_cluster": False,
            "min_prior_markets": 0,
            "cold_start_risk_on": True,
            "cluster_off_mean": -1e9,
            "cluster_off_scale": 1.0,
            "cluster_throttle_mean": -1e9,
            "cluster_boost_mean": 1e9,
            "recent_off_mean": -1e9,
            "recent_throttle_mean": -1e9,
            "recent_boost_mean": 1e9,
            "throttle_scale": 1.0,
            "boost_scale": 1.0,
        }
    ]
    for window in windows:
        for same_cluster in (False, True):
            for min_prior in (20, 100, 288):
                for cold_start in (False, True):
                    for cluster_off_mean in (-0.20, 0.0):
                        for cluster_off_scale in (0.0, 0.50):
                            for recent_off_mean in (-0.20, 0.0, 0.10):
                                for recent_throttle_mean in (0.0, 0.25):
                                    for recent_boost_mean in (0.50, 1.00):
                                        for throttle_scale in (0.50, 0.75):
                                            for boost_scale in (1.0, 1.50):
                                                if recent_off_mean > recent_throttle_mean:
                                                    continue
                                                if recent_throttle_mean >= recent_boost_mean:
                                                    continue
                                                out.append(
                                                    {
                                                        "label": (
                                                            f"bte_w{window}_"
                                                            f"{'same' if same_cluster else 'all'}_"
                                                            f"min{min_prior}_"
                                                            f"coff{cluster_off_mean:g}_cs{cluster_off_scale:g}_"
                                                            f"roff{recent_off_mean:g}_rt{recent_throttle_mean:g}_"
                                                            f"rb{recent_boost_mean:g}_ts{throttle_scale:g}_"
                                                            f"bs{boost_scale:g}_cold{int(cold_start)}"
                                                        ),
                                                        "window": window,
                                                        "same_cluster": same_cluster,
                                                        "min_prior_markets": min_prior,
                                                        "cold_start_risk_on": cold_start,
                                                        "cluster_off_mean": cluster_off_mean,
                                                        "cluster_off_scale": cluster_off_scale,
                                                        "cluster_throttle_mean": 0.0,
                                                        "cluster_boost_mean": recent_boost_mean,
                                                        "recent_off_mean": recent_off_mean,
                                                        "recent_throttle_mean": recent_throttle_mean,
                                                        "recent_boost_mean": recent_boost_mean,
                                                        "throttle_scale": throttle_scale,
                                                        "boost_scale": boost_scale,
                                                    }
                                                )
    return out


def write_report(
    path: Path,
    rows: list[dict[str, Any]],
    folds: list[tuple[int, int, int]],
    results: list[dict[str, Any]],
    objective_name: str,
    candidate: str,
    policies_evaluated: int,
    equivalent_policies_dropped: int,
) -> None:
    lines = [
        "# BTE Cluster Policy Search",
        "",
        f"Dataset rows: `{len(rows)}`",
        f"Date range: `{rows[0]['date']}` to `{rows[-1]['date']}`",
        f"Candidate: `{candidate}`",
        f"Folds: `{len(folds)}`",
        f"Policies evaluated: `{policies_evaluated}`",
        f"Behavior-equivalent policies dropped: `{equivalent_policies_dropped}`",
        f"Objective: `{objective_name}`",
        "",
        "This is a synthetic sizing overlay on already-realized BTE market PnL.",
        "It is useful for discovering throttle/risk-off rules, but promising",
        "policies still need a fresh engine rerun with the rule implemented.",
        "",
        "The search is intentionally limited to a small, interpretable policy",
        "family. It is not a replacement for a proper optimizer over engine-level",
        "strategy configs; it is a cheap filter for deciding which rules deserve",
        "real backtests.",
        "",
        "## Top Policies",
        "",
        "| Rank | Policy | Objective | PnL | Max DD | CVaR 5% | Mean | Win Rate | Worst | Scale Counts |",
        "|---:|---|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for idx, result in enumerate(results[:25], start=1):
        stats = result["stats"]
        scales = ", ".join(f"{key}x={value}" for key, value in result["scale_counts"].items())
        lines.append(
            f"| {idx} | {result['params']['label']} | {result['objective']:.2f} | "
            f"{fmt_usd(stats['pnl'])} | {pct(stats['max_dd_pct'])} | {fmt_usd(stats['cvar05'])} | "
            f"{fmt_usd(stats['mean'])} | {stats['win_rate']:.1%} | {fmt_usd(stats['worst'])} | {scales} |"
        )

    checkpoints: list[tuple[str, dict[str, Any]]] = []
    fixed = next((result for result in results if result["params"]["label"] == "fixed_bte"), None)
    if fixed is not None:
        checkpoints.append(("Fixed BTE", fixed))
    throttle_only = next(
        (result for result in results if result["params"].get("boost_scale", 1.0) <= 1.0),
        None,
    )
    if throttle_only is not None:
        checkpoints.append(("Best No-Boost Policy", throttle_only))
    risk_on = next(
        (
            result
            for result in results
            if set(result["scale_counts"]) <= {"0.5", "0.75", "1"} and "0" not in result["scale_counts"]
        ),
        None,
    )
    if risk_on is not None:
        checkpoints.append(("Best Throttle-Only Risk-On Policy", risk_on))

    if checkpoints:
        lines.extend(["", "## Policy Checks", ""])
        lines.append("| Check | Policy | Objective | PnL | Max DD | CVaR 5% | Scale Counts |")
        lines.append("|---|---|---:|---:|---:|---:|---|")
        for label, result in checkpoints:
            stats = result["stats"]
            scales = ", ".join(f"{key}x={value}" for key, value in result["scale_counts"].items())
            lines.append(
                f"| {label} | {result['params']['label']} | {result['objective']:.2f} | "
                f"{fmt_usd(stats['pnl'])} | {pct(stats['max_dd_pct'])} | "
                f"{fmt_usd(stats['cvar05'])} | {scales} |"
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

    lines.extend(["", "## Best Policy Cluster PnL", ""])
    lines.append("| Cluster | PnL |")
    lines.append("|---|---:|")
    for cluster, value in sorted(best["cluster_pnl"].items(), key=lambda item: item[1]):
        lines.append(f"| {cluster} | {fmt_usd(value)} |")

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n")


def select_result(results: list[dict[str, Any]], selector: str) -> dict[str, Any]:
    if selector == "best":
        return results[0]
    if selector == "no_boost":
        for result in results:
            if result["params"].get("boost_scale", 1.0) <= 1.0:
                return result
    if selector == "risk_on":
        for result in results:
            if set(result["scale_counts"]) <= {"0.5", "0.75", "1"} and "0" not in result["scale_counts"]:
                return result
    if selector == "fixed":
        for result in results:
            if result["params"]["label"] == "fixed_bte":
                return result
    raise ValueError(f"no result matched selector {selector!r}")


def write_policy_artifact(
    path: Path,
    result: dict[str, Any],
    fixed_result: dict[str, Any] | None,
    rows: list[dict[str, Any]],
    folds: list[tuple[int, int, int]],
    args: argparse.Namespace,
    objective_name: str,
    policies_evaluated: int,
    equivalent_policies_dropped: int,
) -> None:
    stats = result["stats"]
    policy = {
        "schema_version": 1,
        "strategy": "back_to_explore",
        "policy_family": "cluster_recent_performance_scale",
        "status": "candidate_requires_engine_rerun",
        "selection": args.policy_selector,
        "source": {
            "dataset_jsonl": args.dataset_jsonl,
            "date_start": rows[0]["date"],
            "date_end": rows[-1]["date"],
            "rows": len(rows),
            "candidate": args.candidate,
            "train_size": args.train_size,
            "test_size": args.test_size,
            "step_size": args.step_size,
            "folds": len(folds),
            "starting_cash_usdc": args.starting_cash,
            "objective": objective_name,
            "policies_evaluated": policies_evaluated,
            "unique_behaviours": policies_evaluated - equivalent_policies_dropped,
            "equivalent_policies_dropped": equivalent_policies_dropped,
        },
        "rule": {
            "description": (
                "Scale BTE market risk by train-window cluster mean and no-lookahead "
                "recent BTE mean PnL. A zero scale means risk-off for that market."
            ),
            "params": result["params"],
            "scale_counts": result["scale_counts"],
            "scale_signature": result["scale_signature"],
        },
        "validation": {
            "selected": compact_stats(stats),
            "fixed_bte": compact_stats(fixed_result["stats"]) if fixed_result is not None else None,
            "delta_vs_fixed": {
                "pnl_usdc": stats["pnl"] - fixed_result["stats"]["pnl"] if fixed_result is not None else None,
                "max_dd_pct": stats["max_dd_pct"] - fixed_result["stats"]["max_dd_pct"]
                if fixed_result is not None
                else None,
                "cvar05_market_pnl_usdc": stats["cvar05"] - fixed_result["stats"]["cvar05"]
                if fixed_result is not None
                else None,
            },
            "folds": result["folds"],
            "cluster_pnl": result["cluster_pnl"],
        },
        "implementation_notes": [
            "This policy was selected from synthetic scaled market PnL, not from a fresh engine replay.",
            "Deployable implementation needs pre-route regime_cluster plus rolling prior BTE performance in runtime context.",
            "Use this candidate first as a risk-off/throttle overlay; do not trust boosted sizing until engine replay confirms it.",
        ],
    }
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(policy, indent=2, sort_keys=True) + "\n")


def write_policy_scale_rows(
    path: Path,
    fold_data: list[FoldData],
    result: dict[str, Any],
    candidate: str,
) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as file:
        for fold in fold_data:
            scales = scales_for_fold(fold, result["params"])
            for row, scale in zip(fold.test_rows, scales, strict=True):
                out = {
                    "slug": row["slug"],
                    "close_ts": int(row["close_ts"]),
                    "date": row["date"],
                    "diagnostic_cluster": row["diagnostic_cluster"],
                    "strategy": "back_to_explore",
                    "policy_label": result["params"]["label"],
                    "scale": float(scale),
                    "candidate_pnl_usdc": bte_pnl(row, candidate),
                }
                file.write(json.dumps(out, sort_keys=True) + "\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("dataset_jsonl")
    parser.add_argument("--candidate", default="bte")
    parser.add_argument("--train-size", type=int, default=2600)
    parser.add_argument("--test-size", type=int, default=650)
    parser.add_argument("--step-size", type=int, default=650)
    parser.add_argument("--starting-cash", type=float, default=2700.0)
    parser.add_argument("--dd-penalty", type=float, default=30.0)
    parser.add_argument("--cvar-penalty", type=float, default=10.0)
    parser.add_argument("--window", action="append", type=int, dest="windows")
    parser.add_argument("--out-md", required=True, type=Path)
    parser.add_argument("--out-json", required=True, type=Path)
    parser.add_argument("--out-policy-json", type=Path)
    parser.add_argument(
        "--out-scale-jsonl",
        type=Path,
        help="Write per-held-out-market policy scale rows for engine replay.",
    )
    parser.add_argument(
        "--policy-selector",
        choices=("no_boost", "best", "risk_on", "fixed"),
        default="no_boost",
        help="Which deduplicated result to export as a compact policy artifact.",
    )
    args = parser.parse_args()

    rows = load_rows(Path(args.dataset_jsonl))
    windows = sorted(set(args.windows or [288, 864, 2016]))
    folds = fold_slices(rows, args.train_size, args.test_size, args.step_size)
    fold_data = prepare_folds(rows, folds, args.candidate, windows)
    results: list[dict[str, Any]] = []
    for params in params_grid(windows):
        result = evaluate(fold_data, params, args.starting_cash)
        result["objective"] = objective(result["stats"], args.dd_penalty, args.cvar_penalty)
        results.append(result)
    results.sort(key=lambda result: result["objective"], reverse=True)
    policies_evaluated = len(results)

    unique_results: list[dict[str, Any]] = []
    seen_signatures: set[str] = set()
    for result in results:
        signature = result["scale_signature"]
        if signature in seen_signatures:
            continue
        seen_signatures.add(signature)
        unique_results.append(result)
    results = unique_results
    equivalent_policies_dropped = policies_evaluated - len(results)

    objective_name = f"pnl - {args.dd_penalty:g}*max_dd_pct + {args.cvar_penalty:g}*cvar05"
    write_report(
        args.out_md,
        rows,
        folds,
        results,
        objective_name,
        args.candidate,
        policies_evaluated,
        equivalent_policies_dropped,
    )
    args.out_json.parent.mkdir(parents=True, exist_ok=True)
    args.out_json.write_text(json.dumps(results[:100], indent=2, sort_keys=True) + "\n")
    if args.out_policy_json is not None:
        fixed_result = next(
            (result for result in results if result["params"]["label"] == "fixed_bte"),
            None,
        )
        write_policy_artifact(
            args.out_policy_json,
            select_result(results, args.policy_selector),
            fixed_result,
            rows,
            folds,
            args,
            objective_name,
            policies_evaluated,
            equivalent_policies_dropped,
        )
    if args.out_scale_jsonl is not None:
        write_policy_scale_rows(
            args.out_scale_jsonl,
            fold_data,
            select_result(results, args.policy_selector),
            args.candidate,
        )
    print(
        f"searched {policies_evaluated} policies across {len(folds)} folds "
        f"({len(results)} unique behaviours)"
    )
    print(f"best: {results[0]['params']['label']} objective={results[0]['objective']:.2f}")
    if args.out_policy_json is not None:
        print(f"policy: {args.policy_selector} -> {args.out_policy_json}")
    if args.out_scale_jsonl is not None:
        print(f"scales: {args.policy_selector} -> {args.out_scale_jsonl}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
