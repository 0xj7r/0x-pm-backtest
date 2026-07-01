#!/usr/bin/env python3
"""Pattern scans: maker/taker fee detection, ladder accumulation, recycle
(sell 99c + buy opposite 1c), multi-horizon stacking, capital recycle rate."""
import json
import re
import statistics as st
import time
from collections import Counter, defaultdict
from datetime import datetime, timezone, timedelta

DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d"
NOW = int(time.time())
ET = timezone(timedelta(hours=-4))
MONTHS = {m: i + 1 for i, m in enumerate(
    "january february march april may june july august september october november december".split())}
HORIZON_SEC = {"5m": 300, "15m": 900, "1h": 3600}


def series_of(slug):
    m = re.match(r"(btc|eth|xrp|sol)-updown-(5m|15m|1h)-(\d+)", slug or "")
    if m:
        return m.group(1), m.group(2), int(m.group(3))
    m = re.match(r"(bitcoin|ethereum|solana|xrp)-up-or-down-(\w+)-(\d+)-2026-(\d+)(am|pm)-et", slug or "")
    if m:
        asset = {"bitcoin": "btc", "ethereum": "eth", "solana": "sol", "xrp": "xrp"}[m.group(1)]
        hr = int(m.group(4)) % 12 + (12 if m.group(5) == "pm" else 0)
        start = int(datetime(2026, MONTHS[m.group(2)], int(m.group(3)), hr, tzinfo=ET).timestamp())
        return asset, "1h", start
    return None, None, None


recs = []
for l in open(f"{DIR}/activity.jsonl"):
    r = json.loads(l)
    a, h, ws = series_of(r.get("slug"))
    if a is None:
        continue
    recs.append({
        "ts": r["timestamp"], "cid": r["conditionId"], "asset": a, "hor": h,
        "wstart": ws, "exp": ws + HORIZON_SEC[h], "side": r["side"],
        "out": r["outcome"], "oi": r["outcomeIndex"], "px": float(r["price"]),
        "qty": float(r["size"]), "usd": float(r["usdcSize"]),
    })
recs.sort(key=lambda r: r["ts"])

# ---- maker/taker via embedded fee ----
# taker BUY: usd = px*qty + fee; maker BUY: usd = px*qty (price may be rounded
# to 2dp in the feed so allow rounding band qty*0.005)
print("== maker/taker fee detection ==")
mk = Counter()
fee_total = 0.0
fee_by_mon = defaultdict(float)
vol_check = defaultdict(float)
for r in recs:
    raw = r["px"] * r["qty"]
    dev = (r["usd"] - raw) if r["side"] == "BUY" else (raw - r["usd"])
    fee_expect = 0.07 * r["px"] * (1 - r["px"]) * r["qty"]
    band = r["qty"] * 0.005 + 0.01
    if fee_expect > band:          # detectable
        if abs(dev - fee_expect) < 0.6 * fee_expect:
            mk["taker"] += 1
            vol_check["taker_usd"] += r["usd"]
        elif abs(dev) < band:
            mk["maker"] += 1
            vol_check["maker_usd"] += r["usd"]
        else:
            mk["odd"] += 1
            vol_check["odd_usd"] += r["usd"]
    else:
        mk["small/extreme"] += 1
        vol_check["small_usd"] += r["usd"]
    if dev > 0:
        fee_total += dev
        fee_by_mon[datetime.fromtimestamp(r["ts"], timezone.utc).strftime("%Y-%m")] += dev
print(dict(mk))
print({k: f"{v:,.0f}" for k, v in vol_check.items()})
print(f"implied fee paid (sum positive deviations): ${fee_total:,.0f}")
print("by month:", {k: f"{v:,.0f}" for k, v in sorted(fee_by_mon.items())})

# ---- ladder accumulation: per market+outcome buy fills ----
print("\n== ladder accumulation ==")
pos_buys = defaultdict(list)
for r in recs:
    if r["side"] == "BUY":
        pos_buys[(r["cid"], r["oi"])].append(r)
nf, span, usd_tot = [], [], []
for k, fs in pos_buys.items():
    fills = [f for f in fs if f["usd"] >= 1.0]   # ignore dust
    if not fills:
        continue
    nf.append(len(fills))
    pxs = [f["px"] for f in fills]
    span.append(max(pxs) - min(pxs))
    usd_tot.append(sum(f["usd"] for f in fills))
nf.sort(); span.sort(); usd_tot.sort()
n = len(nf)
print(f"positions (>=$1 buys): {n}")
print(f"buy fills per position: med {nf[n//2]} p75 {nf[3*n//4]} p90 {nf[9*n//10]} max {nf[-1]}")
print(f"px span within position: med {span[n//2]:.2f} p75 {span[3*n//4]:.2f} p90 {span[9*n//10]:.2f}")
print(f"position cost usd: med {usd_tot[n//2]:.0f} p75 {usd_tot[3*n//4]:.0f} "
      f"p90 {usd_tot[9*n//10]:.0f} p99 {usd_tot[99*n//100]:.0f} max {usd_tot[-1]:,.0f}")
lad = sum(1 for k, fs in pos_buys.items()
          if len([f for f in fs if f["usd"] >= 1]) >= 3
          and max(f["px"] for f in fs if f["usd"] >= 1) - min(f["px"] for f in fs if f["usd"] >= 1) >= 0.05)
print(f"laddered positions (>=3 fills spanning >=5c): {lad} ({100*lad/n:.1f}%)")

# ---- recycle: SELL >=0.95 then/with BUY opposite <=0.06 within 120s ----
print("\n== recycle pattern ==")
by_cid = defaultdict(list)
for r in recs:
    by_cid[r["cid"]].append(r)
n_events = 0
cheap_usd = cheap_sh = 0.0
gap_list = []
mkts_with = 0
hi_sell_usd = 0.0
cheap_recs = []   # for later flip-rate join with winners
for cid, fs in by_cid.items():
    sells_hi = [f for f in fs if f["side"] == "SELL" and f["px"] >= 0.95]
    buys_lo = [f for f in fs if f["side"] == "BUY" and f["px"] <= 0.06 and f["usd"] >= 0.5]
    if not sells_hi or not buys_lo:
        continue
    matched = False
    for b in buys_lo:
        near = [s for s in sells_hi if abs(s["ts"] - b["ts"]) <= 120
                and s["oi"] != b["oi"]]
        if near:
            matched = True
            n_events += 1
            cheap_usd += b["usd"]
            cheap_sh += b["qty"]
            gap_list.append(b["ts"] - max(s["ts"] for s in near if s["ts"] <= b["ts"] + 120))
            cheap_recs.append({"cid": cid, "oi": b["oi"], "qty": b["qty"],
                               "usd": b["usd"], "hor": b["hor"], "asset": b["asset"]})
    if matched:
        mkts_with += 1
        hi_sell_usd += sum(s["usd"] for s in sells_hi)
print(f"recycle events: {n_events} in {mkts_with} markets "
      f"({100*mkts_with/len(by_cid):.1f}% of markets)")
print(f"cheap-side tickets: {cheap_sh:,.0f} shares, ${cheap_usd:,.0f} "
      f"(avg px {cheap_usd/max(cheap_sh,1):.3f}); paired hi-sell proceeds ${hi_sell_usd:,.0f}")
if gap_list:
    gap_list.sort()
    print(f"sell->cheap-buy gap: med {gap_list[len(gap_list)//2]}s")
json.dump(cheap_recs, open(f"{DIR}/cheap_recycle_buys.json", "w"))

# all cheap buys (not only recycle-paired) for flip-rate later
all_cheap = [{"cid": r["cid"], "oi": r["oi"], "qty": r["qty"], "usd": r["usd"],
              "hor": r["hor"], "asset": r["asset"], "px": r["px"], "ts": r["ts"],
              "wstart": r["wstart"]}
             for r in recs if r["side"] == "BUY" and r["px"] <= 0.10]
json.dump(all_cheap, open(f"{DIR}/cheap_all_buys.json", "w"))
print(f"all cheap buys (<=0.10): {len(all_cheap)} fills, "
      f"${sum(c['usd'] for c in all_cheap):,.0f}")

# ---- multi-horizon stacking ----
print("\n== multi-horizon stacking ==")
# net signed exposure (Up positive) per asset+horizon market, active [first trade, exp]
mpos = defaultdict(float)
mspan = {}
for r in recs:
    k = (r["cid"])
    sgn = 1 if r["out"] == "Up" else -1
    q = r["qty"] * (1 if r["side"] == "BUY" else -1) * sgn
    mpos[k] += q
    if k not in mspan:
        mspan[k] = [r["ts"], r["exp"], r["asset"], r["hor"]]
    else:
        mspan[k][0] = min(mspan[k][0], r["ts"])
# sample: for each 5m market with |net|>10 shares, check 15m and 1h same-asset
# markets active at its midpoint and their net direction
agree = Counter()
for cid, net in mpos.items():
    t0, exp, a, h = mspan[cid]
    if h != "5m" or abs(net) < 10:
        continue
    mid = (t0 + exp) / 2
    dir5 = 1 if net > 0 else -1
    for cid2, net2 in mpos.items():
        t02, exp2, a2, h2 = mspan[cid2]
        if a2 != a or h2 == "5m" or abs(net2) < 10:
            continue
        if t02 <= mid <= exp2:
            d2 = 1 if net2 > 0 else -1
            agree[(h2, "agree" if d2 == dir5 else "disagree")] += 1
print(dict(agree))

# ---- intraday capital recycle ----
print("\n== capital recycle ==")
events = sorted([(r["ts"], -r["usd"] if r["side"] == "BUY" else r["usd"]) for r in recs])
redeems = [json.loads(l) for l in open(f"{DIR}/nontrade.jsonl")
           if json.loads(l).get("type") == "REDEEM"]
events += [(r["timestamp"], float(r["usdcSize"])) for r in redeems]
events.sort()
dep = 0.0
day_peak = defaultdict(float)
for ts, v in events:
    dep -= v
    d = datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%d")
    day_peak[d] = max(day_peak[d], dep)
bvol = defaultdict(float)
for r in recs:
    if r["side"] == "BUY":
        bvol[datetime.fromtimestamp(r["ts"], timezone.utc).strftime("%Y-%m-%d")] += r["usd"]
ratios = []
for d in sorted(bvol):
    base = day_peak[d]
    if base > 1000:
        ratios.append(bvol[d] / base)
ratios.sort()
print(f"daily buy volume / same-day peak net deployed: med {ratios[len(ratios)//2]:.1f}x "
      f"p90 {ratios[len(ratios)*9//10]:.1f}x  (n={len(ratios)} days)")
print("note: peak deployed is cumulative-from-Apr2 so later days inherit drawdown; "
      "interpret with winners-based equity curve")

# trade hours profile (UTC)
hh = Counter(datetime.fromtimestamp(r["ts"], timezone.utc).hour for r in recs)
print("\nfills by UTC hour:", [hh.get(h, 0) for h in range(24)])
