#!/usr/bin/env python3
"""Audit taker-fee rebates for the live wallet from the public data API."""
import json
import time
import urllib.request
from collections import Counter, defaultdict
from datetime import datetime, timezone

ADDR = "0x00190179D84224687aDfB93e4A499D36AAD0FF59"
BASE = "https://data-api.polymarket.com/activity"
MONTH_START = int(datetime(2026, 6, 1, tzinfo=timezone.utc).timestamp())
NOW = int(time.time())


def fetch_window(start, end):
    rows, offset = [], 0
    while offset <= 10000:
        url = (f"{BASE}?user={ADDR}&limit=500&offset={offset}"
               f"&start={start}&end={end}")
        req = urllib.request.Request(
            url, headers={"User-Agent": "pm-research/1.0"})
        with urllib.request.urlopen(req, timeout=30) as r:
            batch = json.loads(r.read())
        time.sleep(0.15)
        if not batch:
            break
        rows.extend(batch)
        if len(batch) < 500:
            break
        offset += 500
    if offset > 10000:
        print(f"WARNING: offset ceiling hit for window {start}-{end}")
    return rows


def main():
    rows = []
    win = 6 * 3600
    t = MONTH_START
    while t < NOW:
        rows.extend(fetch_window(t, min(t + win, NOW)))
        t += win
    seen = set()
    uniq = []
    for r in rows:
        k = (r["transactionHash"], r.get("asset"), r.get("type"),
             r.get("size"), r.get("timestamp"))
        if k not in seen:
            seen.add(k)
            uniq.append(r)
    types = Counter(r["type"] for r in uniq)
    print("activity types MTD:", dict(types))

    trades = [r for r in uniq if r["type"] == "TRADE"]
    wv = sum(r["size"] * (1 - r["price"]) for r in trades)
    notional = sum(r["usdcSize"] for r in trades)
    fees = sum(0.07 * r["price"] * (1 - r["price"]) * r["size"] for r in trades)
    days = (NOW - MONTH_START) / 86400
    per_day = defaultdict(lambda: [0.0, 0.0, 0])
    for r in trades:
        d = datetime.fromtimestamp(r["timestamp"], timezone.utc).date().isoformat()
        per_day[d][0] += r["size"] * (1 - r["price"])
        per_day[d][1] += 0.07 * r["price"] * (1 - r["price"]) * r["size"]
        per_day[d][2] += 1

    print(f"\nMTD (June 1 - now, {days:.2f} days):")
    print(f"  trades: {len(trades)}  notional: ${notional:,.2f}")
    print(f"  weighted volume wV: ${wv:,.2f}")
    print(f"  est fees paid: ${fees:,.2f}")
    print("\nper-day:")
    for d in sorted(per_day):
        v = per_day[d]
        print(f"  {d}: wV ${v[0]:>9,.2f}  fees ${v[1]:>7,.2f}  fills {v[2]}")

    tiers = [(10_000_000, "Obsidian", 0.50), (1_000_000, "Platinum", 0.32),
             (200_000, "Gold", 0.18), (20_000, "Silver", 0.08),
             (2_000, "Bronze", 0.03)]
    tier, rate = "None", 0.0
    for thresh, name, rr in tiers:
        if wv >= thresh:
            tier, rate = name, rr
            break
    print(f"\ntier (assuming Silver threshold $20k): {tier} rate {rate:.0%}")
    print(f"  fees/day: ${fees / days:,.2f}")
    print(f"  rebate accrual: ${fees * rate / days:,.2f}/day")
    for mult in (5, 20):
        wv_m = wv * mult
        t_m, r_m = "None", 0.0
        for thresh, name, rr in tiers:
            if wv_m >= thresh:
                t_m, r_m = name, rr
                break
        print(f"  at {mult}x: wV ${wv_m:,.0f} -> {t_m} {r_m:.0%}, "
              f"rebate ${fees * mult * r_m / days:,.2f}/day "
              f"(fees ${fees * mult / days:,.2f}/day)")

    non_trade = [r for r in uniq if r["type"] != "TRADE"]
    nt_types = Counter(r["type"] for r in non_trade)
    print("\nnon-trade rows:", dict(nt_types))
    for ty in nt_types:
        if ty in ("TRADE", "REDEEM", "SPLIT", "MERGE"):
            continue
        sample = [r for r in non_trade if r["type"] == ty][:5]
        for s in sample:
            print(" ", ty, s["timestamp"], s.get("usdcSize"), s.get("title"))

    with open("/tmp/rebate_audit_trades.json", "w") as f:
        json.dump(uniq, f)
    print("\nraw rows saved to /tmp/rebate_audit_trades.json")


if __name__ == "__main__":
    main()
