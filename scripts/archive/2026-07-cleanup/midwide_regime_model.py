#!/usr/bin/env python3
"""Train a replay-safe classifier for the BTC5m mid-wide/toxic fill bucket."""

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

TAG_FEATURES = [
    "br2_high_skew_load",
    "br2_late_confirm",
    "br2_late_favourite_load",
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


def mean_tail(values: deque[float], n: int) -> float:
    if not values:
        return 0.0
    tail = list(values)[-n:]
    return sum(tail) / len(tail) if tail else 0.0


def is_midwide(volatility_range: float) -> bool:
    return 0.78 <= volatility_range < 0.93


def load_fills(path: str, strategy: str) -> list[dict[str, Any]]:
    f, proc = open_input(path)
    fills: list[dict[str, Any]] = []
    prior_ranges: deque[float] = deque(maxlen=7 * 288)
    try:
        for row in iter_rows(f):
            strat = strategy_result(row, strategy)
            ts = close_ts(row)
            final_range = float(row.get("volatility_range") or 0.0)
            resolved_yes = yes_resolved(row, strat)
            prior_1d = mean_tail(prior_ranges, 288)
            prior_3d = mean_tail(prior_ranges, 3 * 288)
            prior_7d = mean_tail(prior_ranges, 7 * 288)
            for fill in strat.get("fills_detail") or []:
                tag = str(fill.get("tag") or "unknown")
                if tag not in TAG_FEATURES:
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
                    "midwide": is_midwide(final_range),
                    "toxic_midwide": is_midwide(final_range) and pnl < 0.0,
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
    return fills


def matrix(fills: list[dict[str, Any]], target: str) -> tuple[np.ndarray, np.ndarray, list[str]]:
    names = list(NUMERIC_FEATURES)
    names.extend(["buy_yes"])
    names.extend([f"tag:{tag}" for tag in TAG_FEATURES])
    names.extend(
        [
            "whipsaw_x_reversal",
            "whipsaw_x_low_efficiency",
            "range_x_reversal",
            "range_x_sign_flip",
            "price_x_model_p",
            "edge_x_confidence",
            "prior1d_x_range",
            "prior7d_minus_1d",
        ]
    )
    rows = []
    y = []
    for fill in fills:
        row = [float(fill.get(key) or 0.0) for key in NUMERIC_FEATURES]
        row.append(1.0 if fill.get("side") == "BuyYes" else 0.0)
        row.extend(1.0 if fill.get("tag") == tag else 0.0 for tag in TAG_FEATURES)
        whipsaw = float(fill.get("regime_whipsaw_score") or 0.0)
        reversal = float(fill.get("regime_reversal_pressure") or 0.0)
        path_eff = float(fill.get("regime_path_efficiency") or 0.0)
        obs_range = float(fill.get("market_yes_range_so_far") or 0.0)
        sign_flip = float(fill.get("regime_sign_flip_rate") or 0.0)
        price = float(fill.get("price") or 0.0)
        model_p = float(fill.get("side_model_p") or 0.5)
        edge = float(fill.get("side_edge_vs_fill") or 0.0)
        conf = float(fill.get("confidence_score") or 0.0)
        prior_1d = float(fill.get("prior_market_range_1d") or 0.0)
        prior_7d = float(fill.get("prior_market_range_7d") or 0.0)
        row.extend(
            [
                whipsaw * reversal,
                whipsaw * (1.0 - path_eff),
                obs_range * reversal,
                obs_range * sign_flip,
                price * model_p,
                edge * conf,
                prior_1d * obs_range,
                prior_7d - prior_1d,
            ]
        )
        rows.append(row)
        y.append(float(fill[target]))
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


def bucket_rows(fills: list[dict[str, Any]], probs: np.ndarray, buckets: int = 5) -> list[dict[str, Any]]:
    order = np.argsort(probs)
    rows = []
    for i, idxs in enumerate(np.array_split(order, buckets), 1):
        selected = [fills[int(idx)] for idx in idxs]
        ps = [float(probs[int(idx)]) for idx in idxs]
        if not selected:
            continue
        toxic = [fill for fill in selected if fill["toxic_midwide"]]
        rows.append(
            {
                "bucket": i,
                "fills": len(selected),
                "avg_prob": sum(ps) / len(ps),
                "pnl": sum(fill["pnl"] for fill in selected),
                "cost": sum(fill["notional"] for fill in selected),
                "midwide_rate": sum(1 for fill in selected if fill["midwide"]) / len(selected),
                "toxic_rate": len(toxic) / len(selected),
                "toxic_pnl": sum(fill["pnl"] for fill in toxic),
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
        for lane in ["all", *TAG_FEATURES]:
            train_lane = train if lane == "all" else [f for f in train if f["tag"] == lane]
            test_lane = test if lane == "all" else [f for f in test if f["tag"] == lane]
            train_lane_probs = train_probs if lane == "all" else np.asarray(
                [p for p, f in zip(train_probs, train) if f["tag"] == lane],
                dtype=np.float64,
            )
            test_lane_probs = test_probs if lane == "all" else np.asarray(
                [p for p, f in zip(test_probs, test) if f["tag"] == lane],
                dtype=np.float64,
            )
            train_removed = [f for f, p in zip(train_lane, train_lane_probs) if p >= threshold]
            test_removed = [f for f, p in zip(test_lane, test_lane_probs) if p >= threshold]
            test_kept = [f for f, p in zip(test_lane, test_lane_probs) if p < threshold]
            if not test_removed:
                continue
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
                    "test_removed_toxic_rate": sum(1 for f in test_removed if f["toxic_midwide"]) / len(test_removed),
                }
            )
    return rows


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile")
    parser.add_argument("--test-days", type=int, default=30)
    parser.add_argument("--target", choices=["midwide", "toxic_midwide"], default="toxic_midwide")
    parser.add_argument("--epochs", type=int, default=3000)
    parser.add_argument("--learning-rate", type=float, default=0.035)
    parser.add_argument("--l2", type=float, default=0.02)
    parser.add_argument("--source-label")
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    fills = load_fills(args.markets_jsonl, args.strategy)
    if len(fills) < 500:
        raise RuntimeError(f"not enough fills: {len(fills)}")

    max_ts = max(fill["ts"] for fill in fills)
    test_start = max_ts - args.test_days * 86400 + 300
    train = [fill for fill in fills if fill["ts"] < test_start]
    test = [fill for fill in fills if fill["ts"] >= test_start]
    if len(train) < 200 or len(test) < 50:
        raise RuntimeError(f"not enough train/test fills: train={len(train)} test={len(test)}")

    x_train, y_train, names = matrix(train, args.target)
    x_test, y_test, _ = matrix(test, args.target)
    mean = x_train.mean(axis=0)
    std = x_train.std(axis=0)
    std[std < 1e-8] = 1.0
    x_train_z = (x_train - mean) / std
    x_test_z = (x_test - mean) / std
    weights = train_weighted_logistic(x_train_z, y_train, args.epochs, args.learning_rate, args.l2)
    p_train = predict(weights, x_train_z)
    p_test = predict(weights, x_test_z)

    raw_coef = weights[1:] / std
    coef = sorted(zip(names, raw_coef), key=lambda kv: abs(kv[1]), reverse=True)[:24]
    train_pnl = sum(fill["pnl"] for fill in train)
    test_pnl = sum(fill["pnl"] for fill in test)
    train_target = int(y_train.sum())
    test_target = int(y_test.sum())

    by_lane: dict[str, dict[str, Any]] = defaultdict(lambda: defaultdict(float))
    for fill, p in zip(test, p_test):
        lane = fill["tag"]
        row = by_lane[lane]
        row["fills"] += 1
        row["pnl"] += fill["pnl"]
        row["cost"] += fill["notional"]
        row["target"] += 1 if fill[args.target] else 0
        row["prob_sum"] += float(p)

    lines = [
        "# BTC5m Mid-Wide Regime Model",
        "",
        f"Source: `{args.source_label or args.markets_jsonl}`",
        f"Target: `{args.target}` using replay-safe fill-time features only.",
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
        "| Risk Bucket | Fills | Avg Risk | PnL | Cost | Mid-Wide Rate | Toxic Rate | Toxic PnL |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in bucket_rows(test, p_test, 5):
        lines.append(
            f"| {row['bucket']} | {row['fills']} | {row['avg_prob']:.4f} | {money(row['pnl'])} | "
            f"{money(row['cost'])} | {pct(row['midwide_rate'] * 100.0)} | "
            f"{pct(row['toxic_rate'] * 100.0)} | {money(row['toxic_pnl'])} |"
        )

    lines.extend(
        [
            "",
            "## Candidate Removal Diagnostics",
            "",
            "Thresholds are fitted from train risk quantiles and then applied to the final test window. Removing a negative-PnL bucket is only a diagnostic; it still needs a clean backtest implementation.",
            "",
            "| Train Quantile | Threshold | Lane | Train Removed | Train Removed PnL | Test Removed | Test Removed PnL | Test Kept PnL | Removed Toxic Rate |",
            "|---:|---:|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for row in removal_rows(p_train, train, p_test, test, [0.70, 0.80, 0.90, 0.95]):
        lines.append(
            f"| {row['quantile']:.2f} | {row['threshold']:.4f} | {row['lane']} | "
            f"{row['train_removed']} | {money(row['train_removed_pnl'])} | "
            f"{row['test_removed']} | {money(row['test_removed_pnl'])} | "
            f"{money(row['test_kept_pnl'])} | {pct(row['test_removed_toxic_rate'] * 100.0)} |"
        )

    lines.extend(
        [
            "",
            "## Test By Lane",
            "",
            "| Lane | Fills | PnL | Cost | Target Rate | Avg Risk |",
            "|---|---:|---:|---:|---:|---:|",
        ]
    )
    for lane, row in sorted(by_lane.items()):
        fills_count = int(row["fills"])
        lines.append(
            f"| {lane} | {fills_count} | {money(row['pnl'])} | {money(row['cost'])} | "
            f"{pct(row['target'] / fills_count * 100.0 if fills_count else 0.0)} | "
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
