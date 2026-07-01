#!/usr/bin/env python3
"""Walk-forward search for replay-safe late-break gate candidates."""

from __future__ import annotations

import argparse
import datetime as dt
from collections import defaultdict
from pathlib import Path
from typing import Any, Callable

import postfill_reversal_model as prm


LATE_LANES = {"br2_late_confirm", "br2_late_favourite_load"}

FEATURES = [
    "price",
    "side_model_p",
    "side_edge_vs_fill",
    "risk_score",
    "market_yes_range_so_far",
    "regime_reversal_pressure",
    "regime_sign_flip_rate",
    "regime_path_efficiency",
    "regime_whipsaw_score",
    "prior_market_range_1d",
    "prior_market_range_3d",
    "prior_market_range_7d",
]


def money(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{value:.2%}"


def f(fill: dict[str, Any], key: str) -> float:
    return float(fill.get(key) or 0.0)


def quantiles(train: list[dict[str, Any]], feature: str) -> tuple[float, float, float]:
    values = sorted(f(fill, feature) for fill in train)
    if not values:
        return (0.0, 0.0, 0.0)
    last = len(values) - 1
    return (values[int(last * 0.25)], values[int(last * 0.50)], values[int(last * 0.75)])


def quartile_predicate(feature: str, bucket: str, qs: tuple[float, float, float]) -> Callable[[dict[str, Any]], bool]:
    q25, q50, q75 = qs
    if bucket == "q1":
        return lambda fill: f(fill, feature) <= q25
    if bucket == "q2":
        return lambda fill: q25 < f(fill, feature) <= q50
    if bucket == "q3":
        return lambda fill: q50 < f(fill, feature) <= q75
    if bucket == "q4":
        return lambda fill: f(fill, feature) > q75
    raise ValueError(bucket)


def acc() -> dict[str, float]:
    return defaultdict(float)


def add_candidate(row: dict[str, float], test: list[dict[str, Any]], predicate: Callable[[dict[str, Any]], bool]) -> None:
    removed = [fill for fill in test if predicate(fill)]
    kept = [fill for fill in test if not predicate(fill)]
    removed_pnl = sum(float(fill["pnl"]) for fill in removed)
    kept_pnl = sum(float(fill["pnl"]) for fill in kept)
    row["folds"] += 1
    row["tested_fills"] += len(test)
    row["base_pnl"] += removed_pnl + kept_pnl
    row["removed_fills"] += len(removed)
    row["removed_pnl"] += removed_pnl
    row["removed_cost"] += sum(float(fill["notional"]) for fill in removed)
    row["kept_pnl"] += kept_pnl
    row["removed_toxic"] += sum(1 for fill in removed if fill["target"])
    row["removed_cross"] += sum(1 for fill in removed if fill["post_crossed_mid"])
    if removed:
        previous_active = int(row["active_folds"])
        row["active_folds"] += 1
        row["helpful_folds"] += 1 if removed_pnl < 0.0 else 0
        row["harmful_folds"] += 1 if removed_pnl > 0.0 else 0
        if previous_active == 0:
            row["worst_fold_removed_pnl"] = removed_pnl
            row["best_fold_removed_pnl"] = removed_pnl
        else:
            row["worst_fold_removed_pnl"] = max(row["worst_fold_removed_pnl"], removed_pnl)
            row["best_fold_removed_pnl"] = min(row["best_fold_removed_pnl"], removed_pnl)


def viable_train_gate(train: list[dict[str, Any]], predicate: Callable[[dict[str, Any]], bool], min_removed: int) -> bool:
    removed = [fill for fill in train if predicate(fill)]
    if len(removed) < min_removed:
        return False
    return sum(float(fill["pnl"]) for fill in removed) < 0.0


def make_candidates(train: list[dict[str, Any]], min_train_removed: int) -> list[tuple[str, Callable[[dict[str, Any]], bool]]]:
    candidates: list[tuple[str, Callable[[dict[str, Any]], bool]]] = []
    by_feature_qs = {feature: quantiles(train, feature) for feature in FEATURES}

    base_predicates: list[tuple[str, Callable[[dict[str, Any]], bool]]] = []
    for feature in FEATURES:
        for bucket in ("q1", "q2", "q3", "q4"):
            pred = quartile_predicate(feature, bucket, by_feature_qs[feature])
            base_predicates.append((f"{feature}:{bucket}", pred))

    for name, pred in base_predicates:
        if viable_train_gate(train, pred, min_train_removed):
            candidates.append((name, pred))

    # Keep pair search focused on the features that looked most plausible in the
    # contrast diagnostics, otherwise the combinatorics get noisy very quickly.
    pair_features = [
        "price",
        "side_model_p",
        "risk_score",
        "prior_market_range_3d",
        "prior_market_range_7d",
        "side_edge_vs_fill",
        "regime_reversal_pressure",
    ]
    pair_preds = [
        item for item in base_predicates if item[0].split(":", 1)[0] in pair_features
    ]
    for i, (left_name, left_pred) in enumerate(pair_preds):
        for right_name, right_pred in pair_preds[i + 1 :]:
            if left_name.split(":", 1)[0] == right_name.split(":", 1)[0]:
                continue
            name = f"{left_name}&{right_name}"
            pred = lambda fill, lp=left_pred, rp=right_pred: lp(fill) and rp(fill)
            if viable_train_gate(train, pred, min_train_removed):
                candidates.append((name, pred))

    lane_specs = [
        ("late_confirm", lambda fill: fill["tag"] == "br2_late_confirm"),
        ("late_fav", lambda fill: fill["tag"] == "br2_late_favourite_load"),
    ]
    lane_candidates: list[tuple[str, Callable[[dict[str, Any]], bool]]] = []
    for lane_name, lane_pred in lane_specs:
        for base_name, base_pred in base_predicates:
            pred = lambda fill, lp=lane_pred, bp=base_pred: lp(fill) and bp(fill)
            name = f"{lane_name}:{base_name}"
            if viable_train_gate(train, pred, min_train_removed):
                lane_candidates.append((name, pred))
    candidates.extend(lane_candidates)
    return candidates


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--min-train-fills", type=int, default=900)
    parser.add_argument("--test-fills", type=int, default=300)
    parser.add_argument("--step-fills", type=int, default=300)
    parser.add_argument("--min-train-removed", type=int, default=20)
    parser.add_argument("--source-label")
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    fills = prm.load_fills(args.markets_jsonl, args.strategy, "toxic_reversal_path")
    fills = [fill for fill in fills if fill["tag"] in LATE_LANES]
    fills.sort(key=lambda fill: (int(fill["ts"]), str(fill["tag"])))
    if len(fills) < args.min_train_fills + args.test_fills:
        raise RuntimeError(
            f"not enough late fills for search: fills={len(fills)} "
            f"required={args.min_train_fills + args.test_fills}"
        )

    rows: dict[str, dict[str, float]] = defaultdict(acc)
    fold_rows: list[str] = []
    fold = 0
    start = args.min_train_fills
    while start + args.test_fills <= len(fills):
        train = fills[:start]
        test = fills[start : start + args.test_fills]
        fold += 1
        fold_pnl = sum(float(fill["pnl"]) for fill in test)
        fold_toxic = sum(1 for fill in test if fill["target"])
        test_start = dt.datetime.fromtimestamp(int(test[0]["ts"]), tz=dt.timezone.utc)
        test_end = dt.datetime.fromtimestamp(int(test[-1]["ts"]), tz=dt.timezone.utc)
        fold_rows.append(
            f"| {fold} | {len(train)} | {len(test)} | {test_start.date()} | "
            f"{test_end.date()} | {money(fold_pnl)} | {pct(fold_toxic / len(test))} |"
        )
        for name, predicate in make_candidates(train, args.min_train_removed):
            add_candidate(rows[name], test, predicate)
        start += args.step_fills

    ranked = sorted(
        rows.items(),
        key=lambda item: (
            item[1]["harmful_folds"],
            item[1]["removed_pnl"],
            -item[1]["active_folds"],
        ),
    )
    source = args.source_label or args.markets_jsonl
    total_pnl = sum(float(fill["pnl"]) for fill in fills)
    total_toxic = sum(1 for fill in fills if fill["target"])
    lines = [
        "# BTC5m Late-Break Walk-Forward Gate Search",
        "",
        f"Source: `{source}`",
        f"Late fills: `{len(fills)}`",
        f"Late-fill PnL: `{money(total_pnl)}`",
        f"Toxic late fills: `{total_toxic}` (`{pct(total_toxic / len(fills))}`)",
        f"Min train fills: `{args.min_train_fills}`",
        f"Test fills per fold: `{args.test_fills}`",
        f"Step fills: `{args.step_fills}`",
        "",
        "Candidate thresholds are computed from each fold's training fills only. A candidate is admitted in a fold only when the same train-side rule removed at least the configured minimum fills and had negative train PnL.",
        "",
        "## Candidate Outcomes",
        "",
        "| Candidate | Folds | Active Folds | Helpful Folds | Harmful Folds | Tested Fills | Removed Fills | Removed Cost | Removed PnL | Worst Fold Removed PnL | Kept PnL | Full-Removal Improvement | Half-Throttle Improvement | Removed Toxic Rate | Removed Cross-Mid Rate |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for name, row in ranked[:60]:
        removed = int(row["removed_fills"])
        if removed == 0:
            continue
        lines.append(
            f"| {name} | {int(row['folds'])} | {int(row['active_folds'])} | "
            f"{int(row['helpful_folds'])} | {int(row['harmful_folds'])} | "
            f"{int(row['tested_fills'])} | {removed} | "
            f"{money(row['removed_cost'])} | {money(row['removed_pnl'])} | "
            f"{money(row['worst_fold_removed_pnl'])} | {money(row['kept_pnl'])} | "
            f"{money(-row['removed_pnl'])} | "
            f"{money(-0.5 * row['removed_pnl'])} | "
            f"{pct(row['removed_toxic'] / removed)} | {pct(row['removed_cross'] / removed)} |"
        )
    lines.extend(
        [
            "",
            "## Folds",
            "",
            "| Fold | Train Fills | Test Fills | Test Start | Test End | Test PnL | Toxic Rate |",
            "|---:|---:|---:|---|---|---:|---:|",
            *fold_rows,
            "",
        ]
    )
    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
