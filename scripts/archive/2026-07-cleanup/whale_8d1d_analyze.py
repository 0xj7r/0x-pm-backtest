#!/usr/bin/env python3
"""Core decomposition of whale 0x8d1d activity: universe, execution, economics."""
import json
import re
import statistics as st
import time
from collections import Counter, defaultdict
from datetime import datetime, timezone, timedelta

DIR = "/Users/jackreid/go/polymarket-backtest/data/external/whale_8d1d"
NOW = int(time.time())

trades = [json.loads(l) for l in open(f"{DIR}/activity.jsonl")]
redeems = [json.loads(l) for l in open(f"{DIR}/nontrade.jsonl")
           if json.loads(l).get("type") == "REDEEM"]

ET = timezone(timedelta(hours=-4))  # EDT for Apr-Jun
MONTHS = {m: i + 1 for i, m in enumerate(
    "january february march april may june july august september october november december".split())}


def series_of(slug, title):
    m = re.match(r"(btc|eth|xrp|sol|doge)-updown-(5m|15m|1h)-(\d+)", slug or "")
    if m:
        return m.group(1), m.group(2), int(m.group(3))
    m = re.match(r"(bitcoin|ethereum|solana|xrp|dogecoin)-up-or-down-(\w+)-(\d+)-2026-(\d+)(am|pm)-et",
                 slug or "")
    if m:
        asset = {"bitcoin": "btc", "ethereum": "eth", "solana": "sol",
                 "xrp": "xrp", "dogecoin": "doge"}[m.group(1)]
        mon, day, hr, ap = MONTHS[m.group(2)], int(m.group(3)), int(m.group(4)), m.group(5)
        if ap == "pm" and hr != 12:
            hr += 12
        if ap == "am" and hr == 12:
            hr = 0
        start = int(datetime(2026, mon, day, hr, tzinfo=ET).timestamp())
        return asset, "1h", start
    return None, None, None


HORIZON_SEC = {"5m": 300, "15m": 900, "1h": 3600}

# enrich
other = Counter()
recs = []
for r in trades:
    a, h, ws = series_of(r.get("slug"), r.get("title"))
    if a is None:
        other[(r.get("slug") or "?")[:40]] += 1
        continue
    recs.append({
        "ts": r["timestamp"], "cid": r["conditionId"], "asset": a, "hor": h,
        "wstart": ws, "exp": ws + HORIZON_SEC[h], "side": r["side"],
        "out": r["outcome"], "oi": r["outcomeIndex"], "px": float(r["price"]),
        "qty": float(r["size"]), "usd": float(r["usdcSize"]),
        "tx": r["transactionHash"],
    })
print(f"classified {len(recs)}/{len(trades)} trades; unclassified slugs: {other.most_common(5)}")

# sanity: trade time vs window
offs = [r["ts"] - r["wstart"] for r in recs if r["hor"] == "5m"]
print(f"5m trade offset from slug ts: p5 {sorted(offs)[len(offs)//20]} "
      f"median {st.median(offs):.0f} p95 {sorted(offs)[len(offs)*19//20]} max {max(offs)}")

# ---- 1. UNIVERSE ----
print("\n== UNIVERSE: buy volume USD by series x month ==")
vol = defaultdict(float)
nmkt = defaultdict(set)
for r in recs:
    mon = datetime.fromtimestamp(r["ts"], timezone.utc).strftime("%Y-%m")
    if r["side"] == "BUY":
        vol[(r["asset"], r["hor"], mon)] += r["usd"]
    nmkt[(r["asset"], r["hor"])].add(r["cid"])
mons = sorted({k[2] for k in vol})
print(f"{'series':12s}" + "".join(f"{m:>12s}" for m in mons) + f"{'markets':>9s}")
for key in sorted(nmkt, key=lambda k: -sum(vol.get((k[0], k[1], m), 0) for m in mons)):
    a, h = key
    row = "".join(f"{vol.get((a, h, m), 0):12,.0f}" for m in mons)
    print(f"{a+'-'+h:12s}{row}{len(nmkt[key]):9d}")
tot = defaultdict(float)
for (a, h, m), v in vol.items():
    tot[m] += v
print(f"{'TOTAL':12s}" + "".join(f"{tot[m]:12,.0f}" for m in mons))

# ---- redeem map: winner per market ----
winner = {}     # cid -> outcomeIndex that won (from redeem)
redeem_usd = defaultdict(float)   # cid -> payout
for r in redeems:
    cid = r["conditionId"]
    redeem_usd[cid] += float(r["usdcSize"])
    winner[cid] = r["outcomeIndex"]
# fallback winner: own trades at extreme price nearest expiry
cand = {}
for r in recs:
    if r["px"] >= 0.98:
        prev = cand.get(r["cid"])
        if prev is None or r["ts"] > prev[0]:
            cand[r["cid"]] = (r["ts"], r["oi"])
    elif r["px"] <= 0.02:
        prev = cand.get(r["cid"])
        if prev is None or r["ts"] > prev[0]:
            cand[r["cid"]] = (r["ts"], 1 - r["oi"])
n_fb = 0
for cid, (_, oi) in cand.items():
    if cid not in winner:
        winner[cid] = oi
        n_fb += 1
all_cids = {r["cid"] for r in recs}
print(f"\nwinner known: {len(winner)}/{len(all_cids)} markets "
      f"({len(winner)-n_fb} via redeem, {n_fb} via extreme-price fallback)")

# ---- per-market-outcome aggregation ----
pos = defaultdict(lambda: [0.0, 0.0, 0.0, 0.0, []])  # (cid,oi) -> [bq,busd,sq,susd,buyfills]
mkt_meta = {}
for r in recs:
    k = (r["cid"], r["oi"])
    p = pos[k]
    if r["side"] == "BUY":
        p[0] += r["qty"]; p[1] += r["usd"]
        p[4].append(r)
    else:
        p[2] += r["qty"]; p[3] += r["usd"]
    mkt_meta[r["cid"]] = (r["asset"], r["hor"], r["exp"])

# ---- 3. ECONOMICS: realized P&L per market = sells + redeem - buys ----
# exclude markets not yet expired
pnl_by_series_day = defaultdict(float)
pnl_by_day = defaultdict(float)
pnl_by_series = defaultdict(float)
buys_by_day = defaultdict(float)
open_mkts = 0
for cid, (a, h, exp) in mkt_meta.items():
    if exp > NOW:
        open_mkts += 1
        continue
    cash = redeem_usd.get(cid, 0.0)
    # if no redeem but winner known and net shares held -> assume auto-redeem missing? count payout
    for oi in (0, 1):
        p = pos.get((cid, oi))
        if not p:
            continue
        cash += p[3] - p[1]
        net = p[0] - p[2]
        if cid not in redeem_usd and winner.get(cid) == oi and net > 0.01:
            cash += net  # unredeemed winning shares at $1 (pending redemption)
    day = datetime.fromtimestamp(exp, timezone.utc).strftime("%Y-%m-%d")
    pnl_by_series_day[(a, h, day)] += cash
    pnl_by_day[day] += cash
    pnl_by_series[(a, h)] += cash
print(f"open (unexpired) markets excluded: {open_mkts}")

print("\n== P&L by series (realized, Apr 2 - Jun 12) ==")
for (a, h), v in sorted(pnl_by_series.items(), key=lambda kv: -kv[1]):
    print(f"  {a}-{h:4s} {v:+12,.0f}")
print(f"  TOTAL    {sum(pnl_by_series.values()):+12,.0f}")

print("\n== P&L by month ==")
bym = defaultdict(float)
for d, v in pnl_by_day.items():
    bym[d[:7]] += v
for m in sorted(bym):
    print(f"  {m}: {bym[m]:+12,.0f}")

days = sorted(pnl_by_day)
dvals = [pnl_by_day[d] for d in days]
green = sum(1 for v in dvals if v > 0)
print(f"\ndaily P&L: {green}/{len(days)} green, median {st.median(dvals):+,.0f}, "
      f"best {max(dvals):+,.0f} ({days[dvals.index(max(dvals))]}), "
      f"worst {min(dvals):+,.0f} ({days[dvals.index(min(dvals))]})")
print("last 10 days:")
for d in days[-10:]:
    print(f"  {d}: {pnl_by_day[d]:+10,.0f}")

# ---- hit rate by entry price bucket ----
print("\n== hit rate by BUY price bucket (share-weighted, winner-known markets) ==")
buck = defaultdict(lambda: [0.0, 0.0, 0.0])  # bucket -> [win_sh, tot_sh, usd]
for (cid, oi), p in pos.items():
    w = winner.get(cid)
    if w is None or mkt_meta[cid][2] > NOW:
        continue
    for f in p[4]:
        b = min(int(f["px"] * 10), 9)
        buck[b][1] += f["qty"]
        buck[b][2] += f["usd"]
        if w == oi:
            buck[b][0] += f["qty"]
print(f"{'bucket':10s}{'shares':>12s}{'usd':>12s}{'win%':>8s}{'breakeven':>10s}{'edge/sh':>9s}")
for b in sorted(buck):
    w, t, u = buck[b]
    avg_px = u / t
    print(f"{b/10:.1f}-{(b+1)/10:.1f}   {t:12,.0f}{u:12,.0f}{100*w/t:8.1f}{100*avg_px:10.1f}"
          f"{(w/t - avg_px):9.3f}")

# ---- 2. EXECUTION ----
print("\n== EXECUTION ==")
fills_by_day = Counter(datetime.fromtimestamp(r["ts"], timezone.utc).strftime("%Y-%m-%d") for r in recs)
print(f"fills/day: median {st.median(fills_by_day.values()):.0f} max {max(fills_by_day.values())}")
usds = sorted(r["usd"] for r in recs if r["side"] == "BUY")
n = len(usds)
print(f"BUY fill usd: p25 {usds[n//4]:.0f} med {usds[n//2]:.0f} p75 {usds[3*n//4]:.0f} "
      f"p95 {usds[19*n//20]:.0f} p99 {usds[99*n//100]:.0f} max {usds[-1]:,.0f}")
# order-level: group by tx+side+oi+cid
orders = defaultdict(lambda: [0.0, set()])
for r in recs:
    o = orders[(r["tx"], r["cid"], r["oi"], r["side"])]
    o[0] += r["usd"]; o[1].add(r["px"])
ousd = sorted(v[0] for v in orders.values())
multi_px = sum(1 for v in orders.values() if len(v[1]) > 1)
print(f"orders (tx-grouped): {len(orders)}, usd med {ousd[len(ousd)//2]:.0f} "
      f"p95 {ousd[len(ousd)*19//20]:.0f}; multi-price (taker sweep) {multi_px} "
      f"({100*multi_px/len(orders):.1f}%)")

# entry price distribution by side bought
print("\nBUY price distribution (USD-weighted):")
pxh = defaultdict(float)
for r in recs:
    if r["side"] == "BUY":
        pxh[min(int(r["px"] * 10), 9)] += r["usd"]
tu = sum(pxh.values())
for b in sorted(pxh):
    print(f"  {b/10:.1f}-{(b+1)/10:.1f}: {100*pxh[b]/tu:5.1f}%  {pxh[b]:12,.0f}")

# hold pattern per market-outcome
cat = Counter()
for (cid, oi), p in pos.items():
    if mkt_meta[cid][2] > NOW or p[0] <= 0:
        continue
    fr = p[2] / p[0]
    if fr < 0.1:
        cat["hold_to_expiry"] += 1
    elif fr > 0.9:
        cat["full_exit"] += 1
    else:
        cat["partial_exit"] += 1
print(f"\nhold pattern (per market-outcome with buys): {dict(cat)}")

# entry timing within window (5m/15m)
for h in ("5m", "15m", "1h"):
    sec = HORIZON_SEC[h]
    rel = [(r["ts"] - r["wstart"]) / sec for r in recs
           if r["hor"] == h and r["side"] == "BUY"]
    if rel:
        rel.sort()
        n = len(rel)
        print(f"{h}: buy timing as fraction of window: p10 {rel[n//10]:.2f} "
              f"med {rel[n//2]:.2f} p90 {rel[9*n//10]:.2f} "
              f"(<0 = pre-open, >1 = post-close)")

# fee estimate (taker fee 0.07*p*(1-p) per share both sides)
fee = sum(0.07 * r["px"] * (1 - r["px"]) * r["qty"] for r in recs)
fee_buy = sum(0.07 * r["px"] * (1 - r["px"]) * r["qty"] for r in recs if r["side"] == "BUY")
print(f"\nfee estimate if ALL taker: total ${fee:,.0f} (buys ${fee_buy:,.0f}, "
      f"sells ${fee-fee_buy:,.0f})")

# capital deployed: cumulative net cash outflow
events = []
for r in recs:
    events.append((r["ts"], -r["usd"] if r["side"] == "BUY" else r["usd"]))
for r in redeems:
    events.append((r["timestamp"], float(r["usdcSize"])))
events.sort()
deployed = 0.0
peak = 0.0
peak_ts = 0
for ts, v in events:
    deployed -= v
    if deployed > peak:
        peak, peak_ts = deployed, ts
print(f"peak net cash deployed: ${peak:,.0f} at "
      f"{datetime.fromtimestamp(peak_ts, timezone.utc)}")
bvol_day = defaultdict(float)
for r in recs:
    if r["side"] == "BUY":
        bvol_day[datetime.fromtimestamp(r['ts'], timezone.utc).strftime('%Y-%m-%d')] += r["usd"]
med_bvol = st.median(sorted(bvol_day.values()))
print(f"median daily buy volume ${med_bvol:,.0f} -> turnover ~{med_bvol/max(peak,1):.1f}x peak capital/day")
