#!/usr/bin/env python3
"""Per-wallet behavioral report from cached /activity pulls
(data/runs/whales/<addr>.jsonl, produced by whale_pull.py).

Reports: family mix, event mix, entry price bands, per-market fill/ladder
stats, both-sides pairing, hold times, hourly pattern, and a cash-flow
P&L proxy per resolved market (buys vs sells+redeems+merges).

Usage: python3 scripts/whale_behavior_report.py <address>
"""
import json
import re
import statistics as st
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from zoneinfo import ZoneInfo

ET = ZoneInfo("America/New_York")
UPDOWN_NUM = re.compile(r"^([a-z]+)-updown-(\d+)m-(\d+)$")
UPDOWN_HOURLY = re.compile(
    r"^([a-z]+)-up-or-down-([a-z]+)-(\d+)-(\d{4})-(\d+)(am|pm)-et$")
MONTHS = {m: i + 1 for i, m in enumerate(
    ["january", "february", "march", "april", "may", "june", "july",
     "august", "september", "october", "november", "december"])}
ASSET_ALIAS = {"bitcoin": "btc", "ethereum": "eth", "solana": "sol",
               "xrp": "xrp", "btc": "btc", "eth": "eth", "sol": "sol"}


def market_meta(slug):
    """-> (asset, horizon_label, horizon_secs, close_unix) or None."""
    m = UPDOWN_NUM.match(slug)
    if m:
        asset, mins, start = m.group(1), int(m.group(2)), int(m.group(3))
        return asset, f"{mins}m", mins * 60, start + mins * 60
    m = UPDOWN_HOURLY.match(slug)
    if m:
        asset = ASSET_ALIAS.get(m.group(1), m.group(1))
        mon, day, year = MONTHS[m.group(2)], int(m.group(3)), int(m.group(4))
        hr = int(m.group(5)) % 12 + (12 if m.group(6) == "pm" else 0)
        open_dt = datetime(year, mon, day, hr, tzinfo=ET)
        return asset, "1h", 3600, int(open_dt.timestamp()) + 3600
    return None


def pct(xs, q):
    return sorted(xs)[int(q * (len(xs) - 1))] if xs else float("nan")


def main():
    addr = sys.argv[1].lower()
    seen, rows = set(), []
    for line in open(f"data/runs/whales/{addr}.jsonl"):
        r = json.loads(line)
        key = (r.get("transactionHash"), r.get("type"), r.get("asset"),
               r.get("side"), r.get("size"), r.get("price"),
               r.get("timestamp"))
        if key in seen:
            continue
        seen.add(key)
        rows.append(r)
    if not rows:
        print("no data")
        return
    ts_min = min(r["timestamp"] for r in rows)
    ts_max = max(r["timestamp"] for r in rows)
    span = ts_max - ts_min
    print(f"=== {addr} ===")
    print(f"events {len(rows)}  span {span/86400:.2f}d  "
          f"{datetime.fromtimestamp(ts_min, timezone.utc):%m-%d %H:%M} -> "
          f"{datetime.fromtimestamp(ts_max, timezone.utc):%m-%d %H:%M} UTC")

    # event mix
    mix = Counter(r["type"] for r in rows)
    usd = defaultdict(float)
    for r in rows:
        k = r["type"] + (":" + r["side"] if r["type"] == "TRADE" else "")
        usd[k] += float(r.get("usdcSize", 0))
        if r["type"] == "TRADE":
            mix["TRADE:" + r["side"]] += 1
    print("\n-- event mix (count / $) --")
    for k in ["TRADE:BUY", "TRADE:SELL", "REDEEM", "MERGE", "SPLIT", "REWARD",
              "CONVERSION"]:
        if mix.get(k) or usd.get(k):
            print(f"  {k:<11} {mix.get(k, 0):>7}  ${usd.get(k, 0):>12,.0f}")

    trades = [r for r in rows if r["type"] == "TRADE"]
    buys = [r for r in trades if r["side"] == "BUY"]
    sells = [r for r in trades if r["side"] == "SELL"]

    # family mix + share of available windows
    fam_mkts = defaultdict(set)
    fam_usd = defaultdict(float)
    other_mkts = set()
    for r in trades:
        meta = market_meta(r.get("slug", "") or "")
        if meta:
            asset, hz, hz_s, _ = meta
            fam_mkts[(asset, hz, hz_s)].add(r["slug"])
            fam_usd[(asset, hz, hz_s)] += float(r.get("usdcSize", 0))
        else:
            other_mkts.add(r.get("slug"))
    print("\n-- family mix (distinct markets traded / available windows in "
          "span / $ volume) --")
    for (asset, hz, hz_s), mkts in sorted(fam_mkts.items(),
                                          key=lambda kv: -len(kv[1])):
        avail = max(span // hz_s, 1)
        print(f"  {asset:>4} {hz:<3} {len(mkts):>5} mkts  "
              f"{len(mkts)/avail:>6.0%} of {avail} windows  "
              f"${fam_usd[(asset, hz, hz_s)]:>11,.0f}")
    if other_mkts:
        print(f"  other (non-updown): {len(other_mkts)} markets")

    # entry price bands (BUY fills)
    bands = [(0, .15), (.15, .30), (.30, .70), (.70, .85), (.85, 1.01)]
    print("\n-- BUY price bands (fills / $ / share of $) --")
    tot_usd = sum(float(b.get("usdcSize", 0)) for b in buys) or 1
    for lo, hi in bands:
        sel = [b for b in buys if lo <= float(b.get("price", 0)) < hi]
        u = sum(float(b.get("usdcSize", 0)) for b in sel)
        print(f"  [{lo:.2f},{hi:.2f}) {len(sel):>7}  ${u:>11,.0f}  "
              f"{u/tot_usd:>5.1%}")
    fill_usd = [float(b.get("usdcSize", 0)) for b in buys]
    if fill_usd:
        print(f"  $/BUY-fill: med {st.median(fill_usd):.1f}  "
              f"p90 {pct(fill_usd, .9):.1f}  max {max(fill_usd):,.0f}")

    # per-market behavior
    by_mkt = defaultdict(list)
    for r in trades:
        if market_meta(r.get("slug", "") or ""):
            by_mkt[r["slug"]].append(r)
    fills_per, orders_per, sweep_orders, n_orders = [], [], 0, 0
    both_sides, pair_vwaps, holds_close, sell_holds = 0, [], [], []
    pnl_rows = []  # (slug, close, cashflow, buy_usd)
    for slug, fs in by_mkt.items():
        meta = market_meta(slug)
        asset, hz, hz_s, close = meta
        fs.sort(key=lambda x: x["timestamp"])
        bf = [f for f in fs if f["side"] == "BUY"]
        fills_per.append(len(bf))
        clusters = defaultdict(list)
        for f in bf:
            clusters[(f["timestamp"], f.get("outcome"))].append(
                float(f.get("price", 0)))
        orders_per.append(len(clusters))
        n_orders += len(clusters)
        sweep_orders += sum(1 for px in clusters.values()
                            if len(set(px)) > 1)
        legs = defaultdict(lambda: [0.0, 0.0])
        first_buy = {}
        for f in bf:
            o = f.get("outcome")
            legs[o][0] += float(f.get("size", 0))
            legs[o][1] += float(f.get("usdcSize", 0))
            first_buy.setdefault(o, f["timestamp"])
            holds_close.append(close - f["timestamp"])
        outs = [o for o, l in legs.items() if l[0] > 0]
        if len(outs) >= 2:
            both_sides += 1
            vw = sorted(l[1] / l[0] for l in legs.values() if l[0] > 0)
            pair_vwaps.append(vw[0] + vw[1])
        for f in fs:
            if f["side"] == "SELL" and f.get("outcome") in first_buy:
                sell_holds.append(f["timestamp"] - first_buy[f["outcome"]])
    print("\n-- per-market (updown only) --")
    if fills_per:
        print(f"  BUY fills/mkt: med {st.median(fills_per):.0f} "
              f"p90 {pct(fills_per, .9):.0f} max {max(fills_per)}  "
              f"(order-clusters/mkt med {st.median(orders_per):.0f})")
        print(f"  multi-price sweep orders: {sweep_orders}/{n_orders} "
              f"({sweep_orders/max(n_orders,1):.0%}) -> taker-sweep hint")
        print(f"  both-sides bought: {both_sides}/{len(by_mkt)} "
              f"({both_sides/len(by_mkt):.0%})")
    if pair_vwaps:
        print(f"  pair VWAP sum: med {st.median(pair_vwaps):.3f}  "
              f"p25 {pct(pair_vwaps, .25):.3f}  p75 {pct(pair_vwaps, .75):.3f}  "
              f"<1.00: {sum(1 for c in pair_vwaps if c < 1)/len(pair_vwaps):.0%}")
    if holds_close:
        print(f"  entry->close: med {st.median(holds_close):.0f}s  "
              f"p10 {pct(holds_close, .1):.0f}s  p90 {pct(holds_close, .9):.0f}s")
    if sell_holds:
        print(f"  buy->sell (same outcome): med {st.median(sell_holds):.0f}s  "
              f"n={len(sell_holds)}")

    # hourly pattern (UTC) of trade fills
    byh = Counter(datetime.fromtimestamp(r["timestamp"], timezone.utc).hour
                  for r in trades)
    peak = max(byh.values()) if byh else 1
    print("\n-- fills by UTC hour --")
    for h in range(24):
        n = byh.get(h, 0)
        print(f"  {h:02d} {n:>6} {'#' * int(30 * n / peak)}")

    # P&L proxy: cash flow per fully-in-span resolved updown market
    cash = defaultdict(float)
    buy_cost = defaultdict(float)
    closes = {}
    for r in rows:
        slug = r.get("slug", "") or ""
        meta = market_meta(slug)
        if not meta:
            continue
        _, _, hz_s, close = meta
        if close - hz_s < ts_min or close > ts_max - 1800:
            continue  # window not fully inside data span, or too new to redeem
        closes[slug] = close
        u = float(r.get("usdcSize", 0))
        if r["type"] == "TRADE":
            if r["side"] == "BUY":
                cash[slug] -= u
                buy_cost[slug] += u
            else:
                cash[slug] += u
        elif r["type"] in ("REDEEM", "MERGE"):
            cash[slug] += u
        elif r["type"] == "SPLIT":
            cash[slug] -= u
    if cash:
        pl = list(cash.values())
        cost = sum(buy_cost.values())
        print("\n-- cash-flow P&L proxy (resolved updown mkts fully in span) --")
        print(f"  markets {len(pl)}  net ${sum(pl):+,.0f} on ${cost:,.0f} "
              f"buy volume ({sum(pl)/max(cost,1):+.2%})")
        print(f"  win-rate {sum(1 for x in pl if x > 0)/len(pl):.0%}  "
              f"med ${st.median(pl):+.2f}  p10 ${pct(pl, .1):+.2f}  "
              f"p90 ${pct(pl, .9):+.2f}")
        byday = defaultdict(float)
        for slug, v in cash.items():
            d = datetime.fromtimestamp(closes[slug], timezone.utc).date()
            byday[d] += v
        days = "  ".join(f"{d:%m-%d} ${v:+,.0f}" for d, v in sorted(byday.items()))
        print(f"  by close-day: {days}")
        print("  (caveat: open positions redeemed outside span, and rewards, "
              "are not captured)")


if __name__ == "__main__":
    main()
