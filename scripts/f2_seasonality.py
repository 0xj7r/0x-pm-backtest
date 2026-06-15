#!/usr/bin/env python3
"""F2: session seasonality gating study for the BTC-5m fade.

Fits hour-of-day x day-of-week (and hour-only) exclusion gates on W3
(May 7-18 tune window) trades, then applies the frozen gates to W1
(Feb 12 - Mar 31) and W2 (April) and reports out-of-window NET and
daily Sharpe change vs ungated. Read-only over existing trades files.
"""
import json
import math
import datetime as dt
from collections import defaultdict

DIR = "/Users/jackreid/go/polymarket-backtest/data/runs/alpha/feemin"
CUTOFF_NS = int(dt.datetime(2026, 5, 19, tzinfo=dt.timezone.utc).timestamp() * 1e9)
MIN_N = 30

FILES = {
    ("base", "W3"): "base.trades.jsonl",
    ("base", "W1"): "W1_base.trades.jsonl",
    ("base", "W2"): "W2_base.trades.jsonl",
    ("midtimeout", "W3"): "midtimeout.trades.jsonl",
    ("midtimeout", "W1"): "W1_midtimeout.trades.jsonl",
    ("midtimeout", "W2"): "W2_midtimeout.trades.jsonl",
}


def load(variant, window):
    out = []
    with open(f"{DIR}/{FILES[(variant, window)]}") as fh:
        for line in fh:
            d = json.loads(line)
            if d.get("window_secs") != 300:
                continue
            ts = d["decision_ts_ns"]
            assert ts < CUTOFF_NS, "trade past 2026-05-19 cutoff"
            t = dt.datetime.fromtimestamp(ts / 1e9, dt.timezone.utc)
            out.append({
                "pnl": d["pnl"],
                "hour": t.hour,
                "dow": t.weekday(),  # 0=Mon
                "date": t.date(),
            })
    return out


def cells(trades, key):
    agg = defaultdict(lambda: [0, 0.0])
    for tr in trades:
        k = key(tr)
        agg[k][0] += 1
        agg[k][1] += tr["pnl"]
    return agg


def fit_gate(trades, key):
    """Exclude cells with negative total pnl and n >= MIN_N."""
    agg = cells(trades, key)
    return {k for k, (n, p) in agg.items() if n >= MIN_N and p < 0}


def metrics(trades, excluded, key):
    kept = [t for t in trades if key(t) not in excluded]
    removed = [t for t in trades if key(t) in excluded]
    daily = defaultdict(float)
    for t in kept:
        daily[t["date"]] += t["pnl"]
    # days where everything was gated out count as zero-pnl days
    all_days = {t["date"] for t in trades}
    vals = [daily.get(d, 0.0) for d in sorted(all_days)]
    mean = sum(vals) / len(vals)
    var = sum((v - mean) ** 2 for v in vals) / (len(vals) - 1) if len(vals) > 1 else 0.0
    sharpe = mean / math.sqrt(var) if var > 0 else float("nan")
    return {
        "n": len(kept),
        "n_removed": len(removed),
        "net": sum(t["pnl"] for t in kept),
        "removed_pnl": sum(t["pnl"] for t in removed),
        "sharpe": sharpe,
    }


HKEY = lambda t: t["hour"]
HDKEY = lambda t: (t["dow"], t["hour"])
DOW = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]


def main():
    data = {k: load(*k) for k in FILES}

    for variant in ["base", "midtimeout"]:
        w3 = data[(variant, "W3")]
        gate_hd = fit_gate(w3, HDKEY)
        gate_h = fit_gate(w3, HKEY)
        print(f"\n===== variant: {variant} =====")
        print(f"hour x dow gate ({len(gate_hd)} cells excluded): "
              + ", ".join(f"{DOW[d]} {h:02d}h" for d, h in sorted(gate_hd)))
        print(f"hour-only gate ({len(gate_h)} hours excluded): "
              + ", ".join(f"{h:02d}h" for h in sorted(gate_h)))

        # W3 hour table for the doc
        agg = cells(w3, HKEY)
        print("\nW3 hour-of-day table (UTC):")
        print("hour |    n |     pnl | pnl/trade")
        for h in range(24):
            n, p = agg.get(h, (0, 0.0))
            mark = " <EXCL" if h in gate_h else ""
            print(f"  {h:02d} | {n:4d} | {p:8.1f} | {p/n if n else 0:8.2f}{mark}")

        for gname, gate, key in [("hour x dow", gate_hd, HDKEY), ("hour-only", gate_h, HKEY)]:
            print(f"\n--- gate: {gname} ---")
            print(f"{'win':4} | {'n_kept':6} | {'n_rm':5} | {'ungated NET':>11} | {'gated NET':>10} | "
                  f"{'delta':>8} | {'rm_pnl':>8} | {'Sh ung':>7} | {'Sh gat':>7}")
            rows = {}
            for w in ["W3", "W1", "W2"]:
                trades = data[(variant, w)]
                ung = metrics(trades, set(), key)
                gat = metrics(trades, gate, key)
                delta = gat["net"] - ung["net"]
                rows[w] = delta
                print(f"{w:4} | {gat['n']:6d} | {gat['n_removed']:5d} | {ung['net']:11.1f} | "
                      f"{gat['net']:10.1f} | {delta:8.1f} | {gat['removed_pnl']:8.1f} | "
                      f"{ung['sharpe']:7.3f} | {gat['sharpe']:7.3f}")
            if rows["W3"] != 0:
                print(f"survival: W1 delta / W3 delta = {rows['W1']/rows['W3']:.2f}, "
                      f"W2 delta / W3 delta = {rows['W2']/rows['W3']:.2f}")


if __name__ == "__main__":
    main()
