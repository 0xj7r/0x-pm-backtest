#!/usr/bin/env python3
"""Regime-conditional decomposition: which config holds up on the quietest days.

Classifies each day by realized vol (Binance BTCUSDT spot, 1-minute log returns)
and decomposes every available trades file into quiet / mid / loud tercile P&L.

Primary section: tune window May 7-18 (selection-safe).
Supplement: May 19-28 test-window runs (perp/rearm/blend) -- comparison only,
flagged as test data; no selection decisions should be made from it alone.
"""

import glob
import json
import math
import os
from collections import defaultdict
from datetime import datetime, timezone

import numpy as np
import pandas as pd

REPO = "/Users/jackreid/go/polymarket-backtest"
BINANCE = f"{REPO}/data/cache/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT"
ALPHA = f"{REPO}/data/runs/alpha"

TUNE_DAYS = [f"2026-05-{d:02d}" for d in range(7, 19)]
TEST_DAYS = [f"2026-05-{d:02d}" for d in range(19, 29)]

TUNE_FILES = {
    "champ(ex30)": f"{ALPHA}/exitsweep/ex30.trades.jsonl",
    "ex0_hold": f"{ALPHA}/exitsweep/ex0.trades.jsonl",
    "ex10": f"{ALPHA}/exitsweep/ex10.trades.jsonl",
    "ex20": f"{ALPHA}/exitsweep/ex20.trades.jsonl",
    "ex45": f"{ALPHA}/exitsweep/ex45.trades.jsonl",
    "ex60": f"{ALPHA}/exitsweep/ex60.trades.jsonl",
    "ex90": f"{ALPHA}/exitsweep/ex90.trades.jsonl",
    "ex120": f"{ALPHA}/exitsweep/ex120.trades.jsonl",
    "sbc10_koff": f"{ALPHA}/sizing/sbc10_koff.trades.jsonl",
    "sbc10_kon": f"{ALPHA}/sizing/sbc10_kon.trades.jsonl",
    "sbc90_koff": f"{ALPHA}/sizing/sbc90_koff.trades.jsonl",
    "sbc90_kon": f"{ALPHA}/sizing/sbc90_kon.trades.jsonl",
    "whale_champ_c4": f"{ALPHA}/whaleclone/champ_c4.trades.jsonl",
    "whale_hold_c2": f"{ALPHA}/whaleclone/hold_c2.trades.jsonl",
    "whale_hold_c4": f"{ALPHA}/whaleclone/hold_c4.trades.jsonl",
    "whale_hold_c8": f"{ALPHA}/whaleclone/hold_c8.trades.jsonl",
    "calm_e0_c1": f"{ALPHA}/calmgrind/calm_e0_c1.trades.jsonl",
    "calm_e0_c4": f"{ALPHA}/calmgrind/calm_e0_c4.trades.jsonl",
    "calm_e30_c1": f"{ALPHA}/calmgrind/calm_e30_c1.trades.jsonl",
    "calm_e30_c4": f"{ALPHA}/calmgrind/calm_e30_c4.trades.jsonl",
    "fillfloor_0.05": f"{ALPHA}/fillcap/floor_0.05.trades.jsonl",
    "fillfloor_0.08": f"{ALPHA}/fillcap/floor_0.08.trades.jsonl",
    "fillfloor_0.16": f"{ALPHA}/fillcap/floor_0.16.trades.jsonl",
    "btc15_fade": f"{ALPHA}/overnight2/X_btc15_fade.trades.jsonl",
    "combo_tune": "/tmp/combo_tune.trades.jsonl",
}

TEST_FILES = {
    "champ(ex30_test)": f"{ALPHA}/exitsweep/ex30_test.trades.jsonl",
    "ex90_test": f"{ALPHA}/exitsweep/ex90_test.trades.jsonl",
    "perp@0.16": "/tmp/perp_test.trades.jsonl",
    "perp@0.12": "/tmp/perp12_test.trades.jsonl",
    "rearm": "/tmp/rearm_test.trades.jsonl",
    "blend": "/tmp/blend_test.trades.jsonl",
    "btc15_fade_test": f"{ALPHA}/overnight2/X_btc15_fade_test.trades.jsonl",
}


def daily_rv(day: str) -> float:
    """Annualization-free daily realized vol: std of 1-min log returns, in bp."""
    files = glob.glob(f"{BINANCE}/date={day}/*.parquet")
    if not files:
        return float("nan")
    df = pd.read_parquet(files[0], columns=["price", "transact_time_ms"])
    df["price"] = df["price"].astype(float)
    # field named _ms but holds microseconds in this cache
    df["minute"] = df["transact_time_ms"].astype("int64") // 60_000_000
    px = df.groupby("minute")["price"].last()
    rets = np.diff(np.log(px.values))
    return float(np.std(rets) * 1e4)


def load_daily_pnl(path: str):
    by_day = defaultdict(lambda: {"pnl": 0.0, "n": 0, "wins": 0})
    regime_pnl = defaultdict(float)
    with open(path) as fh:
        for line in fh:
            r = json.loads(line)
            day = datetime.fromtimestamp(r["open_ts_ns"] / 1e9, timezone.utc).strftime("%Y-%m-%d")
            by_day[day]["pnl"] += r["pnl"]
            by_day[day]["n"] += 1
            by_day[day]["wins"] += bool(r.get("won"))
            regime_pnl[r.get("regime", "?")] += r["pnl"]
    return by_day, regime_pnl


def tercile_map(days, rv):
    ranked = sorted(days, key=lambda d: rv[d])
    k = len(ranked) // 3
    out = {}
    for i, d in enumerate(ranked):
        out[d] = "quiet" if i < k + (len(ranked) % 3 > 0) else ("mid" if i < 2 * k + (len(ranked) % 3 > 0) else "loud")
    # simpler: equal-ish split
    out = {}
    n = len(ranked)
    for i, d in enumerate(ranked):
        out[d] = "quiet" if i < math.ceil(n / 3) else ("mid" if i < math.ceil(2 * n / 3) else "loud")
    return out


def report(title, files, days, rv, terc):
    print(f"\n=== {title} ===")
    print("day RV (bp/min, quiet->loud):", ", ".join(f"{d[5:]}={rv[d]:.1f}[{terc[d][0]}]" for d in sorted(days, key=lambda d: rv[d])))
    rows = []
    detail = {}
    for name, path in files.items():
        if not os.path.exists(path):
            print(f"  MISSING {name}: {path}")
            continue
        by_day, regime_pnl = load_daily_pnl(path)
        agg = {"quiet": [], "mid": [], "loud": []}
        for d in days:
            agg[terc[d]].append(by_day.get(d, {"pnl": 0.0, "n": 0})["pnl"])
        q, m, l = (sum(agg[k]) for k in ("quiet", "mid", "loud"))
        nq = {k: sum(1 for d in days if terc[d] == k) for k in ("quiet", "mid", "loud")}
        n_quiet_tr = sum(by_day.get(d, {"n": 0})["n"] for d in days if terc[d] == "quiet")
        qdays = [by_day.get(d, {"pnl": 0.0})["pnl"] for d in days if terc[d] == "quiet"]
        rows.append({
            "config": name, "total": q + m + l,
            "quiet_total": q, "quiet_perday": q / nq["quiet"],
            "quiet_green": sum(p > 0 for p in qdays), "quiet_days": nq["quiet"],
            "quiet_worst": min(qdays) if qdays else 0.0,
            "quiet_trades": n_quiet_tr,
            "mid_perday": m / nq["mid"], "loud_perday": l / nq["loud"],
            "quiet_share": (q / (q + m + l)) if (q + m + l) else float("nan"),
        })
        detail[name] = by_day
    rows.sort(key=lambda r: -r["quiet_perday"])
    hdr = f"{'config':18s} {'total$':>9s} {'quiet$':>8s} {'q$/day':>8s} {'qgreen':>6s} {'qworst':>8s} {'qtrades':>7s} {'mid$/d':>8s} {'loud$/d':>9s} {'q_share':>7s}"
    print(hdr)
    for r in rows:
        print(f"{r['config']:18s} {r['total']:9.0f} {r['quiet_total']:8.0f} {r['quiet_perday']:8.0f} "
              f"{r['quiet_green']}/{r['quiet_days']:<4d} {r['quiet_worst']:8.0f} {r['quiet_trades']:7d} "
              f"{r['mid_perday']:8.0f} {r['loud_perday']:9.0f} {r['quiet_share']:7.2f}")
    return detail


def main():
    rv = {}
    for d in TUNE_DAYS + TEST_DAYS:
        rv[d] = daily_rv(d)
    tune_terc = tercile_map(TUNE_DAYS, rv)
    test_terc = tercile_map(TEST_DAYS, rv)

    detail = report("TUNE WINDOW May 7-18 (selection-safe)", TUNE_FILES, TUNE_DAYS, rv, tune_terc)

    # exit-horizon vs quietness: per-tercile P&L for the exit sweep
    print("\n--- exit horizon x tercile (tune, total $) ---")
    print(f"{'exit':12s} {'quiet':>8s} {'mid':>8s} {'loud':>9s}")
    for name in ["ex0_hold", "ex10", "ex20", "champ(ex30)", "ex45", "ex60", "ex90", "ex120"]:
        if name not in detail:
            continue
        by_day = detail[name]
        t = {"quiet": 0.0, "mid": 0.0, "loud": 0.0}
        for d in TUNE_DAYS:
            t[tune_terc[d]] += by_day.get(d, {"pnl": 0.0})["pnl"]
        print(f"{name:12s} {t['quiet']:8.0f} {t['mid']:8.0f} {t['loud']:9.0f}")

    report("TEST WINDOW May 19-28 (perp/rearm comparison -- test data, no further selection)",
           TEST_FILES, TEST_DAYS, rv, test_terc)

    # quiet-day per-day table for the headline test configs
    print("\n--- test-window per-day P&L (sorted quiet->loud) ---")
    cols = ["champ(ex30_test)", "perp@0.16", "perp@0.12", "rearm", "blend"]
    daily = {c: load_daily_pnl(TEST_FILES[c])[0] for c in cols if os.path.exists(TEST_FILES[c])}
    print(f"{'day':6s} {'RV':>6s} {'terc':5s} " + " ".join(f"{c:>16s}" for c in daily))
    for d in sorted(TEST_DAYS, key=lambda d: rv[d]):
        print(f"{d[5:]:6s} {rv[d]:6.1f} {test_terc[d]:5s} " + " ".join(f"{daily[c].get(d, {'pnl': 0.0})['pnl']:16.0f}" for c in daily))


if __name__ == "__main__":
    main()
