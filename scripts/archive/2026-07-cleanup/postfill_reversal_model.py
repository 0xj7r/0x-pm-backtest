#!/usr/bin/env python3
"""Train a replay-safe classifier for toxic post-fill reversal paths."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import subprocess
import sys
from collections import defaultdict, deque
from pathlib import Path
from typing import Any, Iterable, Iterator, TextIO

import numpy as np


AWS_PROFILE: str | None = None

LANES = [
    "br2_high_skew_load",
    "br2_late_confirm",
    "br2_late_favourite_load",
]

NUMERIC_FEATURES = [
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


def open_input(path: str) -> tuple[TextIO, subprocess.Popen[str] | None]:
    if path == "-":
        return sys.stdin, None
    if path.startswith("s3://"):
        env = os.environ.copy()
        if AWS_PROFILE:
            env["AWS_PROFILE"] = AWS_PROFILE
        proc = subprocess.Popen(
            ["aws", "s3", "cp", path, "-"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        assert proc.stdout is not None
        return proc.stdout, proc
    return Path(path).open(), None


def iter_rows(lines: Iterable[str]) -> Iterator[dict[str, Any]]:
    for line in lines:
        line = line.strip()
        if line:
            yield json.loads(line)


def close_ts(row: dict[str, Any]) -> int:
    if row.get("close_ts") is not None:
        return int(row["close_ts"])
    return int(str(row.get("slug") or "").rsplit("-", 1)[1]) + 300


def mean_tail(values: deque[float], n: int) -> float:
    if not values:
        return 0.0
    tail = list(values)[-n:]
    return sum(tail) / len(tail) if tail else 0.0


def strategy_result(row: dict[str, Any], strategy: str) -> dict[str, Any]:
    return ((row.get("per_strategy") or {}).get(strategy)) or {}


def yes_resolved(row: dict[str, Any], strat: dict[str, Any]) -> bool:
    if "yes_resolved" in strat:
        return bool(strat["yes_resolved"])
    return str(row.get("outcome_label") or "").lower() in ("yes", "up")


def fill_won(fill: dict[str, Any], resolved_yes: bool) -> bool | None:
    side = str(fill.get("side") or "")
    if side == "BuyYes":
        return resolved_yes
    if side == "BuyNo":
        return not resolved_yes
    return None


def fill_pnl(fill: dict[str, Any], resolved_yes: bool) -> tuple[float, bool | None]:
    won = fill_won(fill, resolved_yes)
    shares = float(fill.get("shares") or 0.0)
    notional = float(fill.get("notional") or 0.0)
    rebate = float(fill.get("rebate_usdc") or 0.0)
    return (shares if won else 0.0) - notional + rebate, won


def path_label(path: dict[str, Any], pnl: float, target: str) -> bool:
    crossed = bool(path.get("crossed_mid_after_fill"))
    adverse = float(path.get("adverse_excursion") or 0.0)
    final_side_mid = float(path.get("final_side_mid") or 0.0)
    if target == "crossed_mid_after_fill":
        return crossed
    if target == "adverse_soft_finish":
        return adverse >= 0.20 and final_side_mid < 0.60
    if target == "toxic_crossed_mid":
        return crossed and pnl < 0.0
    if target == "toxic_reversal_path":
        return pnl < 0.0 and (crossed or (adverse >= 0.20 and final_side_mid < 0.60))
    raise ValueError(target)


def load_fills(path: str, strategy: str, target: str) -> list[dict[str, Any]]:
    f, proc = open_input(path)
    fills: list[dict[str, Any]] = []
    prior_ranges: deque[float] = deque(maxlen=7 * 288)
    missing_paths = 0
    try:
        for row in iter_rows(f):
            strat = strategy_result(row, strategy)
            ts = close_ts(row)
            resolved_yes = yes_resolved(row, strat)
            final_range = float(row.get("volatility_range") or 0.0)
            prior_1d = mean_tail(prior_ranges, 288)
            prior_3d = mean_tail(prior_ranges, 3 * 288)
            prior_7d = mean_tail(prior_ranges, 7 * 288)
            for fill in strat.get("fills_detail") or []:
                tag = str(fill.get("tag") or "unknown")
                if tag not in LANES:
                    continue
                path_obj = fill.get("post_fill_path")
                if not isinstance(path_obj, dict):
                    missing_paths += 1
                    continue
                pnl, won = fill_pnl(fill, resolved_yes)
                item = {
                    "ts": ts,
                    "date": dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date(),
                    "tag": tag,
                    "side": str(fill.get("side") or "unknown"),
                    "pnl": pnl,
                    "won": bool(won),
                    "notional": float(fill.get("notional") or 0.0),
                    "price": float(fill.get("price") or 0.0),
                    "final_range": final_range,
                    "target": path_label(path_obj, pnl, target),
                    "post_crossed_mid": bool(path_obj.get("crossed_mid_after_fill")),
                    "post_adverse_excursion": float(path_obj.get("adverse_excursion") or 0.0),
                    "post_favourable_excursion": float(path_obj.get("favourable_excursion") or 0.0),
                    "post_final_side_mid": float(path_obj.get("final_side_mid") or 0.0),
                    "prior_market_range_1d": prior_1d,
                    "prior_market_range_3d": prior_3d,
                    "prior_market_range_7d": prior_7d,
                }
                for key in NUMERIC_FEATURES:
                    if key in item:
                        continue
                    value = fill.get(key)
                    item[key] = float(value) if value is not None else 0.0
                fills.append(item)
            prior_ranges.append(final_range)
    finally:
        if proc is not None:
            assert proc.stderr is not None
            stderr = proc.stderr.read()
            rc = proc.wait()
            if rc != 0:
                raise RuntimeError(stderr.strip())
        elif f is not sys.stdin:
            f.close()
    if missing_paths and not fills:
        raise RuntimeError(
            f"no fills with post_fill_path found; skipped {missing_paths} lane fills without path labels"
        )
    return fills


def matrix(fills: list[dict[str, Any]]) -> tuple[np.ndarray, np.ndarray, list[str]]:
    names = list(NUMERIC_FEATURES)
    names.extend(["buy_yes"])
    names.extend([f"tag:{tag}" for tag in LANES])
    names.extend(
        [
            "whipsaw_x_reversal",
            "whipsaw_x_low_efficiency",
            "range_x_reversal",
            "range_x_sign_flip",
            "vol_x_reversal",
            "price_x_model_p",
            "edge_x_confidence",
            "risk_x_range",
            "prior1d_x_range",
            "prior7d_minus_1d",
        ]
    )
    rows = []
    y = []
    for fill in fills:
        row = [float(fill.get(key) or 0.0) for key in NUMERIC_FEATURES]
        row.append(1.0 if fill.get("side") == "BuyYes" else 0.0)
        row.extend(1.0 if fill.get("tag") == tag else 0.0 for tag in LANES)
        whipsaw = float(fill.get("regime_whipsaw_score") or 0.0)
        reversal = float(fill.get("regime_reversal_pressure") or 0.0)
        path_eff = float(fill.get("regime_path_efficiency") or 0.0)
        obs_range = float(fill.get("market_yes_range_so_far") or 0.0)
        sign_flip = float(fill.get("regime_sign_flip_rate") or 0.0)
        vol = float(fill.get("regime_realized_vol_180s_bps") or 0.0)
        price = float(fill.get("price") or 0.0)
        model_p = float(fill.get("side_model_p") or 0.5)
        edge = float(fill.get("side_edge_vs_fill") or 0.0)
        conf = float(fill.get("confidence_score") or 0.0)
        risk = float(fill.get("risk_score") or 0.0)
        prior_1d = float(fill.get("prior_market_range_1d") or 0.0)
        prior_7d = float(fill.get("prior_market_range_7d") or 0.0)
        row.extend(
            [
                whipsaw * reversal,
                whipsaw * (1.0 - path_eff),
                obs_range * reversal,
                obs_range * sign_flip,
                vol * reversal,
                price * model_p,
                edge * conf,
                risk * obs_range,
                prior_1d * obs_range,
                prior_7d - prior_1d,
            ]
        )
        rows.append(row)
        y.append(float(fill["target"]))
    return np.asarray(rows, dtype=np.float64), np.asarray(y, dtype=np.float64), names


def sigmoid(x: np.ndarray) -> np.ndarray:
    return 1.0 / (1.0 + np.exp(-np.clip(x, -35.0, 35.0)))


def train_weighted_logistic(
    x_train: np.ndarray,
    y_train: np.ndarray,
    epochs: int,
    learning_rate: float,
    l2: float,
) -> np.ndarray:
    weights = np.zeros(x_train.shape[1] + 1, dtype=np.float64)
    x_aug = np.c_[np.ones(len(x_train)), x_train]
    pos_rate = y_train.mean()
    pos_w = 0.5 / max(pos_rate, 1e-6)
    neg_w = 0.5 / max(1.0 - pos_rate, 1e-6)
    sample_w = np.where(y_train > 0.5, pos_w, neg_w)
    for _ in range(epochs):
        p = sigmoid(x_aug @ weights)
        grad = (x_aug.T @ ((p - y_train) * sample_w)) / len(y_train)
        grad[1:] += l2 * weights[1:]
        weights -= learning_rate * grad
    return weights


def predict(weights: np.ndarray, x: np.ndarray) -> np.ndarray:
    return sigmoid(np.c_[np.ones(len(x)), x] @ weights)


def log_loss(y: np.ndarray, p: np.ndarray) -> float:
    p = np.clip(p, 1e-5, 1.0 - 1e-5)
    return float(-(y * np.log(p) + (1.0 - y) * np.log(1.0 - p)).mean())


def brier(y: np.ndarray, p: np.ndarray) -> float:
    return float(((p - y) ** 2).mean())


def auc(y: np.ndarray, p: np.ndarray) -> float:
    order = np.argsort(p)
    ranks = np.empty_like(order, dtype=np.float64)
    ranks[order] = np.arange(1, len(p) + 1)
    n_pos = float(y.sum())
    n_neg = float(len(y) - y.sum())
    if n_pos == 0.0 or n_neg == 0.0:
        return math.nan
    return float((ranks[y > 0.5].sum() - n_pos * (n_pos + 1.0) / 2.0) / (n_pos * n_neg))


def money(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{value:.2f}%"


def bucket_rows(fills: list[dict[str, Any]], probs: np.ndarray, buckets: int) -> list[dict[str, Any]]:
    order = np.argsort(probs)
    rows = []
    for i, idxs in enumerate(np.array_split(order, buckets), 1):
        selected = [fills[int(idx)] for idx in idxs]
        ps = [float(probs[int(idx)]) for idx in idxs]
        if not selected:
            continue
        rows.append(
            {
                "bucket": i,
                "fills": len(selected),
                "avg_prob": sum(ps) / len(ps),
                "pnl": sum(fill["pnl"] for fill in selected),
                "cost": sum(fill["notional"] for fill in selected),
                "target_rate": sum(1 for fill in selected if fill["target"]) / len(selected),
                "cross_rate": sum(1 for fill in selected if fill["post_crossed_mid"]) / len(selected),
                "avg_adverse": sum(fill["post_adverse_excursion"] for fill in selected) / len(selected),
                "avg_final_side_mid": sum(fill["post_final_side_mid"] for fill in selected) / len(selected),
            }
        )
    return rows


def removal_rows(
    train_probs: np.ndarray,
    train: list[dict[str, Any]],
    test_probs: np.ndarray,
    test: list[dict[str, Any]],
    quantiles: list[float],
) -> list[dict[str, Any]]:
    rows = []
    for quantile in quantiles:
        threshold = float(np.quantile(train_probs, quantile))
        for lane in ["all", *LANES]:
            train_pairs = [(f, p) for f, p in zip(train, train_probs) if lane == "all" or f["tag"] == lane]
            test_pairs = [(f, p) for f, p in zip(test, test_probs) if lane == "all" or f["tag"] == lane]
            test_removed = [f for f, p in test_pairs if p >= threshold]
            test_kept = [f for f, p in test_pairs if p < threshold]
            if not test_removed:
                continue
            train_removed = [f for f, p in train_pairs if p >= threshold]
            rows.append(
                {
                    "quantile": quantile,
                    "threshold": threshold,
                    "lane": lane,
                    "train_removed": len(train_removed),
                    "train_removed_pnl": sum(f["pnl"] for f in train_removed),
                    "test_removed": len(test_removed),
                    "test_removed_pnl": sum(f["pnl"] for f in test_removed),
                    "test_kept_pnl": sum(f["pnl"] for f in test_kept),
                    "removed_target_rate": sum(1 for f in test_removed if f["target"]) / len(test_removed),
                    "removed_cross_rate": sum(1 for f in test_removed if f["post_crossed_mid"]) / len(test_removed),
                }
            )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile")
    parser.add_argument("--test-days", type=int, default=30)
    parser.add_argument(
        "--target",
        choices=[
            "crossed_mid_after_fill",
            "adverse_soft_finish",
            "toxic_crossed_mid",
            "toxic_reversal_path",
        ],
        default="toxic_reversal_path",
    )
    parser.add_argument("--epochs", type=int, default=3000)
    parser.add_argument("--learning-rate", type=float, default=0.035)
    parser.add_argument("--l2", type=float, default=0.02)
    parser.add_argument("--min-fills", type=int, default=500)
    parser.add_argument("--source-label")
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    fills = load_fills(args.markets_jsonl, args.strategy, args.target)
    if len(fills) < args.min_fills:
        raise RuntimeError(f"not enough fills with post-fill paths: {len(fills)}")

    max_ts = max(fill["ts"] for fill in fills)
    test_start = max_ts - args.test_days * 86400 + 300
    train = [fill for fill in fills if fill["ts"] < test_start]
    test = [fill for fill in fills if fill["ts"] >= test_start]
    if len(train) < 200 or len(test) < 50:
        raise RuntimeError(f"not enough train/test fills: train={len(train)} test={len(test)}")

    x_train, y_train, names = matrix(train)
    x_test, y_test, _ = matrix(test)
    mean = x_train.mean(axis=0)
    std = x_train.std(axis=0)
    std[std < 1e-8] = 1.0
    x_train_z = (x_train - mean) / std
    x_test_z = (x_test - mean) / std
    weights = train_weighted_logistic(x_train_z, y_train, args.epochs, args.learning_rate, args.l2)
    p_train = predict(weights, x_train_z)
    p_test = predict(weights, x_test_z)

    raw_coef = weights[1:] / std
    coef = sorted(zip(names, raw_coef), key=lambda kv: abs(kv[1]), reverse=True)[:28]

    train_target = int(y_train.sum())
    test_target = int(y_test.sum())
    train_pnl = sum(fill["pnl"] for fill in train)
    test_pnl = sum(fill["pnl"] for fill in test)

    by_lane: dict[str, dict[str, Any]] = defaultdict(lambda: defaultdict(float))
    for fill, p in zip(test, p_test):
        row = by_lane[fill["tag"]]
        row["fills"] += 1
        row["pnl"] += fill["pnl"]
        row["cost"] += fill["notional"]
        row["target"] += 1 if fill["target"] else 0
        row["cross"] += 1 if fill["post_crossed_mid"] else 0
        row["prob_sum"] += float(p)
        row["adverse_sum"] += fill["post_adverse_excursion"]

    lines = [
        "# BTC5m Post-Fill Reversal Model",
        "",
        f"Source: `{args.source_label or args.markets_jsonl}`",
        f"Target: `{args.target}`. Features are fill-time/replay-safe only; the target uses post-fill path labels for offline diagnosis.",
        f"Train fills: `{len(train)}` before `{dt.datetime.fromtimestamp(test_start, tz=dt.timezone.utc).isoformat()}`",
        f"Test fills: `{len(test)}` in final `{args.test_days}` days",
        "",
        "## Model Quality",
        "",
        "| Split | Positives | Base Rate | Log Loss | Brier | AUC | PnL |",
        "|---|---:|---:|---:|---:|---:|---:|",
        f"| train | {train_target} | {pct(float(y_train.mean()) * 100.0)} | {log_loss(y_train, p_train):.4f} | {brier(y_train, p_train):.4f} | {auc(y_train, p_train):.4f} | {money(train_pnl)} |",
        f"| test | {test_target} | {pct(float(y_test.mean()) * 100.0)} | {log_loss(y_test, p_test):.4f} | {brier(y_test, p_test):.4f} | {auc(y_test, p_test):.4f} | {money(test_pnl)} |",
        "",
        "## Test Risk Buckets",
        "",
        "| Risk Bucket | Fills | Avg Risk | PnL | Cost | Target Rate | Cross-Mid Rate | Avg Adverse | Avg Final Side Mid |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in bucket_rows(test, p_test, 5):
        lines.append(
            f"| {row['bucket']} | {row['fills']} | {row['avg_prob']:.4f} | {money(row['pnl'])} | "
            f"{money(row['cost'])} | {pct(row['target_rate'] * 100.0)} | "
            f"{pct(row['cross_rate'] * 100.0)} | {row['avg_adverse']:.4f} | "
            f"{row['avg_final_side_mid']:.4f} |"
        )

    lines.extend(
        [
            "",
            "## Candidate Removal Diagnostics",
            "",
            "Thresholds are fitted from train risk quantiles and applied to the final test window. Positive removed PnL means the gate would have removed good trades, so only negative removed PnL is interesting.",
            "",
            "| Train Quantile | Threshold | Lane | Train Removed | Train Removed PnL | Test Removed | Test Removed PnL | Test Kept PnL | Removed Target Rate | Removed Cross Rate |",
            "|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for row in removal_rows(p_train, train, p_test, test, [0.70, 0.80, 0.90, 0.95]):
        lines.append(
            f"| {row['quantile']:.2f} | {row['threshold']:.4f} | {row['lane']} | "
            f"{row['train_removed']} | {money(row['train_removed_pnl'])} | "
            f"{row['test_removed']} | {money(row['test_removed_pnl'])} | "
            f"{money(row['test_kept_pnl'])} | {pct(row['removed_target_rate'] * 100.0)} | "
            f"{pct(row['removed_cross_rate'] * 100.0)} |"
        )

    lines.extend(
        [
            "",
            "## Test By Lane",
            "",
            "| Lane | Fills | PnL | Cost | Target Rate | Cross-Mid Rate | Avg Adverse | Avg Risk |",
            "|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for lane, row in sorted(by_lane.items()):
        fills_count = int(row["fills"])
        lines.append(
            f"| {lane} | {fills_count} | {money(row['pnl'])} | {money(row['cost'])} | "
            f"{pct(row['target'] / fills_count * 100.0 if fills_count else 0.0)} | "
            f"{pct(row['cross'] / fills_count * 100.0 if fills_count else 0.0)} | "
            f"{row['adverse_sum'] / fills_count if fills_count else 0.0:.4f} | "
            f"{row['prob_sum'] / fills_count if fills_count else 0.0:.4f} |"
        )

    lines.extend(
        [
            "",
            "## Largest Coefficients",
            "",
            "| Feature | Coefficient |",
            "|---|---:|",
        ]
    )
    for name, value in coef:
        lines.append(f"| {name} | {value:.4f} |")
    lines.append("")

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
