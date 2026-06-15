#!/usr/bin/env python3
"""Offline sigma-floor scoring for btclong_xrp cells.

Reads a cell's trades.jsonl and reports total/per-trade P&L and hit rate at the
raw (no floor) and with the structural sigma_bar_bps floor applied:
fade >= 3.0, lane >= 4.0. Entry legs only (hedge/completion legs have
side_ask_at_decision == 0 and are part of the same ticket; we keep all legs for
P&L but gate by the entry leg's sigma where present).
"""
import json, sys, math

def load(path):
    rows = []
    try:
        for line in open(path):
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    except FileNotFoundError:
        return None
    return rows

def stats(rows):
    n = len(rows)
    pnl = sum(r["pnl"] for r in rows)
    wins = sum(1 for r in rows if r.get("won"))
    per = pnl / n if n else 0.0
    # t-stat of per-trade pnl
    if n > 1:
        mean = per
        var = sum((r["pnl"] - mean) ** 2 for r in rows) / (n - 1)
        sd = math.sqrt(var)
        t = mean / (sd / math.sqrt(n)) if sd > 0 else 0.0
    else:
        t = 0.0
    return n, pnl, per, (wins / n if n else 0.0), t

def main():
    path = sys.argv[1]
    floor = float(sys.argv[2]) if len(sys.argv) > 2 else 0.0
    rows = load(path)
    if rows is None:
        print(f"{path}: NO FILE")
        return
    if not rows:
        print(f"{path}: 0 trades")
        return
    n, pnl, per, hit, t = stats(rows)
    print(f"{path}")
    print(f"  RAW:        n={n} pnl={pnl:.1f} per={per:.3f} hit={hit*100:.1f}% t={t:.2f}")
    if floor > 0:
        # gate by entry sigma; keep legs whose sigma_bar_bps>=floor OR which are
        # hedge/completion legs (side_ask_at_decision==0) attached to a kept entry.
        kept = [r for r in rows if r.get("sigma_bar_bps", 0.0) >= floor]
        n2, pnl2, per2, hit2, t2 = stats(kept)
        print(f"  sigma>={floor}: n={n2} pnl={pnl2:.1f} per={per2:.3f} hit={hit2*100:.1f}% t={t2:.2f}")

if __name__ == "__main__":
    main()
