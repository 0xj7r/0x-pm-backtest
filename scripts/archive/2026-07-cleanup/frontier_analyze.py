#!/usr/bin/env python3
"""Participation frontier analysis for the BTC fade.

Reads per-threshold trades JSONL files produced by frontier_run.sh and emits:
  1. the frontier table (trades/day, P&L, per-trade, hit, daily Sharpe, worst day)
     for spot-only vs perp-led on 5m and 15m
  2. the marginal cohort at each threshold vs the 0.16 champion (5m)
  3. hour-of-day P&L decomposition at 0.10 vs 0.16
"""
import json
import math
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else Path("frontier_out")
THRESHOLDS = ["0.06", "0.08", "0.10", "0.12", "0.14", "0.16", "0.20"]
N_DAYS = 12  # May 7-18 inclusive


def load(horizon, state, thr):
    path = OUT / f"{horizon}_{state}_{thr.replace('.', '')}.trades.jsonl"
    if not path.exists():
        return None
    trades = []
    with open(path) as f:
        for line in f:
            t = json.loads(line)
            ts = t["decision_ts_ns"] / 1e9
            dt = datetime.fromtimestamp(ts, tz=timezone.utc)
            t["date"] = dt.strftime("%Y-%m-%d")
            t["hour"] = dt.hour
            trades.append(t)
    return trades


def stats(trades):
    n = len(trades)
    pnl = sum(t["pnl"] for t in trades)
    hit = sum(1 for t in trades if t["pnl"] > 0) / n if n else 0.0
    daily = defaultdict(float)
    for t in trades:
        daily[t["date"]] += t["pnl"]
    series = [daily.get(d, 0.0) for d in sorted(daily)]
    # fixed 12-day window: days with zero trades count as zero pnl
    while len(series) < N_DAYS:
        series.append(0.0)
    mu = sum(series) / len(series)
    var = sum((x - mu) ** 2 for x in series) / (len(series) - 1)
    sharpe = mu / math.sqrt(var) if var > 0 else float("nan")
    worst = min(daily.values()) if daily else 0.0
    worst_day = min(daily, key=daily.get) if daily else "-"
    return {
        "trades": n,
        "tpd": n / N_DAYS,
        "pnl": pnl,
        "per_trade": pnl / n if n else 0.0,
        "hit": hit * 100,
        "sharpe": sharpe,
        "worst": worst,
        "worst_day": worst_day,
    }


def frontier_table():
    for horizon in ("5m", "15m"):
        print(f"\n## BTC-{horizon} frontier (May 7-18, {N_DAYS} days)")
        hdr = f"{'state':<5} {'thr':>5} {'trades':>6} {'tr/day':>6} {'P&L$':>10} {'$/tr':>7} {'hit%':>5} {'Sharpe':>6} {'worst day$':>11}"
        print(hdr)
        for state in ("spot", "perp"):
            for thr in THRESHOLDS:
                trades = load(horizon, state, thr)
                if trades is None:
                    print(f"{state:<5} {thr:>5}  (missing)")
                    continue
                s = stats(trades)
                print(
                    f"{state:<5} {thr:>5} {s['trades']:>6} {s['tpd']:>6.1f} "
                    f"{s['pnl']:>10.0f} {s['per_trade']:>7.2f} {s['hit']:>5.1f} "
                    f"{s['sharpe']:>6.2f} {s['worst']:>8.0f} {s['worst_day'][5:]}"
                )


def marginal_cohort(horizon="5m"):
    print(f"\n## Marginal cohorts vs thr 0.16 ({horizon}, markets entered at thr X but NOT at 0.16)")
    hdr = f"{'state':<5} {'thr':>5} {'n_marg':>6} {'P&L$':>9} {'$/tr':>7} {'hit%':>5} {'Sharpe':>6} {'worst$':>7}"
    print(hdr)
    for state in ("spot", "perp"):
        base = load(horizon, state, "0.16")
        if base is None:
            continue
        base_keys = {t["open_ts_ns"] for t in base}
        for thr in ("0.06", "0.08", "0.10", "0.12", "0.14"):
            trades = load(horizon, state, thr)
            if trades is None:
                continue
            marg = [t for t in trades if t["open_ts_ns"] not in base_keys]
            if not marg:
                continue
            s = stats(marg)
            print(
                f"{state:<5} {thr:>5} {s['trades']:>6} {s['pnl']:>9.0f} "
                f"{s['per_trade']:>7.2f} {s['hit']:>5.1f} {s['sharpe']:>6.2f} {s['worst']:>7.0f}"
            )


def hour_of_day(horizon="5m"):
    print(f"\n## Hour-of-day (UTC) P&L, {horizon}: thr 0.10 vs 0.16, marginal = 0.10-only markets")
    for state in ("spot", "perp"):
        t10 = load(horizon, state, "0.10")
        t16 = load(horizon, state, "0.16")
        if t10 is None or t16 is None:
            continue
        base_keys = {t["open_ts_ns"] for t in t16}
        marg = [t for t in t10 if t["open_ts_ns"] not in base_keys]
        agg = lambda trades: {
            h: (sum(t["pnl"] for t in trades if t["hour"] == h),
                sum(1 for t in trades if t["hour"] == h))
            for h in range(24)
        }
        a10, a16, am = agg(t10), agg(t16), agg(marg)
        print(f"\n[{state}]  hour | thr0.16 pnl (n) | thr0.10 pnl (n) | marginal pnl (n)")
        for h in range(24):
            p16, n16 = a16[h]
            p10, n10 = a10[h]
            pm, nm = am[h]
            print(
                f"  {h:02d}   | {p16:>8.0f} ({n16:>3}) | {p10:>8.0f} ({n10:>3}) | {pm:>8.0f} ({nm:>3})"
            )


if __name__ == "__main__":
    frontier_table()
    marginal_cohort()
    hour_of_day()
