#!/usr/bin/env python3
"""Debug winner-inference 'unknown' markets: print net legs vs redeem payout."""
import json
from collections import defaultdict

PATH = "/Users/jackreid/go/polymarket-backtest/data/external/whale_2855/activity.jsonl"

mkts = defaultdict(list)
for l in open(PATH):
    r = json.loads(l)
    if r.get("conditionId"):
        mkts[r["conditionId"]].append(r)

n_shown = 0
for cid, fs in mkts.items():
    buys = defaultdict(lambda: [0.0, 0.0])
    sells = defaultdict(lambda: [0.0, 0.0])
    redeem = 0.0
    n_redeem = 0
    outcome_of = {}
    for r in fs:
        a = r.get("asset")
        u = float(r.get("usdcSize", 0))
        if r["type"] == "TRADE":
            outcome_of[a] = r.get("outcome")
            (buys if r["side"] == "BUY" else sells)[a][0] += float(r["size"])
            (buys if r["side"] == "BUY" else sells)[a][1] += u
        elif r["type"] == "REDEEM":
            redeem += u
            n_redeem += 1
    bought = [a for a, b in buys.items() if b[0] > 0.01]
    if len(bought) != 2 or redeem <= 0:
        continue
    a1, a2 = bought
    net1 = buys[a1][0] - sells[a1][0]
    net2 = buys[a2][0] - sells[a2][0]
    tol = 0.02 * max(net1, net2) + 2
    if (abs(redeem - max(net1, net2)) < tol or
            abs(redeem - min(net1, net2)) < tol):
        continue
    cash = redeem + sum(s[1] for s in sells.values()) - sum(b[1] for b in buys.values())
    print(f"{fs[0].get('slug')}: net[{outcome_of.get(a1)}]={net1:.1f} "
          f"net[{outcome_of.get(a2)}]={net2:.1f} redeem={redeem:.1f} "
          f"(n_redeem={n_redeem}) sum_nets={net1+net2:.1f} cash={cash:.1f}")
    n_shown += 1
    if n_shown >= 15:
        break
