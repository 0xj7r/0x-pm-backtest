#!/usr/bin/env python3
"""Evaluate regime-based routing on overlapping market artifacts.

Each candidate is provided as `name=strategy:path/to/markets.jsonl`.  The script
joins candidates by market slug, derives live-safe cluster labels from available
fill-time features, learns the best candidate per cluster on the chronological
train slice, then scores that routing policy on the held-out slice.
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


def parse_candidate(value: str) -> tuple[str, str, Path]:
    try:
        name, rest = value.split("=", 1)
        strategy, raw_path = rest.split(":", 1)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(
            "candidate must be NAME=STRATEGY:path/to/markets.jsonl"
        ) from exc
    return name, strategy, Path(raw_path)


def close_ts(row: dict[str, Any]) -> int:
    value = row.get("close_ts")
    if value is not None:
        return int(value)
    slug = str(row.get("slug") or "")
    return int(slug.rsplit("-", 1)[1]) + 300


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
                "pnl": float(strat.get("pnl_usdc") or 0.0),
                "fills": int(strat.get("fills") or 0),
                "filled_notional": float(strat.get("filled_notional_usdc") or 0.0),
                "features": features,
            }
    return rows


def mean_feature(values: list[float]) -> float | None:
    return sum(values) / len(values) if values else None


def feature_source_rows(candidates: dict[str, dict[str, Any]], source: str) -> list[dict[str, Any]]:
    if source == "union":
        return list(candidates.values())
    row = candidates.get(source)
    return [row] if row is not None else []


def cluster_for(candidates: dict[str, dict[str, Any]], source: str) -> str:
    rows = feature_source_rows(candidates, source)
    rows = [row for row in rows if row["fills"] > 0]
    if not rows:
        return "no_trade"
    features: dict[str, float | None] = {}
    for key in FEATURES:
        vals = [row["features"].get(key) for row in rows]
        features[key] = mean_feature([float(v) for v in vals if v is not None and math.isfinite(v)])
    return cluster_label(features)


def equity_stats(pnls: list[float], starting_cash: float) -> dict[str, float]:
    equity = starting_cash
    peak = starting_cash
    max_dd = 0.0
    for pnl in pnls:
        equity += pnl
        peak = max(peak, equity)
        if peak > 0:
            max_dd = max(max_dd, (peak - equity) / peak)
    return {"pnl": equity - starting_cash, "end": equity, "max_dd_pct": max_dd * 100.0}


def fmt_usd(value: float) -> str:
    return f"${value:,.2f}"


def add(acc: dict[str, float], pnl: float, fills: int) -> None:
    acc["markets"] += 1
    acc["fills"] += fills
    acc["pnl"] += pnl
    acc["wins"] += 1 if pnl > 0.0 else 0
    acc["losses"] += 1 if pnl < 0.0 else 0
    acc["worst"] = min(acc.get("worst", 0.0), pnl)
    acc["best"] = max(acc.get("best", 0.0), pnl)


def write_policy_table(lines: list[str], title: str, policy_rows: list[dict[str, Any]]) -> None:
    lines.extend([f"## {title}", ""])
    lines.append("| Cluster | Train Markets | Train PnL By Candidate | Route | Test Markets | Route Test PnL |")
    lines.append("|---|---:|---|---|---:|---:|")
    for row in policy_rows:
        pnl_by_candidate = ", ".join(
            f"{name}={fmt_usd(pnl)}" for name, pnl in sorted(row["train_pnl_by_candidate"].items())
        )
        lines.append(
            f"| {row['cluster']} | {row['train_markets']} | {pnl_by_candidate} | "
            f"{row['route']} | {row['test_markets']} | {fmt_usd(row['test_pnl'])} |"
        )
    lines.append("")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", action="append", required=True, type=parse_candidate)
    parser.add_argument("--feature-source", default="union", help="candidate name or 'union'")
    parser.add_argument("--train-frac", type=float, default=0.60)
    parser.add_argument("--min-train-markets", type=int, default=20)
    parser.add_argument("--starting-cash", type=float, default=2700.0)
    parser.add_argument("--out-md", required=True)
    parser.add_argument("--out-json")
    args = parser.parse_args()

    loaded = {
        name: load_candidate(name, strategy, path)
        for name, strategy, path in args.candidate
    }
    if len(loaded) < 2:
        raise RuntimeError("need at least two candidates")
    candidate_names = sorted(loaded)
    shared_slugs = set.intersection(*(set(rows) for rows in loaded.values()))
    joined: list[dict[str, Any]] = []
    for slug in shared_slugs:
        candidates = {name: loaded[name][slug] for name in candidate_names}
        ts = max(row["close_ts"] for row in candidates.values())
        joined.append(
            {
                "slug": slug,
                "close_ts": ts,
                "date": dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat(),
                "cluster": cluster_for(candidates, args.feature_source),
                "candidates": candidates,
            }
        )
    joined.sort(key=lambda row: row["close_ts"])
    if len(joined) < 100:
        raise RuntimeError(f"not enough overlapping markets: {len(joined)}")

    split_idx = max(1, min(len(joined) - 1, int(len(joined) * args.train_frac)))
    train = joined[:split_idx]
    test = joined[split_idx:]

    train_by_cluster: dict[str, dict[str, dict[str, float]]] = defaultdict(
        lambda: defaultdict(lambda: defaultdict(float))
    )
    for row in train:
        cluster = row["cluster"]
        for name, candidate in row["candidates"].items():
            add(train_by_cluster[cluster][name], candidate["pnl"], candidate["fills"])

    global_train_pnl = {
        name: sum(row["candidates"][name]["pnl"] for row in train)
        for name in candidate_names
    }
    default_route = max(global_train_pnl, key=global_train_pnl.get)
    policy: dict[str, str] = {}
    policy_rows: list[dict[str, Any]] = []
    for cluster, per_candidate in sorted(train_by_cluster.items()):
        train_markets = int(next(iter(per_candidate.values()))["markets"])
        if train_markets < args.min_train_markets:
            route = default_route
        else:
            route = max(candidate_names, key=lambda name: per_candidate[name]["pnl"])
        policy[cluster] = route
        test_rows = [row for row in test if row["cluster"] == cluster]
        test_pnl = sum(row["candidates"][route]["pnl"] for row in test_rows)
        policy_rows.append(
            {
                "cluster": cluster,
                "train_markets": train_markets,
                "train_pnl_by_candidate": {
                    name: per_candidate[name]["pnl"] for name in candidate_names
                },
                "route": route,
                "test_markets": len(test_rows),
                "test_pnl": test_pnl,
            }
        )

    test_baselines = {
        name: [row["candidates"][name]["pnl"] for row in test]
        for name in candidate_names
    }
    route_pnls = [row["candidates"][policy.get(row["cluster"], default_route)]["pnl"] for row in test]
    oracle_pnls = [max(row["candidates"][name]["pnl"] for name in candidate_names) for row in test]
    risk_off_pnls = [0.0 for _ in test]

    by_cluster_test: dict[tuple[str, str], dict[str, float]] = defaultdict(lambda: defaultdict(float))
    for row in test:
        cluster = row["cluster"]
        route = policy.get(cluster, default_route)
        add(by_cluster_test[(cluster, "router")], row["candidates"][route]["pnl"], row["candidates"][route]["fills"])
        for name in candidate_names:
            candidate = row["candidates"][name]
            add(by_cluster_test[(cluster, name)], candidate["pnl"], candidate["fills"])

    lines: list[str] = [
        "# Router Overlap Evaluation",
        "",
        f"Feature source: `{args.feature_source}`",
        f"Shared markets: `{len(joined)}`",
        f"Train markets: `{len(train)}`",
        f"Test markets: `{len(test)}`",
        f"Train range: `{train[0]['date']}` to `{train[-1]['date']}`",
        f"Test range: `{test[0]['date']}` to `{test[-1]['date']}`",
        "",
        "This is an offline diagnostic. Cluster labels are based on fill-time",
        "features available in the completed artifacts; production routing still",
        "needs equivalent market-level features before order submission.",
        "",
        "## Test Summary",
        "",
        "| Policy | Test PnL | End Equity | Max DD |",
        "|---|---:|---:|---:|",
    ]
    summaries: dict[str, dict[str, float]] = {}
    for name, pnls in test_baselines.items():
        summaries[name] = equity_stats(pnls, args.starting_cash)
    summaries["cluster_router"] = equity_stats(route_pnls, args.starting_cash)
    summaries["oracle_best_per_market"] = equity_stats(oracle_pnls, args.starting_cash)
    summaries["risk_off"] = equity_stats(risk_off_pnls, args.starting_cash)
    for name, stats in summaries.items():
        lines.append(
            f"| {name} | {fmt_usd(stats['pnl'])} | {fmt_usd(stats['end'])} | {stats['max_dd_pct']:.2f}% |"
        )
    lines.append("")

    write_policy_table(lines, "Learned Cluster Policy", policy_rows)

    lines.extend(["## Test PnL By Cluster", ""])
    lines.append("| Cluster | Candidate | Markets | Fills | PnL | Mean / Market | Win Rate | Worst | Best |")
    lines.append("|---|---|---:|---:|---:|---:|---:|---:|---:|")
    for (cluster, name), acc in sorted(by_cluster_test.items()):
        markets = int(acc["markets"])
        mean = acc["pnl"] / markets if markets else 0.0
        win_rate = acc["wins"] / markets if markets else 0.0
        lines.append(
            f"| {cluster} | {name} | {markets} | {int(acc['fills'])} | "
            f"{fmt_usd(acc['pnl'])} | {fmt_usd(mean)} | {win_rate:.1%} | "
            f"{fmt_usd(acc['worst'])} | {fmt_usd(acc['best'])} |"
        )
    lines.append("")

    out_md = Path(args.out_md)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_md.write_text("\n".join(lines) + "\n")

    if args.out_json:
        out = {
            "candidate_names": candidate_names,
            "feature_source": args.feature_source,
            "shared_markets": len(joined),
            "train_markets": len(train),
            "test_markets": len(test),
            "policy": policy,
            "summaries": summaries,
        }
        Path(args.out_json).write_text(json.dumps(out, indent=2, sort_keys=True) + "\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
