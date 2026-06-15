#!/usr/bin/env python3
"""Ground-truth checks: leaderboard P&L, gamma resolutions vs inferred winners,
redeem coverage, sample market forensics."""
import json
import random
import time
import urllib.parse
import urllib.request
from collections import defaultdict
from datetime import datetime, timezone

DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d"
ADDR = "0x8d1d5d1c6041b13fc708b5d9f668070e1724ed4a"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "pm-research/1.0"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


# 1. leaderboard / authoritative pnl
for win in ("1d", "7d", "30d", "all"):
    try:
        d = get(f"https://lb-api.polymarket.com/profit?window={win}&limit=1&address={ADDR}")
        print(f"lb profit {win}: {d}")
    except Exception as e:
        print(f"lb profit {win} failed: {e}")
try:
    d = get(f"https://data-api.polymarket.com/value?user={ADDR}")
    print(f"portfolio value: {d}")
except Exception as e:
    print(f"value failed: {e}")

# 2. redeem rows per day (recent) + total
redeems = [json.loads(l) for l in open(f"{DIR}/nontrade.jsonl")
           if json.loads(l).get("type") == "REDEEM"]
tot = sum(float(r["usdcSize"]) for r in redeems)
byday = defaultdict(float)
cnt = defaultdict(int)
for r in redeems:
    d = datetime.fromtimestamp(r["timestamp"], timezone.utc).strftime("%Y-%m-%d")
    byday[d] += float(r["usdcSize"])
    cnt[d] += 1
print(f"\nredeem total ${tot:,.0f} over {len(redeems)} rows")
for d in sorted(byday)[-8:]:
    print(f"  {d}: {cnt[d]:4d} redeems ${byday[d]:10,.0f}")

# 3. gamma resolution check on sample of condition ids
trades = [json.loads(l) for l in open(f"{DIR}/activity.jsonl")]
by_cid = defaultdict(list)
for t in trades:
    by_cid[t["conditionId"]].append(t)
cids = list(by_cid)
random.seed(7)
sample = random.sample(cids, 40)
agree = disagree = unknown = 0
# rebuild my winner inference
winner = {}
for r in redeems:
    winner[r["conditionId"]] = ("redeem", r["outcomeIndex"])
for cid in cids:
    if cid in winner:
        continue
    best = None
    for t in by_cid[cid]:
        px = float(t["price"])
        if px >= 0.98:
            cand = t["outcomeIndex"]
        elif px <= 0.02:
            cand = 1 - t["outcomeIndex"]
        else:
            continue
        if best is None or t["timestamp"] > best[0]:
            best = (t["timestamp"], cand)
    if best:
        winner[cid] = ("fallback", best[1])

for cid in sample:
    try:
        m = get(f"https://gamma-api.polymarket.com/markets?condition_ids={cid}")
        if not m:
            unknown += 1
            continue
        m = m[0]
        prices = json.loads(m.get("outcomePrices", "[]"))
        outs = json.loads(m.get("outcomes", "[]"))
        if not prices:
            unknown += 1
            continue
        true_oi = 0 if float(prices[0]) > 0.5 else 1
        mine = winner.get(cid)
        tag = "none"
        if mine:
            tag = f"{mine[0]}:{'OK' if mine[1] == true_oi else 'WRONG'}"
            if mine[1] == true_oi:
                agree += 1
            else:
                disagree += 1
        else:
            unknown += 1
        print(f"  {m['slug'][:42]:44s} true={outs[true_oi]:4s} {tag}")
        time.sleep(0.1)
    except Exception as e:
        print(f"  {cid[:16]} gamma err {e}")
print(f"\nagree {agree} disagree {disagree} no-inference/unresolved {unknown}")

# 4. forensic dump of one recycle-looking market (sell>=0.97 + buy<=0.05)
shown = 0
for cid in cids:
    fs = sorted(by_cid[cid], key=lambda t: t["timestamp"])
    has_hi_sell = any(t["side"] == "SELL" and float(t["price"]) >= 0.97 for t in fs)
    has_lo_buy = any(t["side"] == "BUY" and float(t["price"]) <= 0.05 for t in fs)
    if has_hi_sell and has_lo_buy and len(fs) <= 14:
        print(f"\n-- {fs[0]['slug']} ({cid[:10]}...) --")
        for t in fs:
            print(f"  {datetime.fromtimestamp(t['timestamp'], timezone.utc).strftime('%H:%M:%S')} "
                  f"{t['side']:4s} {t['outcome']:4s} px={float(t['price']):.3f} "
                  f"qty={float(t['size']):8.1f} usd={float(t['usdcSize']):8.2f}")
        rd = [r for r in redeems if r["conditionId"] == cid]
        for r in rd:
            print(f"  REDEEM oi={r['outcomeIndex']} size={r['size']} usd={r['usdcSize']}")
        try:
            m = get(f"https://gamma-api.polymarket.com/markets?condition_ids={cid}")[0]
            print(f"  gamma outcomes={m.get('outcomes')} prices={m.get('outcomePrices')}")
        except Exception as e:
            print(f"  gamma err {e}")
        shown += 1
        if shown >= 3:
            break
