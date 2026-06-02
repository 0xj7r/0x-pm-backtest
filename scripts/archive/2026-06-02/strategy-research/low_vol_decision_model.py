#!/usr/bin/env python3
"""Train a first-pass low-vol directional model from walk-forward decision logs.

Labels use the eventual market outcome, but every feature is taken from the
decision row at replay time. The model is evaluated by side EV:
predicted_win_probability - side_ask.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from pathlib import Path
from typing import Any

import numpy as np


BASE_FEATURES = [
    "side_price",
    "legacy_side_p",
    "legacy_edge",
    "confidence_score",
    "risk_score",
    "seconds_to_close",
    "yes_mid",
    "market_mid",
    "feature_observed_yes_range_so_far",
    "feature_momentum",
    "feature_book_imbalance_top3",
    "feature_microprice_dev",
    "feature_microprice_spot_alignment",
    "feature_top3_delta_5s",
    "feature_top3_delta_15s",
    "feature_spot_score",
    "feature_spot_fast_momentum",
    "feature_spot_broad_momentum",
    "feature_spot_momentum_600s",
    "feature_spot_momentum_1800s",
    "feature_spot_1h_4h_alignment",
    "feature_spot_fast_long_alignment",
    "feature_direction_raw",
    "feature_stability",
    "feature_sign_persistence",
    "feature_markov_persistence",
    "feature_whipsaw",
    "feature_path_risk",
    "feature_imbalance_turn",
    "feature_markov_reversal_risk",
    "feature_volatility_penalty",
    "feature_volatility_regime",
    "feature_dir_flip_rate_8",
    "feature_dir_std_8",
    "feature_dir_abs_mean_8",
]


def parse_close_ts(slug: str, fallback: int) -> int:
    try:
        return int(slug.rsplit("-", 1)[1]) + 300
    except Exception:
        return fallback


def load_market_resolution(
    markets_jsonl: str,
    manifest_jsonl: str,
    skip_markets: int,
    max_markets: int,
) -> dict[int, bool]:
    by_slug: dict[str, bool] = {}
    with open(markets_jsonl) as fh:
        for line in fh:
            if not line.strip():
                continue
            row = json.loads(line)
            br2 = (row.get("per_strategy") or {}).get("bonereaper_v2") or {}
            by_slug[str(row.get("slug"))] = bool(br2.get("yes_resolved"))

    manifest = []
    with open(manifest_jsonl) as fh:
        for line in fh:
            if line.strip():
                row = json.loads(line)
                row["_close_sort"] = int(row.get("close_ts") or parse_close_ts(str(row.get("slug")), 0))
                manifest.append(row)
    manifest.sort(key=lambda row: row["_close_sort"])
    if skip_markets:
        manifest = manifest[skip_markets:]
    if max_markets:
        manifest = manifest[:max_markets]

    out: dict[int, bool] = {}
    for idx, row in enumerate(manifest, 1):
        slug = str(row.get("slug"))
        if slug in by_slug:
            out[idx] = by_slug[slug]
    return out


def iter_rows(path: str):
    with open(path) as fh:
        for line in fh:
            if line.strip():
                yield json.loads(line)


def row_value(row: dict[str, Any], name: str) -> float:
    return float(row.get(name) or 0.0)


def make_examples(args: argparse.Namespace) -> list[dict[str, Any]]:
    resolved = load_market_resolution(
        args.markets_jsonl,
        args.manifest_jsonl,
        args.skip_markets,
        args.max_markets,
    )
    examples = []
    for row in iter_rows(args.decision_log):
        market_id = int(row.get("market_id") or 0)
        if market_id not in resolved:
            continue
        if not row.get("has_model_output"):
            continue
        side_is_yes = bool(row.get("side_is_yes"))
        side_price = float(row.get("yes_ask") if side_is_yes else 1.0 - float(row.get("yes_bid") or 0.0))
        if side_price < args.min_price or side_price > args.max_price:
            continue
        close_ns = (int(row.get("market_id") or 0),)
        ts_s = int(row.get("ts_ns") or 0) / 1_000_000_000.0
        # Decision logs do not carry close_ts; recover seconds-to-close from
        # the feature row when possible, else use the 5m market clock proxy.
        seconds_to_close = 300.0 - ((ts_s % 300.0 + 300.0) % 300.0)
        if seconds_to_close < args.min_seconds_to_close or seconds_to_close > args.max_seconds_to_close:
            continue
        vol = row_value(row, "feature_volatility_regime")
        if vol > args.max_volatility_regime:
            continue
        legacy_side_p = row_value(row, "calibrated_p")
        if legacy_side_p < args.min_legacy_side_p:
            continue
        won = side_is_yes == resolved[market_id]
        item = dict(row)
        item["side_price"] = side_price
        item["legacy_side_p"] = legacy_side_p
        item["legacy_edge"] = legacy_side_p - side_price
        item["seconds_to_close"] = seconds_to_close
        item["won"] = won
        item["ev_realized"] = (1.0 if won else 0.0) - side_price
        item["ts_s"] = ts_s
        examples.append(item)
    examples.sort(key=lambda row: (row["ts_s"], int(row.get("market_id") or 0), int(row.get("event_idx") or 0)))
    return examples


def matrix(rows: list[dict[str, Any]]) -> tuple[np.ndarray, np.ndarray, list[str]]:
    names = list(BASE_FEATURES)
    names.extend(
        [
            "buy_yes",
            "price_x_legacy_p",
            "edge_x_conf",
            "range_x_flip",
            "range_x_whipsaw",
            "risk_x_range",
            "fast_x_broad_spot",
            "price_x_seconds",
        ]
    )
    x = []
    y = []
    for row in rows:
        values = [row_value(row, name) for name in BASE_FEATURES]
        side_price = row_value(row, "side_price")
        legacy_p = row_value(row, "legacy_side_p")
        legacy_edge = row_value(row, "legacy_edge")
        conf = row_value(row, "confidence_score")
        obs_range = row_value(row, "feature_observed_yes_range_so_far")
        flip = row_value(row, "feature_dir_flip_rate_8")
        whipsaw = row_value(row, "feature_whipsaw")
        risk = row_value(row, "risk_score")
        fast = row_value(row, "feature_spot_fast_momentum")
        broad = row_value(row, "feature_spot_broad_momentum")
        seconds = row_value(row, "seconds_to_close")
        values.extend(
            [
                1.0 if row.get("side_is_yes") else 0.0,
                side_price * legacy_p,
                legacy_edge * conf,
                obs_range * flip,
                obs_range * whipsaw,
                risk * obs_range,
                fast * broad,
                side_price * seconds / 300.0,
            ]
        )
        x.append(values)
        y.append(1.0 if row["won"] else 0.0)
    return np.asarray(x, dtype=np.float64), np.asarray(y, dtype=np.float64), names


def sigmoid(x: np.ndarray) -> np.ndarray:
    return 1.0 / (1.0 + np.exp(-np.clip(x, -35.0, 35.0)))


def train_logistic(x_train: np.ndarray, y_train: np.ndarray, epochs: int, lr: float, l2: float) -> np.ndarray:
    x_aug = np.c_[np.ones(len(x_train)), x_train]
    weights = np.zeros(x_aug.shape[1])
    pos_rate = y_train.mean()
    pos_w = 0.5 / max(pos_rate, 1e-6)
    neg_w = 0.5 / max(1.0 - pos_rate, 1e-6)
    sample_w = np.where(y_train > 0.5, pos_w, neg_w)
    for _ in range(epochs):
        p = sigmoid(x_aug @ weights)
        grad = (x_aug.T @ ((p - y_train) * sample_w)) / len(y_train)
        grad[1:] += l2 * weights[1:]
        weights -= lr * grad
    return weights


def predict(weights: np.ndarray, x: np.ndarray) -> np.ndarray:
    return sigmoid(np.c_[np.ones(len(x)), x] @ weights)


def auc(y: np.ndarray, p: np.ndarray) -> float:
    order = np.argsort(p)
    ranks = np.empty_like(order, dtype=np.float64)
    ranks[order] = np.arange(1, len(p) + 1)
    n_pos = float(y.sum())
    n_neg = float(len(y) - y.sum())
    if n_pos == 0.0 or n_neg == 0.0:
        return math.nan
    return float((ranks[y > 0.5].sum() - n_pos * (n_pos + 1.0) / 2.0) / (n_pos * n_neg))


def log_loss(y: np.ndarray, p: np.ndarray) -> float:
    p = np.clip(p, 1e-5, 1.0 - 1e-5)
    return float(-(y * np.log(p) + (1.0 - y) * np.log(1.0 - p)).mean())


def money(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{100.0 * value:.2f}%"


def bucket_table(rows: list[dict[str, Any]], scores: np.ndarray, buckets: int) -> list[str]:
    lines = [
        "| Bucket | Rows | Avg Score | Avg Price | Win Rate | Realized EV | Legacy EV |",
        "|---:|---:|---:|---:|---:|---:|---:|",
    ]
    order = np.argsort(scores)
    for i, idxs in enumerate(np.array_split(order, buckets), 1):
        subset = [rows[int(idx)] for idx in idxs]
        if not subset:
            continue
        avg_score = float(np.mean(scores[idxs]))
        ev = sum(float(row["ev_realized"]) for row in subset)
        legacy = sum(float(row["legacy_edge"]) for row in subset)
        win_rate = sum(1 for row in subset if row["won"]) / len(subset)
        avg_price = sum(float(row["side_price"]) for row in subset) / len(subset)
        lines.append(
            f"| {i} | {len(subset)} | {avg_score:.4f} | {avg_price:.4f} | "
            f"{pct(win_rate)} | {money(ev)} | {legacy:.2f} |"
        )
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("decision_log")
    parser.add_argument("--markets-jsonl", required=True)
    parser.add_argument(
        "--manifest-jsonl",
        default="data/runs/volgate/markets-may-labeled.jsonl",
    )
    parser.add_argument("--skip-markets", type=int, default=0)
    parser.add_argument("--max-markets", type=int, default=0)
    parser.add_argument("--train-frac", type=float, default=0.70)
    parser.add_argument("--min-price", type=float, default=0.45)
    parser.add_argument("--max-price", type=float, default=0.97)
    parser.add_argument("--min-seconds-to-close", type=float, default=15.0)
    parser.add_argument("--max-seconds-to-close", type=float, default=180.0)
    parser.add_argument("--max-volatility-regime", type=float, default=1.25)
    parser.add_argument("--min-legacy-side-p", type=float, default=0.55)
    parser.add_argument("--epochs", type=int, default=2500)
    parser.add_argument("--learning-rate", type=float, default=0.035)
    parser.add_argument("--l2", type=float, default=0.02)
    parser.add_argument("--out-md", required=True)
    parser.add_argument("--out-json", help="write machine-readable model artifact")
    args = parser.parse_args()

    rows = make_examples(args)
    if len(rows) < 500:
        raise RuntimeError(f"not enough examples after filters: {len(rows)}")
    split = max(1, min(len(rows) - 1, int(len(rows) * args.train_frac)))
    train = rows[:split]
    test = rows[split:]
    x_train, y_train, names = matrix(train)
    x_test, y_test, _ = matrix(test)
    mean = x_train.mean(axis=0)
    std = x_train.std(axis=0)
    std[std < 1e-8] = 1.0
    x_train_z = (x_train - mean) / std
    x_test_z = (x_test - mean) / std
    weights = train_logistic(x_train_z, y_train, args.epochs, args.learning_rate, args.l2)
    p_train = predict(weights, x_train_z)
    p_test = predict(weights, x_test_z)
    test_prices = np.asarray([row["side_price"] for row in test])
    specialist_ev = p_test - test_prices
    legacy_ev = np.asarray([row["legacy_edge"] for row in test])
    raw_coef = weights[1:] / std
    coefs = sorted(zip(names, raw_coef), key=lambda kv: abs(kv[1]), reverse=True)[:30]

    def realized(selected: list[dict[str, Any]]) -> float:
        return sum(float(row["ev_realized"]) for row in selected)

    lines = [
        "# Low-Vol Decision Model",
        "",
        f"Decision log: `{args.decision_log}`",
        f"Examples after filters: `{len(rows)}`",
        f"Train rows: `{len(train)}`; test rows: `{len(test)}`",
        f"Filters: price `{args.min_price}-{args.max_price}`, seconds_to_close `{args.min_seconds_to_close}-{args.max_seconds_to_close}`, volatility_regime <= `{args.max_volatility_regime}`, legacy_side_p >= `{args.min_legacy_side_p}`",
        "",
        "## Model Quality",
        "",
        "| Split | Win Rate | Log Loss | AUC | Realized EV |",
        "|---|---:|---:|---:|---:|",
        f"| train | {pct(float(y_train.mean()))} | {log_loss(y_train, p_train):.4f} | {auc(y_train, p_train):.4f} | {money(realized(train))} |",
        f"| test | {pct(float(y_test.mean()))} | {log_loss(y_test, p_test):.4f} | {auc(y_test, p_test):.4f} | {money(realized(test))} |",
        "",
        "## Test Buckets By Specialist EV",
        "",
        *bucket_table(test, specialist_ev, 5),
        "",
        "## Test Buckets By Legacy Edge",
        "",
        *bucket_table(test, legacy_ev, 5),
        "",
        "## Positive-EV Candidate Slices",
        "",
        "| Gate | Rows | Win Rate | Avg Price | Realized EV |",
        "|---|---:|---:|---:|---:|",
    ]
    for name, mask in [
        ("specialist_ev_gt_0", specialist_ev > 0.0),
        ("specialist_ev_top20pct", specialist_ev >= np.quantile(specialist_ev, 0.80)),
        ("legacy_edge_gt_0", legacy_ev > 0.0),
        ("legacy_edge_top20pct", legacy_ev >= np.quantile(legacy_ev, 0.80)),
    ]:
        selected = [row for row, keep in zip(test, mask) if bool(keep)]
        if not selected:
            continue
        win_rate = sum(1 for row in selected if row["won"]) / len(selected)
        avg_price = sum(float(row["side_price"]) for row in selected) / len(selected)
        lines.append(
            f"| {name} | {len(selected)} | {pct(win_rate)} | {avg_price:.4f} | {money(realized(selected))} |"
        )
    lines.extend(["", "## Largest Coefficients", "", "| Feature | Coefficient |", "|---|---:|"])
    for name, coef in coefs:
        lines.append(f"| {name} | {coef:.4f} |")
    lines.append("")

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines))
    if args.out_json:
        artifact = {
            "kind": "low_vol_directional_logistic",
            "decision_log": args.decision_log,
            "markets_jsonl": args.markets_jsonl,
            "manifest_jsonl": args.manifest_jsonl,
            "filters": {
                "skip_markets": args.skip_markets,
                "max_markets": args.max_markets,
                "train_frac": args.train_frac,
                "min_price": args.min_price,
                "max_price": args.max_price,
                "min_seconds_to_close": args.min_seconds_to_close,
                "max_seconds_to_close": args.max_seconds_to_close,
                "max_volatility_regime": args.max_volatility_regime,
                "min_legacy_side_p": args.min_legacy_side_p,
            },
            "training": {
                "epochs": args.epochs,
                "learning_rate": args.learning_rate,
                "l2": args.l2,
                "examples": len(rows),
                "train_rows": len(train),
                "test_rows": len(test),
            },
            "metrics": {
                "train_win_rate": float(y_train.mean()),
                "train_log_loss": log_loss(y_train, p_train),
                "train_auc": auc(y_train, p_train),
                "train_realized_ev": realized(train),
                "test_win_rate": float(y_test.mean()),
                "test_log_loss": log_loss(y_test, p_test),
                "test_auc": auc(y_test, p_test),
                "test_realized_ev": realized(test),
            },
            "features": names,
            "standardization": {
                "mean": [float(v) for v in mean],
                "std": [float(v) for v in std],
            },
            "weights": [float(v) for v in weights],
            "raw_coefficients": {
                name: float(coef)
                for name, coef in zip(names, raw_coef)
            },
        }
        out_json = Path(args.out_json)
        out_json.parent.mkdir(parents=True, exist_ok=True)
        out_json.write_text(json.dumps(artifact, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
