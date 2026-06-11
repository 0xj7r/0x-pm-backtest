#!/usr/bin/env python3
"""Decompose a whale's updown P&L by entry price band and time-to-close.

Winner inference per market: outcome whose bought qty best matches total
REDEEM usdc (winning shares pay $1). Markets with no REDEEM and a single
bought side are assumed lost (no payout). Ambiguous markets are skipped.

Per BUY fill: pnl = qty*1{winner} - usdc. Aggregated over
(price band x seconds-to-close bucket), plus leg-skew accuracy.

Usage: python3 scripts/whale_edge_decompose.py <address>
"""
import json
import sys
from collections import defaultdict

from whale_behavior_report import market_meta

BANDS = [(0, .15), (.15, .30), (.30, .50), (.50, .70), (.70, .85), (.85, 1.01)]
TBUCKETS = [(0, 30), (30, 60), (60, 120), (120, 240), (240, 600), (600, 10**9)]


def main():
    addr = sys.argv[1].lower()
    seen = set()
    by_mkt = defaultdict(lambda: {"fills": [], "redeem": 0.0})
    for line in open(f"data/runs/whales/{addr}.jsonl"):
        r = json.loads(line)
        key = (r.get("transactionHash"), r.get("type"), r.get("asset"),
               r.get("side"), r.get("size"), r.get("price"), r.get("timestamp"))
        if key in seen:
            continue
        seen.add(key)
        slug = r.get("slug", "") or ""
        meta = market_meta(slug)
        if not meta:
            continue
        m = by_mkt[slug]
        if r["type"] == "TRADE" and r["side"] == "BUY":
            m["fills"].append(r)
        elif r["type"] == "REDEEM":
            m["redeem"] += float(r.get("usdcSize", 0))

    cell = defaultdict(lambda: [0.0, 0.0, 0])  # (band,tb) -> [pnl, cost, n]
    skew_right = skew_total = 0
    n_resolved = n_skipped = 0
    bigger_leg_first = 0
    for slug, m in by_mkt.items():
        meta = market_meta(slug)
        _, hz, hz_s, close = meta
        legs = defaultdict(lambda: [0.0, 0.0])  # outcome -> [qty, usd]
        for f in m["fills"]:
            o = f.get("outcome")
            legs[o][0] += float(f.get("size", 0))
            legs[o][1] += float(f.get("usdcSize", 0))
        outs = list(legs)
        if not outs:
            continue
        winner = None
        if m["redeem"] > 0:
            cand = min(outs, key=lambda o: abs(legs[o][0] - m["redeem"]))
            if abs(legs[cand][0] - m["redeem"]) <= 0.02 * max(m["redeem"], 1):
                winner = cand
        elif len(outs) == 1:
            winner = "Up" if outs[0] == "Down" else "Down"  # sole side lost
        if winner is None:
            n_skipped += 1
            continue
        n_resolved += 1
        if len(outs) >= 2:
            skew_total += 1
            big = max(outs, key=lambda o: legs[o][1])
            if big == winner:
                skew_right += 1
        for f in m["fills"]:
            px = float(f.get("price", 0))
            usd = float(f.get("usdcSize", 0))
            qty = float(f.get("size", 0))
            ttc = max(close - f["timestamp"], 0)
            pnl = qty * (1.0 if f.get("outcome") == winner else 0.0) - usd
            bi = next(i for i, (lo, hi) in enumerate(BANDS) if lo <= px < hi)
            ti = next(i for i, (lo, hi) in enumerate(TBUCKETS)
                      if lo <= ttc < hi)
            c = cell[(bi, ti)]
            c[0] += pnl
            c[1] += usd
            c[2] += 1

    print(f"=== {addr} edge decomposition ===")
    print(f"resolved {n_resolved} updown mkts, skipped {n_skipped} ambiguous")
    if skew_total:
        print(f"bigger-$ leg was the winner: {skew_right}/{skew_total} "
              f"({skew_right/skew_total:.0%}) of both-sides markets")
    hdr = "  ".join(f"{lo:>4}-{min(hi, 9999):<4}s" for lo, hi in TBUCKETS)
    print(f"\nP&L $ (return on cost) by price band x time-to-close")
    print(f"{'band':<12}{hdr}")
    for bi, (lo, hi) in enumerate(BANDS):
        row = []
        for ti in range(len(TBUCKETS)):
            pnl, cost, n = cell.get((bi, ti), [0, 0, 0])
            row.append(f"{pnl:+8.0f} ({pnl/cost:+5.1%})" if cost > 0
                       else f"{'-':>16}")
        print(f"[{lo:.2f},{hi:.2f}) " + " ".join(row))
    tot_p = sum(v[0] for v in cell.values())
    tot_c = sum(v[1] for v in cell.values())
    print(f"\ntotal fill-level pnl ${tot_p:+,.0f} on ${tot_c:,.0f} "
          f"({tot_p/max(tot_c,1):+.2%})  [resolved mkts only]")


if __name__ == "__main__":
    main()
