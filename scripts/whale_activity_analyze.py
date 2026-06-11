#!/usr/bin/env python3
"""Pull a wallet's recent Polymarket /activity and decompose per-market
behavior: same-side scalps, both-side pair assembly, cheap-tail buys.

Usage: python3 scripts/whale_activity_analyze.py <address> <hours>
"""
import json
import sys
import time
import urllib.parse
import urllib.request

BASE = "https://data-api.polymarket.com/activity"


def fetch(addr, start, end):
    rows, offset = [], 0
    while offset <= 10000:
        q = urllib.parse.urlencode({
            "user": addr, "limit": 500, "offset": offset,
            "start": start, "end": end, "type": "TRADE",
        })
        req = urllib.request.Request(f"{BASE}?{q}",
                                     headers={"User-Agent": "pm-research/1.0"})
        with urllib.request.urlopen(req, timeout=30) as r:
            page = json.load(r)
        if not isinstance(page, list) or not page:
            break
        rows.extend(page)
        if len(page) < 500:
            break
        offset += 500
        time.sleep(0.15)
    return rows


def main():
    addr, hours = sys.argv[1], float(sys.argv[2])
    end = int(time.time())
    start = end - int(hours * 3600)
    fills = []
    # 30-min windows to stay under offset ceilings
    cur = start
    while cur < end:
        fills.extend(fetch(addr, cur, min(cur + 1800, end)))
        cur += 1800
    print(f"{len(fills)} fills over {hours}h")
    if not fills:
        return

    by_mkt = {}
    for f in fills:
        key = f.get("slug") or f.get("conditionId")
        by_mkt.setdefault(key, []).append(f)
    print(f"{len(by_mkt)} distinct markets")

    n_scalp = n_pair = n_tail = 0
    pair_costs, scalp_edges, holds = [], [], []
    families = {}
    for slug, fs in by_mkt.items():
        fam = "-".join(slug.split("-")[:3]) if slug else "?"
        families[fam] = families.get(fam, 0) + 1
        fs.sort(key=lambda x: x.get("timestamp", 0))
        buys = {}   # outcome -> [qty, cost]
        sells = {}
        first_buy_ts = {}
        for f in fs:
            o = f.get("outcome", "?")
            qty = float(f.get("size", 0))
            usd = float(f.get("usdcSize", 0))
            if f.get("side") == "BUY":
                b = buys.setdefault(o, [0.0, 0.0])
                b[0] += qty; b[1] += usd
                first_buy_ts.setdefault(o, f.get("timestamp", 0))
                if usd > 0 and usd / max(qty, 1e-9) <= 0.15:
                    n_tail += 1
            else:
                s = sells.setdefault(o, [0.0, 0.0])
                s[0] += qty; s[1] += usd
                if o in first_buy_ts:
                    holds.append(f.get("timestamp", 0) - first_buy_ts[o])
        # scalp: sold some of what was bought, same outcome
        for o, (sq, susd) in sells.items():
            if o in buys and buys[o][0] > 0 and sq > 0:
                n_scalp += 1
                b_px = buys[o][1] / buys[o][0]
                s_px = susd / sq
                scalp_edges.append(s_px - b_px)
        # pair: bought BOTH outcomes
        outs = [o for o, b in buys.items() if b[0] > 0]
        if len(outs) >= 2:
            n_pair += 1
            # paired quantity = min leg shares; pair cost = sum of leg VWAPs
            vwaps = sorted(b[1] / b[0] for b in buys.values() if b[0] > 0)
            pair_costs.append(sum(vwaps[:2]))

    import statistics as st
    print(f"\nmarket families: {dict(sorted(families.items(), key=lambda kv: -kv[1])[:8])}")
    print(f"markets with same-side round trip (scalp): {n_scalp}")
    if scalp_edges:
        print(f"  scalp edge per share: median {st.median(scalp_edges):+.3f} mean {st.mean(scalp_edges):+.3f}")
    print(f"markets where BOTH outcomes were bought (pair assembly): {n_pair}/{len(by_mkt)}")
    if pair_costs:
        print(f"  combined VWAP pair cost: median {st.median(pair_costs):.3f} "
              f"p25 {sorted(pair_costs)[len(pair_costs)//4]:.3f} "
              f"share<1.00: {sum(1 for c in pair_costs if c < 1.0)/len(pair_costs):.0%}")
    print(f"cheap-tail buys (<=0.15): {n_tail}")
    if holds:
        print(f"buy->sell hold (same outcome): median {st.median(holds):.0f}s")


if __name__ == "__main__":
    main()
