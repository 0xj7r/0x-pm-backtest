#!/usr/bin/env python3
"""Score one ETH-complete cell from its trades.jsonl.

The sigma>=3 floor is --min-entry-sigma-bps 3.0 (prunes sub-survival low-vol
entries). Applied offline here by keeping trades with sigma_bar_bps >= 3.0.

Reports NET, $/day, hit, Sharpe (daily), e/day for raw and sigma>=3 subsets.
"""
import json
import sys
import math
from collections import defaultdict
from datetime import datetime, timezone


def load(path):
    rows = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def day_of(ns):
    return datetime.fromtimestamp(ns / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")


def score(rows, label):
    if not rows:
        print(f"{label}: NO TRADES")
        return None
    per_day_pnl = defaultdict(float)
    per_day_n = defaultdict(int)
    n_win = 0
    net = 0.0
    for r in rows:
        d = day_of(r["decision_ts_ns"])
        per_day_pnl[d] += r["pnl"]
        per_day_n[d] += 1
        net += r["pnl"]
        if r.get("won"):
            n_win += 1
    days = sorted(per_day_pnl)
    n_days = len(days)
    daily = [per_day_pnl[d] for d in days]
    mean_d = sum(daily) / n_days
    sd_d = math.sqrt(sum((x - mean_d) ** 2 for x in daily) / n_days) if n_days > 1 else 0.0
    sharpe = (mean_d / sd_d) if sd_d > 0 else float("nan")
    n = len(rows)
    hit = n_win / n
    e_per_trade = net / n
    e_per_day = net / n_days
    green = sum(1 for x in daily if x > 0)
    print(f"{label}:")
    print(f"  trades={n} days={n_days} NET=${net:,.0f}")
    print(f"  $/day=${e_per_day:,.0f}  hit={hit*100:.1f}%  e/trade=${e_per_trade:.3f}")
    print(f"  daily mean=${mean_d:,.0f} sd=${sd_d:,.0f} Sharpe={sharpe:.2f}  green={green}/{n_days}")
    return dict(net=net, n=n, days=n_days, hit=hit, e_day=e_per_day, sharpe=sharpe, e_trade=e_per_trade, green=green)


def main():
    path = sys.argv[1]
    sigma_floor = float(sys.argv[2]) if len(sys.argv) > 2 else 3.0
    rows = load(path)
    print(f"== {path} ==")
    score(rows, "RAW")
    # sigma>=3 floor: keep trades where sigma_bar_bps >= sigma_floor
    flt = [r for r in rows if r.get("sigma_bar_bps", 0.0) >= sigma_floor]
    score(flt, f"SIGMA>={sigma_floor:g}bps")


if __name__ == "__main__":
    main()
