#!/usr/bin/env python3
"""Train DirModel on May/June dir samples (time-ordered split).

Usage:
  python3 scripts/mayjune_dir_train.py data/runs/mayjune_calibrate/dir_samples_may_train.jsonl \\
    data/runs/mayjune_calibrate/dir_model_may_train.json

Split: first 70% of samples by decision_ts_ns = train, rest = test.
Writes {w,b,mu,sd} JSON for pm-alpha DirModel::load_json.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

NAMES = [
    "funding_rate_bps",
    "oi_delta_5m",
    "oi_delta_30m",
    "basis_bps",
    "perp_flow_imbal_60s",
    "perp_burst_300s",
    "liq_proxy",
    "spot_flow_imbal_60s",
    "trend_60s_sigma",
    "trend_300s_sigma",
    "trend_1800s_sigma",
    "trend_alignment",
    "vol_expansion",
    "tau_fraction",
]


def orient(row: dict) -> tuple[list[float], float]:
    v = row["features"]["values"]
    sgn = 1.0 if row["move_up"] else -1.0
    x = [
        v[0] * sgn,
        v[1],
        v[2],
        v[3] * sgn,
        v[4] * sgn,
        v[5],
        v[6],
        v[7] * sgn,
        abs(v[8]),
        abs(v[9]),
        abs(v[10]),
        v[11] * sgn,
        v[12],
        v[13],
    ]
    y = 1.0 if (row["move_up"] == row["resolved_yes"]) else 0.0
    return x, y


def fit_logistic(X: np.ndarray, y: np.ndarray, l2: float = 1e-3, lr: float = 0.1, epochs: int = 400):
    mu, sd = X.mean(0), X.std(0) + 1e-9
    Xn = (X - mu) / sd
    w = np.zeros(X.shape[1])
    b = float(np.log(y.mean() / (1 - y.mean() + 1e-9)))
    n = len(y)
    for _ in range(epochs):
        p = 1.0 / (1.0 + np.exp(-(Xn @ w + b)))
        g = Xn.T @ (p - y) / n + l2 * w
        gb = (p - y).mean()
        w -= lr * g
        b -= lr * gb
    return w, b, mu, sd


def log_loss(Xn: np.ndarray, y: np.ndarray, w: np.ndarray, b: float) -> float:
    p = 1.0 / (1.0 + np.exp(-(Xn @ w + b)))
    p = np.clip(p, 1e-6, 1 - 1e-6)
    return float(-np.mean(y * np.log(p) + (1 - y) * np.log(1 - p)))


def main() -> int:
    samples_path = Path(sys.argv[1])
    out_path = Path(sys.argv[2])
    rows = []
    for line in samples_path.read_text().splitlines():
        if line.strip():
            rows.append(json.loads(line))
    rows.sort(key=lambda r: int(r.get("decision_ts_ns") or 0))
    if len(rows) < 200:
        print(f"too few samples ({len(rows)}) — need >= 200", file=sys.stderr)
        return 1

    split = int(len(rows) * 0.7)
    train_rows, test_rows = rows[:split], rows[split:]
    train_X, train_y, test_X, test_y = [], [], [], []
    for r in train_rows:
        x, y = orient(r)
        train_X.append(x)
        train_y.append(y)
    for r in test_rows:
        x, y = orient(r)
        test_X.append(x)
        test_y.append(y)

    Xtr, ytr = np.array(train_X), np.array(train_y)
    Xte, yte = np.array(test_X), np.array(test_y)
    w, b, mu, sd = fit_logistic(Xtr, ytr)
    Xtr_n = (Xtr - mu) / sd
    Xte_n = (Xte - mu) / sd
    ll_tr = log_loss(Xtr_n, ytr, w, b)
    ll_te = log_loss(Xte_n, yte, w, b)
    base_te = float(-np.mean(yte * np.log(yte.mean() + 1e-9) + (1 - yte) * np.log(1 - yte.mean() + 1e-9)))

    out = {
        "feature_names": NAMES,
        "w": w.tolist(),
        "b": float(b),
        "mu": mu.tolist(),
        "sd": sd.tolist(),
        "train_n": len(train_rows),
        "test_n": len(test_rows),
        "train_log_loss": ll_tr,
        "test_log_loss": ll_te,
        "test_base_log_loss": base_te,
    }
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(out, indent=2) + "\n")
    print(
        f"dir model: train={len(train_rows)} test={len(test_rows)} "
        f"ll_train={ll_tr:.4f} ll_test={ll_te:.4f} base_test={base_te:.4f}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())