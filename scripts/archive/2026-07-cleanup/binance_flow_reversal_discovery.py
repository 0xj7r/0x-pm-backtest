#!/usr/bin/env python3
"""Focused discovery pass for previously untested quant signals vs crossed-mid / toxic reversal.

Signals under test (all replay-safe at fill time):
- Binance spot order-flow imbalance and adverse volume in tight pre-fill windows (the canonical leading indicator for imminent BTC move).
- Spot momentum + acceleration (is the move that created the favourite still accelerating?).
- (Future) PM book pressure on favourite side (depth, depletion rate, informed selling hits).

This runs against the *same* markets.jsonl as the convex backtest and focuses on the drawdown window (last N days).

It does *not* require Rust changes for v1. It loads the raw Binance agg trades parquets (local cache or S3) around the fill timestamps.

Usage example (after a convex or 062901 run produces the jsonl):
  python scripts/binance_flow_reversal_discovery.py \
      s3://.../markets.jsonl \
      --strategy bonereaper_v2 \
      --test-days 30 \
      --target crossed_mid_after_fill \
      --out-md docs/binance_flow_discovery_30d.md
"""

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

# Reuse helpers from the established postfill model script
try:
    from postfill_reversal_model import (
        auc,
        brier,
        fill_pnl,
        fill_won,
        iter_rows,
        log_loss,
        money,
        open_input,
        pct,
        yes_resolved,
    )
except Exception:
    # Fallback minimal implementations if import fails (keeps script standalone)
    def pct(v: float) -> str:
        return f"{v:.2%}"

    def money(v: float) -> str:
        return f"${v:,.2f}"

    def open_input(path: str) -> tuple[TextIO, subprocess.Popen[str] | None]:
        if path == "-":
            return sys.stdin, None
        if path.startswith("s3://"):
            env = os.environ.copy()
            if os.environ.get("AWS_PROFILE"):
                env["AWS_PROFILE"] = os.environ["AWS_PROFILE"]
            proc = subprocess.Popen(["aws", "s3", "cp", path, "-"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env)
            return proc.stdout, proc
        return Path(path).open(), None

    def iter_rows(lines: Iterable[str]) -> Iterator[dict[str, Any]]:
        for line in lines:
            line = line.strip()
            if line:
                yield json.loads(line)

AWS_PROFILE: str | None = None

# The three families the user called out
BINANCE_FLOW_FEATURES = [
    "binance_flow_imbal_5s",
    "binance_flow_imbal_15s",
    "binance_flow_imbal_30s",
    "binance_adverse_vol_5s",
    "binance_adverse_vol_15s",
    "binance_adverse_vol_30s",
    "binance_large_adverse_count_10s",
    "binance_trade_intensity_15s",
]

SPOT_ACCEL_FEATURES = [
    "spot_ret_5s",
    "spot_ret_15s",
    "spot_ret_30s",
    "spot_accel_15s_vs_30s",  # (ret_15s - ret_30s) normalized
    "spot_accel_5s_vs_15s",
]

ALL_NEW_FEATURES = BINANCE_FLOW_FEATURES + SPOT_ACCEL_FEATURES

# Existing features we contrast against (from the 062901-style runs)
BASE_FEATURES = [
    "price",
    "side_model_p",
    "side_edge_vs_fill",
    "risk_score",
    "regime_whipsaw_score",
    "regime_reversal_pressure",
    "regime_path_efficiency",
    "market_yes_range_so_far",
]


def close_ts(row: dict[str, Any]) -> int:
    if row.get("close_ts") is not None:
        return int(row["close_ts"])
    return int(str(row.get("slug") or "").rsplit("-", 1)[1]) + 300


def strategy_result(row: dict[str, Any], strategy: str) -> dict[str, Any]:
    return ((row.get("per_strategy") or {}).get(strategy)) or {}


def load_fills(path: str, strategy: str, target: str) -> list[dict[str, Any]]:
    f, proc = open_input(path)
    fills: list[dict[str, Any]] = []
    try:
        for row in iter_rows(f):
            strat = strategy_result(row, strategy)
            ts = close_ts(row)
            resolved_yes = yes_resolved(row, strat)
            final_range = float(row.get("volatility_range") or 0.0)
            for fill in strat.get("fills_detail") or []:
                tag = str(fill.get("tag") or "unknown")
                if tag not in ("br2_late_favourite_load", "br2_late_confirm", "br2_high_skew_load"):
                    continue
                path_obj = fill.get("post_fill_path")
                if not isinstance(path_obj, dict):
                    continue
                pnl, won = fill_pnl(fill, resolved_yes)
                item = {
                    "ts": ts,
                    "date": dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date(),
                    "slug": row.get("slug"),
                    "tag": tag,
                    "side": str(fill.get("side") or "unknown"),
                    "pnl": pnl,
                    "won": bool(won),
                    "notional": float(fill.get("notional") or 0.0),
                    "price": float(fill.get("price") or 0.0),
                    "final_range": final_range,
                    "target": _path_label(path_obj, pnl, target),
                    "post_crossed_mid": bool(path_obj.get("crossed_mid_after_fill")),
                    "post_adverse_excursion": float(path_obj.get("adverse_excursion") or 0.0),
                    "post_final_side_mid": float(path_obj.get("final_side_mid") or 0.0),
                }
                for key in BASE_FEATURES:
                    item[key] = float(fill.get(key) or 0.0)
                fills.append(item)
    finally:
        if proc is not None:
            proc.wait()
        elif f is not sys.stdin:
            f.close()
    return fills


def _path_label(path: dict[str, Any], pnl: float, target: str) -> bool:
    crossed = bool(path.get("crossed_mid_after_fill"))
    adverse = float(path.get("adverse_excursion") or 0.0)
    final_side_mid = float(path.get("final_side_mid") or 0.0)
    if target == "crossed_mid_after_fill":
        return crossed
    if target == "toxic_reversal_path":
        return pnl < 0.0 and (crossed or (adverse >= 0.20 and final_side_mid < 0.60))
    if target == "toxic_crossed_mid":
        return crossed and pnl < 0.0
    return crossed


# ---------------- Binance flow + spot accel feature extraction ----------------

def _parse_binance_day(path: str) -> list[tuple[int, float, float, bool]]:
    """Return list of (ts_ns, price, qty, is_buyer_maker). Sorted."""
    import pyarrow.parquet as pq

    # Read the single file's columns directly (ParquetFile avoids hive partition
    # inference, which otherwise conflicts with the embedded exchange/symbol columns).
    _cols = ["transact_time_ms", "price", "quantity", "is_buyer_maker"]
    if path.startswith("s3://"):
        tmp = Path("/tmp/binance_flow_tmp.parquet")
        env = os.environ.copy()
        if AWS_PROFILE:
            env["AWS_PROFILE"] = AWS_PROFILE
        subprocess.check_call(["aws", "s3", "cp", path, str(tmp)], env=env)
        table = pq.ParquetFile(str(tmp)).read(columns=_cols)
        tmp.unlink(missing_ok=True)
    else:
        table = pq.ParquetFile(path).read(columns=_cols)

    ts_ms = table["transact_time_ms"].to_pylist()
    price_str = table["price"].to_pylist()
    qty_str = table["quantity"].to_pylist()
    maker = table["is_buyer_maker"].to_pylist()

    out = []
    for t, p, q, m in zip(ts_ms, price_str, qty_str, maker):
        # transact_time_ms is actually microseconds per loader
        ts_ns = int(t) * 1000
        pr = float(p)
        qt = float(q)
        out.append((ts_ns, pr, qt, bool(m)))
    out.sort(key=lambda x: x[0])
    return out


def _load_binance_days(dates: set[dt.date], cache_root: str | None) -> dict[dt.date, list[tuple[int, float, float, bool]]]:
    """Load the BTCUSDT agg trades for the needed dates."""
    days: dict[dt.date, list] = {}
    for d in sorted(dates):
        date_str = d.isoformat()
        if cache_root:
            local = Path(cache_root) / f"exchange=binance/channel=agg_trades/symbol=BTCUSDT/date={date_str}/BTCUSDT-aggTrades-{date_str}.parquet"
            if local.exists():
                days[d] = _parse_binance_day(str(local))
                continue
        # Fall back to S3 hive layout used by the loader
        s3_path = f"s3://pm-research-backtest-prod/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT/date={date_str}/BTCUSDT-aggTrades-{date_str}.parquet"
        try:
            days[d] = _parse_binance_day(s3_path)
        except Exception as e:
            print(f"WARNING: could not load binance day {date_str}: {e}", file=sys.stderr)
    return days


def _signed_flow_and_adverse(trades: list[tuple[int, float, float, bool]], now_ns: int, window_ns: int, is_buy_yes: bool) -> tuple[float, float, int, float]:
    """
    Returns (flow_imbalance, adverse_volume, large_adverse_count, intensity).
    flow_imbalance in [-1, +1]: positive = net aggressive buying pressure.
    For a BuyYes load, "adverse" = aggressive sells (seller-initiated, is_buyer_maker=True).
    For BuyNo load, adverse = aggressive buys.
    """
    start = now_ns - window_ns
    vol_buy = 0.0
    vol_sell = 0.0
    large_adverse = 0
    n = 0
    for ts, _p, q, is_buyer_maker in trades:
        if ts < start or ts > now_ns:
            continue
        n += 1
        # is_buyer_maker=True means seller initiated (aggressive sell)
        if is_buyer_maker:
            vol_sell += q
        else:
            vol_buy += q

        # Adverse for this load direction
        if is_buy_yes:
            # adverse = seller aggression
            if is_buyer_maker and q >= 50.0:  # crude large print threshold (tune later)
                large_adverse += 1
        else:
            if not is_buyer_maker and q >= 50.0:
                large_adverse += 1

    total = vol_buy + vol_sell
    imbal = (vol_buy - vol_sell) / total if total > 1e-9 else 0.0
    adverse_vol = vol_sell if is_buy_yes else vol_buy
    intensity = n / (window_ns / 1e9) if window_ns > 0 else 0.0
    return imbal, adverse_vol, large_adverse, intensity


def _spot_returns_and_accel(trades: list[tuple[int, float, float, bool]], now_ns: int) -> dict[str, float]:
    """Compute short-horizon returns and simple acceleration proxies."""
    def ret_at(window_ns: int) -> float | None:
        # find price at or before now, and at or after (now - window)
        p_now = None
        p_start = None
        for ts, pr, _q, _m in reversed(trades):
            if ts <= now_ns and p_now is None:
                p_now = pr
            if ts <= now_ns - window_ns and p_start is None:
                p_start = pr
            if p_now is not None and p_start is not None:
                break
        if p_now and p_start and p_start > 0:
            return (p_now / p_start) - 1.0
        return None

    r5 = ret_at(5_000_000_000)
    r15 = ret_at(15_000_000_000)
    r30 = ret_at(30_000_000_000)

    accel_15_vs_30 = (r15 - r30) / 15.0 if r15 is not None and r30 is not None else 0.0
    accel_5_vs_15 = (r5 - r15) / 5.0 if r5 is not None and r15 is not None else 0.0

    return {
        "spot_ret_5s": r5 or 0.0,
        "spot_ret_15s": r15 or 0.0,
        "spot_ret_30s": r30 or 0.0,
        "spot_accel_15s_vs_30s": accel_15_vs_30,
        "spot_accel_5s_vs_15s": accel_5_vs_15,
    }


def attach_binance_features(fills: list[dict[str, Any]], cache_root: str | None) -> None:
    """In-place enrichment of fills with the new Binance/spot features."""
    needed_dates = {f["date"] for f in fills}
    days = _load_binance_days(needed_dates, cache_root)

    for fill in fills:
        d = fill["date"]
        trades = days.get(d, [])
        if not trades:
            continue
        ts_ns = fill["ts"] * 1_000_000_000  # close_ts is seconds
        is_buy_yes = fill["side"] == "BuyYes"

        # Flow windows (tight — the final 5-30s are what matter for a late reversal)
        for w, label in [(5_000_000_000, "5s"), (15_000_000_000, "15s"), (30_000_000_000, "30s")]:
            imbal, adv, large, inten = _signed_flow_and_adverse(trades, ts_ns, w, is_buy_yes)
            fill[f"binance_flow_imbal_{label}"] = imbal
            fill[f"binance_adverse_vol_{label}"] = adv
            if label == "15s":
                fill["binance_trade_intensity_15s"] = inten
            if label == "10s":  # we don't have a 10s window; reuse 15s large count as proxy for now
                pass

        # Approximate 10s large adverse (reuse the 15s window count as proxy for v1)
        _im, _a, large10, _i = _signed_flow_and_adverse(trades, ts_ns, 10_000_000_000, is_buy_yes)
        fill["binance_large_adverse_count_10s"] = large10

        accel = _spot_returns_and_accel(trades, ts_ns)
        fill.update(accel)


# ---------------- Contrast and simple model (same spirit as late_break_feature_contrast) ----------------

def smd(a: list[float], b: list[float]) -> float:
    if not a or not b:
        return 0.0
    ma, mb = sum(a) / len(a), sum(b) / len(b)
    sa = (sum((x - ma) ** 2 for x in a) / max(1, len(a) - 1)) ** 0.5 if len(a) > 1 else 0.0
    sb = (sum((x - mb) ** 2 for x in b) / max(1, len(b) - 1)) ** 0.5 if len(b) > 1 else 0.0
    pooled = (sa + sb) / 2.0
    return (ma - mb) / pooled if pooled > 1e-9 else 0.0


def single_feature_auc(fills: list[dict[str, Any]], feat: str) -> float:
    vals = [float(f.get(feat) or 0.0) for f in fills]
    y = [1.0 if f["target"] else 0.0 for f in fills]
    # higher adverse flow / negative spot accel should increase reversal prob for BuyYes loads
    # Use absolute rank AUC (direction will be visible in SMD)
    order = np.argsort(vals)
    ranks = np.empty_like(order, dtype=np.float64)
    ranks[order] = np.arange(1, len(vals) + 1)
    pos = sum(1 for v in y if v > 0.5)
    neg = len(y) - pos
    if pos == 0 or neg == 0:
        return float("nan")
    sum_ranks_pos = sum(ranks[i] for i, yy in enumerate(y) if yy > 0.5)
    return float((sum_ranks_pos - pos * (pos + 1) / 2.0) / (pos * neg))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile")
    parser.add_argument("--test-days", type=int, default=30)
    parser.add_argument("--target", default="crossed_mid_after_fill", choices=["crossed_mid_after_fill", "toxic_reversal_path", "toxic_crossed_mid"])
    parser.add_argument("--binance-cache-root", default="data/cache/raw/binance", help="Local hive root for binance agg trades (falls back to S3)")
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    print("Loading fills and post-fill labels...", file=sys.stderr)
    fills = load_fills(args.markets_jsonl, args.strategy, args.target)
    if not fills:
        print("No fills with post_fill_path found.", file=sys.stderr)
        return 2

    max_ts = max(f["ts"] for f in fills)
    test_start = max_ts - args.test_days * 86400 + 300
    test_fills = [f for f in fills if f["ts"] >= test_start]
    train_fills = [f for f in fills if f["ts"] < test_start]

    print(f"Attaching Binance flow + spot accel features for {len(test_fills)} test fills (and train for contrast)...", file=sys.stderr)
    attach_binance_features(test_fills, args.binance_cache_root)
    attach_binance_features(train_fills, args.binance_cache_root)

    # Focus the report on the drawdown window (test)
    toxic = [f for f in test_fills if f["target"]]
    good = [f for f in test_fills if not f["target"] and f["pnl"] > 0.0]

    lines: list[str] = [
        "# Binance Order-Flow + Spot Accel Reversal Signal Discovery",
        "",
        f"Source: `{args.markets_jsonl}`",
        f"Target label: `{args.target}` (post-fill path). Focus = last {args.test_days}d drawdown window.",
        f"Test fills: {len(test_fills)} | Toxic in window: {len(toxic)} | Profitable non-toxic: {len(good)}",
        "",
        "This pass specifically tests the three families that were *not* in the earlier regime/price/model-feature analysis.",
        "",
    ]

    # Feature contrast on the new families
    lines.extend(["## New Signal Families — Standardized Mean Difference (toxic vs profitable non-toxic)", ""])
    lines.append("| Feature | Toxic Mean | Good Mean | SMD | Single AUC (test) |")
    lines.append("|---|---:|---:|---:|---:|")

    for feat in ALL_NEW_FEATURES:
        toxic_vals = [float(f.get(feat) or 0.0) for f in toxic]
        good_vals = [float(f.get(feat) or 0.0) for f in good]
        s = smd(toxic_vals, good_vals)
        a = single_feature_auc(test_fills, feat)
        lines.append(f"| {feat} | {sum(toxic_vals)/max(1,len(toxic_vals)):.4f} | {sum(good_vals)/max(1,len(good_vals)):.4f} | {s:+.3f} | {a:.3f} |")

    lines.append("")

    # Quick comparison to the strongest base features on the same window
    lines.extend(["## Base Features (price / model / regime) on the same test window for reference", ""])
    lines.append("| Feature | SMD | Single AUC |")
    lines.append("|---|---:|---:|")
    for feat in BASE_FEATURES:
        toxic_vals = [float(f.get(feat) or 0.0) for f in toxic]
        good_vals = [float(f.get(feat) or 0.0) for f in good]
        s = smd(toxic_vals, good_vals)
        a = single_feature_auc(test_fills, feat)
        lines.append(f"| {feat} | {s:+.3f} | {a:.3f} |")

    lines.append("")

    # Simple joint logistic on new features only (to see if the family has juice together)
    # Reuse the matrix + train logic from the other script if possible, otherwise a tiny local version.
    try:
        from postfill_reversal_model import matrix as _old_matrix, train_weighted_logistic, predict, sigmoid
        # Build a tiny matrix using only the new features for a "new family only" model
        def new_matrix(fills: list[dict[str, Any]]):
            names = ALL_NEW_FEATURES[:]
            rows = []
            y = []
            for f in fills:
                row = [float(f.get(k) or 0.0) for k in names]
                rows.append(row)
                y.append(1.0 if f["target"] else 0.0)
            return np.asarray(rows, dtype=np.float64), np.asarray(y, dtype=np.float64), names

        x_tr, y_tr, names = new_matrix(train_fills)
        x_te, y_te, _ = new_matrix(test_fills)
        if len(x_tr) > 50 and len(x_te) > 20 and y_tr.mean() > 0.01:
            mean = x_tr.mean(0); std = x_tr.std(0); std[std < 1e-8] = 1.0
            x_tr_z = (x_tr - mean) / std
            x_te_z = (x_te - mean) / std
            w = train_weighted_logistic(x_tr_z, y_tr, 2000, 0.03, 0.02)
            p_te = predict(w, x_te_z)
            new_auc = auc(y_te, p_te)
            lines.append("## New-features-only logistic (train on pre-window, test on drawdown window)")
            lines.append(f"Test AUC using **only** the Binance flow + spot accel family: **{new_auc:.3f}**")
            lines.append("")
    except Exception as e:
        lines.append(f"(Skipped joint new-family model: {e})")
        lines.append("")

    lines.append("## Interpretation & Next Steps")
    lines.append("")
    lines.append("If any of the new Binance flow or accel features show |SMD| materially larger than the base set (~0.4 was the previous best), or if the new-features-only model reaches AUC > ~0.60 on the exact 30d drawdown window, we have a real, previously untested signal.")
    lines.append("That signal would let us:")
    lines.append("- Size the favourite loads more aggressively on the ones the flow says are stable.")
    lines.append("- Concentrate the convex tail protection on the fragile subset (higher coverage frac only where the signal is bad).")
    lines.append("- Potentially reduce the blanket convex premium on the full history while still protecting the tail.")
    lines.append("")
    lines.append("Run this against the exact jsonl produced by the current convex_scaled / convex_reversal backtest for the cleanest apples-to-apples read.")

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines))
    print(f"Wrote {args.out_md}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
