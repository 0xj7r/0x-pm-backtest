#!/usr/bin/env python3
"""Train/evaluate P(continuation | DirFeatures) on the Feb-Apr dataset.
Pure numpy logistic regression (L2, gradient descent) — the science gate:
does the feature set beat the naive base rate out-of-sample (train Feb-Mar,
test Apr), measured by log-loss and lift in the top deciles?

Usage: python3 scripts/dir_train.py data/runs/alpha/dir [model_out.json]
The optional second arg writes {w,b,mu,sd} for pm-alpha's DirModel (the Rust
side replicates the move-relative orientation in `load()` below exactly).
"""
import glob
import json
import sys

import numpy as np

NAMES = ["funding_rate_bps", "oi_delta_5m", "oi_delta_30m", "basis_bps",
         "perp_flow_imbal_60s", "perp_burst_300s", "liq_proxy",
         "spot_flow_imbal_60s", "trend_60s_sigma", "trend_300s_sigma",
         "trend_1800s_sigma", "trend_alignment", "vol_expansion", "tau_fraction"]


def load(dir_path):
    train_X, train_y, test_X, test_y = [], [], [], []
    for f in sorted(glob.glob(f"{dir_path}/dir_*.jsonl")):
        if "febapr_all" in f:
            continue
        is_test = "apr" in f
        for line in open(f):
            r = json.loads(line)
            v = r["features"]["values"]
            # orient move-relative: signed features flip with move direction
            sgn = 1.0 if r["move_up"] else -1.0
            x = [v[0] * sgn, v[1], v[2], v[3] * sgn, v[4] * sgn, v[5], v[6],
                 v[7] * sgn, abs(v[8]), abs(v[9]), abs(v[10]),
                 v[11] * sgn, v[12], v[13]]
            y = 1.0 if (r["move_up"] == r["resolved_yes"]) else 0.0
            (test_X if is_test else train_X).append(x)
            (test_y if is_test else train_y).append(y)
    return (np.array(train_X), np.array(train_y),
            np.array(test_X), np.array(test_y))


def fit_logistic(X, y, l2=1e-3, lr=0.1, epochs=300):
    mu, sd = X.mean(0), X.std(0) + 1e-9
    Xn = (X - mu) / sd
    w = np.zeros(X.shape[1])
    b = np.log(y.mean() / (1 - y.mean()))
    n = len(y)
    for _ in range(epochs):
        p = 1 / (1 + np.exp(-(Xn @ w + b)))
        g = Xn.T @ (p - y) / n + l2 * w
        gb = (p - y).mean()
        w -= lr * g
        b -= lr * gb
    return w, b, mu, sd


def main():
    d = sys.argv[1]
    Xtr, ytr, Xte, yte = load(d)
    print(f"train (Feb-Mar): {len(ytr):,}  test (Apr): {len(yte):,}")
    print(f"base continuation: train {ytr.mean():.3f}  test {yte.mean():.3f}")

    w, b, mu, sd = fit_logistic(Xtr, ytr)
    pte = 1 / (1 + np.exp(-(((Xte - mu) / sd) @ w + b)))

    eps = 1e-9
    ll_model = -np.mean(yte * np.log(pte + eps) + (1 - yte) * np.log(1 - pte + eps))
    pb = yte.mean()
    ll_base = -(pb * np.log(pb) + (1 - pb) * np.log(1 - pb))
    print(f"\nApr OOS log-loss: model {ll_model:.4f} vs constant-base {ll_base:.4f} "
          f"({'BEATS' if ll_model < ll_base else 'LOSES TO'} base)")

    order = np.argsort(-pte)
    n = len(pte)
    print("\nrealized continuation by model-score decile (Apr OOS):")
    for k in range(10):
        idx = order[k * n // 10:(k + 1) * n // 10]
        print(f"  decile {k+1:2d}: predicted {pte[idx].mean():.3f}  realized {yte[idx].mean():.3f}  (n={len(idx):,})")

    print("\ntop |weights| (move-oriented, standardized):")
    for i in np.argsort(-np.abs(w))[:8]:
        print(f"  {NAMES[i]:22s} {w[i]:+.3f}")

    if len(sys.argv) > 2:
        out = {"w": w.tolist(), "b": float(b), "mu": mu.tolist(), "sd": sd.tolist()}
        with open(sys.argv[2], "w") as f:
            json.dump(out, f, indent=1)
        print(f"\nmodel written: {sys.argv[2]}")


if __name__ == "__main__":
    main()
