#!/usr/bin/env python3
"""Quant-grade signal screen: measure fee-adjusted edge distributions before backtest.

Implements the SSOT formulae from docs/research/strategy-hunt/05-quant-signals.md.
Does not replace pm-app alpha (no fill simulation); answers: "is there signal mass?"

Usage:
  python3 scripts/quant_signal_screen.py --asset btc --horizon 5m \\
      --date-start 2026-05-07 --date-end 2026-05-18
"""

from __future__ import annotations

import argparse
import json
import math
import os
from collections import defaultdict
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq
import zstandard

ROOT = Path(__file__).resolve().parents[1]
TICKS = ROOT / "data/cache/ticks"
SPOT_DIR = ROOT / "data/cache/raw/binance/exchange=binance/channel=agg_trades"
MANIFEST_DIR = ROOT / "data/manifests/canonical"

TICK_DT = np.dtype([
    ("ts_ns", "<i8"), ("yes_bid", "<f4"), ("yes_ask", "<f4"),
    ("bids", "<f4", (10,)), ("asks", "<f4", (10,)),
    ("no_bid", "<f4"), ("no_ask", "<f4"),
    ("no_bids", "<f4", (10,)), ("no_asks", "<f4", (10,)),
])

FEE_RATE = 0.07
VOL_LOOKBACK_S = 3600
SAMPLE_DT_S = 1
EDGE_THRESHOLDS = (0.08, 0.12, 0.16)


def phi(x: float) -> float:
    return 0.5 * (1.0 + math.erf(x / math.sqrt(2.0)))


def realized_vol_bps(prices: np.ndarray, bar_secs: float) -> float | None:
    if len(prices) < 30:
        return None
    r = np.diff(np.log(prices))
    r = r[np.isfinite(r)]
    if len(r) < 20:
        return None
    step_std = float(np.std(r))
    bar_std = step_std * math.sqrt(bar_secs / SAMPLE_DT_S)
    return bar_std * 10_000.0


def bsm_p_up(spot: float, strike: float, tau: float, sigma_bar_bps: float) -> float | None:
    if spot <= 0 or strike <= 0 or tau <= 0 or sigma_bar_bps <= 0:
        return None
    sigma_bar = sigma_bar_bps / 10_000.0
    sigma_rem = sigma_bar * math.sqrt(tau)
    if sigma_rem < 1e-12:
        return 1.0 if spot > strike else 0.0
    d = math.log(spot / strike) / sigma_rem
    return phi(d)


def fee_per_share(price: float) -> float:
    return FEE_RATE * price * (1.0 - price)


def net_edge(p_hat: float, ask: float) -> float:
    return p_hat - ask - fee_per_share(ask)


def load_ticks(date: str, asset_id: str) -> np.ndarray | None:
    for suffix in ("2s", "1s"):
        path = TICKS / date / f"{asset_id}.{suffix}.btc"
        if path.exists():
            break
    else:
        return None
    raw = path.read_bytes()
    if raw[:4] != b"PTC2":
        return None
    body = zstandard.ZstdDecompressor().decompress(raw[4:], max_output_size=600_000_000)
    n = int.from_bytes(body[:8], "little")
    return np.frombuffer(body, dtype=TICK_DT, count=n, offset=8)


def load_spot_series(symbol: str, date: str) -> tuple[np.ndarray, np.ndarray] | None:
    d = SPOT_DIR / f"symbol={symbol}/date={date}"
    if not d.exists():
        return None
    files = list(d.glob("*.parquet"))
    if not files:
        return None
    t = pq.ParquetFile(files[0]).read(columns=["price", "transact_time_ms"])
    us = t.column("transact_time_ms").to_numpy()
    px = np.asarray(t.column("price").to_pylist(), dtype=float)
    return us, px


def spot_at(spot_us: np.ndarray, spot_px: np.ndarray, ts_ns: int) -> float | None:
    if len(spot_us) == 0:
        return None
    target_us = ts_ns // 1000
    idx = np.searchsorted(spot_us, target_us, side="right") - 1
    if idx < 0:
        return None
    p = spot_px[idx]
    return float(p) if p > 0 else None


def trailing_vol(spot_us: np.ndarray, spot_px: np.ndarray, ts_ns: int, bar_secs: float) -> float | None:
    end_us = ts_ns // 1000
    start_us = end_us - VOL_LOOKBACK_S * 1_000_000
    i0 = int(np.searchsorted(spot_us, start_us, side="left"))
    i1 = int(np.searchsorted(spot_us, end_us, side="right"))
    if i1 - i0 < 30:
        return None
    sub_us = spot_us[i0:i1: max(1, (i1 - i0) // 500)]
    sub_px = spot_px[i0:i1: max(1, (i1 - i0) // 500)]
    prices = []
    for t_us in sub_us:
        idx = np.searchsorted(spot_us, t_us, side="right") - 1
        if idx >= 0 and spot_px[idx] > 0:
            prices.append(spot_px[idx])
    if len(prices) < 30:
        return None
    return realized_vol_bps(np.array(prices, dtype=float), bar_secs)


def parse_open_ts(slug: str, close_ts: int, horizon: str) -> int:
    parts = slug.rsplit("-", 1)
    if len(parts) == 2 and parts[1].isdigit():
        return int(parts[1])
    bar = 300 if horizon == "5m" else 900
    return close_ts - bar


def market_duration_secs(horizon: str) -> int:
    return 300 if horizon == "5m" else 900


def iter_manifest(asset: str, horizon: str):
    path = MANIFEST_DIR / f"{asset}-updown-{horizon}_up.jsonl"
    with open(path) as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--asset", default="btc")
    ap.add_argument("--horizon", default="5m")
    ap.add_argument("--date-start", required=True)
    ap.add_argument("--date-end", required=True)
    ap.add_argument("--symbol", default=None)
    ap.add_argument("--max-markets", type=int, default=200)
    args = ap.parse_args()

    symbol = args.symbol or {"btc": "BTCUSDT", "eth": "ETHUSDT", "sol": "SOLUSDT"}.get(args.asset, "BTCUSDT")
    bar_secs = 300.0 if args.horizon == "5m" else 900.0

    edges_yes: list[float] = []
    edges_no: list[float] = []
    net_edges: list[float] = []
    by_thresh = defaultdict(int)
    n_decisions = 0
    n_markets = 0

    for m in iter_manifest(args.asset, args.horizon):
        if n_markets >= args.max_markets:
            break
        date = m.get("date") or m.get("market_date")
        if not date or date < args.date_start or date > args.date_end:
            continue
        asset_id = m.get("asset_id")
        close_ts = int(m.get("close_ts") or 0)
        slug = m.get("slug") or ""
        open_ts = parse_open_ts(slug, close_ts, args.horizon)
        open_ns = open_ts * 1_000_000_000
        close_ns = (close_ts if close_ts > open_ts else open_ts + market_duration_secs(args.horizon)) * 1_000_000_000
        if not asset_id or not slug:
            continue

        ticks = load_ticks(date, asset_id)
        spot_data = load_spot_series(symbol, date)
        if ticks is None or spot_data is None:
            continue
        spot_us, spot_px = spot_data
        strike = spot_at(spot_us, spot_px, open_ns)
        if strike is None or strike <= 0:
            continue

        # sample every ~5s in tradeable zone
        t0 = open_ns + 30_000_000_000
        t1 = close_ns - 90_000_000_000
        step = 5_000_000_000
        n_markets += 1

        for ts_ns in range(t0, t1, step):
            spot = spot_at(spot_us, spot_px, ts_ns)
            sigma_bps = trailing_vol(spot_us, spot_px, ts_ns, bar_secs)
            if spot is None or sigma_bps is None:
                continue
            tau = (close_ns - ts_ns) / (close_ns - open_ns)
            p_up = bsm_p_up(spot, strike, tau, sigma_bps)
            if p_up is None:
                continue

            idx = np.searchsorted(ticks["ts_ns"], ts_ns, side="right") - 1
            if idx < 0:
                continue
            row = ticks[idx]
            yes_ask = float(row["yes_ask"])
            no_ask = float(row["no_ask"])
            if yes_ask <= 0 or no_ask <= 0:
                continue

            e_yes = p_up - yes_ask
            e_no = (1.0 - p_up) - no_ask
            if e_yes >= e_no:
                side_edge = e_yes
                net_e = net_edge(p_up, yes_ask)
            else:
                side_edge = e_no
                net_e = net_edge(1.0 - p_up, no_ask)

            n_decisions += 1
            edges_yes.append(e_yes)
            edges_no.append(e_no)
            net_edges.append(net_e)
            for th in EDGE_THRESHOLDS:
                if side_edge >= th and net_e > 0:
                    by_thresh[th] += 1

    def tstat(xs: list[float]) -> float:
        if len(xs) < 2:
            return 0.0
        a = np.array(xs)
        return float(a.mean() / (a.std(ddof=1) / math.sqrt(len(a))))

    print(f"# Quant signal screen — {args.asset}-{args.horizon} {args.date_start}..{args.date_end}")
    print(f"markets={n_markets} decisions={n_decisions} symbol={symbol}")
    print()
    print("| metric | value |")
    print("|---|---:|")
    if n_decisions == 0:
        print("ERROR: no decisions — check manifest dates and cache coverage")
        return
    print(f"| mean ε_yes | {np.mean(edges_yes):+.4f} |")
    print(f"| mean ε_no | {np.mean(edges_no):+.4f} |")
    print(f"| mean ε_net (best side) | {np.mean(net_edges):+.4f} |")
    print(f"| t-stat ε_net | {tstat(net_edges):+.2f} |")
    print(f"| frac ε_net > 0 | {np.mean(np.array(net_edges) > 0):.1%} |")
    for th in EDGE_THRESHOLDS:
        rate = by_thresh[th] / max(1, n_decisions)
        print(f"| events ε≥{th:.2f} & net>0 | {by_thresh[th]} ({rate:.2%}) |")
    print()
    if tstat(net_edges) > 2.0:
        print("PASS: signal t-stat > 2 — proceed to alpha backtest (C1 class)")
    else:
        print("FAIL: insufficient fee-adjusted edge mass — do not tune thresholds")


if __name__ == "__main__":
    main()