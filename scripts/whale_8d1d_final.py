#!/usr/bin/env python3
"""Final economics with authoritative winners: imputed-payout P&L, hit rates,
cheap-ticket flip rate, per-series and per-day P&L, reconciliation."""
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


# winners: outcome name ("Up"/"Down") per cid
winner = {}
for l in open(f"{DIR}/winners.jsonl"):
    r = json.loads(l)
    if r.get("winner"):
        winner[r["cid"]] = r["winner"]
print(f"winners loaded: {len(winner)}")

recs = []
for l in open(f"{DIR}/activity.jsonl"):
    r = json.loads(l)
    a, h, ws = series_of(r.get("slug"))
    if a is None:
        continue
    recs.append({
        "ts": r["timestamp"], "cid": r["conditionId"], "asset": a, "hor": h,
        "wstart": ws, "exp": ws + HORIZON_SEC[h], "side": r["side"],
        "out": r["outcome"], "px": float(r["price"]),
        "qty": float(r["size"]), "usd": float(r["usdcSize"]),
    })

redeem_usd = defaultdict(float)
for l in open(f"{DIR}/nontrade.jsonl"):
    r = json.loads(l)
    if r.get("type") == "REDEEM":
        redeem_usd[r["conditionId"]] += float(r["usdcSize"])

# per market aggregation
pos = defaultdict(lambda: defaultdict(lambda: [0.0, 0.0, 0.0, 0.0]))  # cid -> out -> bq,busd,sq,susd
meta = {}
buy_fills = defaultdict(list)
for r in recs:
    p = pos[r["cid"]][r["out"]]
    if r["side"] == "BUY":
        p[0] += r["qty"]; p[1] += r["usd"]
        buy_fills[r["cid"]].append(r)
    else:
        p[2] += r["qty"]; p[3] += r["usd"]
    meta[r["cid"]] = (r["asset"], r["hor"], r["exp"])

# P&L with imputed payout: cash from trades + net_held(winner)*$1
pnl_mkt = {}
imputed_payout_tot = 0.0
recorded_redeem_tot = 0.0
n_excl = 0
for cid, (a, h, exp) in meta.items():
    if exp > NOW - 120:
        continue
    w = winner.get(cid)
    cash = 0.0
    payout = 0.0
    held = False
    for out, (bq, busd, sq, susd) in pos[cid].items():
        cash += susd - busd
        if bq - sq > 1.0:
            held = True
        if w is not None and out == w:
            payout += max(bq - sq, 0.0)
    if w is None and held:
        n_excl += 1   # unresolved payout would register as fake loss
        continue
    pnl_mkt[cid] = cash + payout
    imputed_payout_tot += payout
    recorded_redeem_tot += redeem_usd.get(cid, 0.0)
print(f"excluded expired markets with held shares but unknown winner: {n_excl}")

print(f"imputed payouts ${imputed_payout_tot:,.0f} vs recorded REDEEM "
      f"${recorded_redeem_tot:,.0f} (gap = unrecorded auto-redeems)")
total = sum(pnl_mkt.values())
print(f"TOTAL P&L (imputed): ${total:+,.0f}  (leaderboard all-time: +$103,854)")

unknown_held = sum(1 for cid in meta if cid not in winner and meta[cid][2] < NOW - 120
                   and any(p[0] - p[2] > 1 for p in pos[cid].values()))
print(f"expired markets w/o winner but net shares held (P&L undercount risk): {unknown_held}")

# by series
print("\n== P&L by series (imputed) ==")
bys = defaultdict(float)
for cid, v in pnl_mkt.items():
    a, h, _ = meta[cid]
    bys[(a, h)] += v
for (a, h), v in sorted(bys.items(), key=lambda kv: -kv[1]):
    print(f"  {a}-{h:4s} {v:+12,.0f}")

# by month and day
byd = defaultdict(float)
for cid, v in pnl_mkt.items():
    d = datetime.fromtimestamp(meta[cid][2], timezone.utc).strftime("%Y-%m-%d")
    byd[d] += v
bym = defaultdict(float)
for d, v in byd.items():
    bym[d[:7]] += v
print("\n== P&L by month ==", {k: f"{v:+,.0f}" for k, v in sorted(bym.items())})
days = sorted(byd)
dv = [byd[d] for d in days]
print(f"daily: {sum(1 for v in dv if v > 0)}/{len(dv)} green, med {st.median(dv):+,.0f}, "
      f"best {max(dv):+,.0f} {days[dv.index(max(dv))]}, worst {min(dv):+,.0f} {days[dv.index(min(dv))]}")
print("last 12 days:")
for d in days[-12:]:
    print(f"  {d}: {byd[d]:+10,.0f}")

# hit rate by entry bucket (share-weighted, true winners)
print("\n== hit rate by BUY price bucket (true winners) ==")
buck = defaultdict(lambda: [0.0, 0.0, 0.0])
for cid, fills in buy_fills.items():
    w = winner.get(cid)
    if w is None or meta[cid][2] > NOW - 120:
        continue
    for f in fills:
        b = min(int(f["px"] * 10), 9)
        buck[b][1] += f["qty"]
        buck[b][2] += f["usd"]
        if f["out"] == w:
            buck[b][0] += f["qty"]
print(f"{'bucket':9s}{'shares':>12s}{'usd':>12s}{'win%':>7s}{'be%':>7s}{'edge/$1':>9s}")
for b in sorted(buck):
    wq, t, u = buck[b]
    if t < 1:
        continue
    avg = u / t
    print(f"{b/10:.1f}-{(b+1)/10:.1f}  {t:12,.0f}{u:12,.0f}{100*wq/t:7.1f}{100*avg:7.1f}"
          f"{(wq - u)/u:+9.3f}")

# finer buckets at extremes
print("\nfine buckets (<=0.10):")
fb = defaultdict(lambda: [0.0, 0.0, 0.0])
for cid, fills in buy_fills.items():
    w = winner.get(cid)
    if w is None or meta[cid][2] > NOW - 120:
        continue
    for f in fills:
        if f["px"] <= 0.10:
            b = round(f["px"], 2)
            fb[b][1] += f["qty"]
            fb[b][2] += f["usd"]
            if f["out"] == w:
                fb[b][0] += f["qty"]
for b in sorted(fb):
    wq, t, u = fb[b]
    if u < 100:
        continue
    print(f"  px={b:.2f}  sh {t:10,.0f} usd {u:9,.0f} win% {100*wq/t:6.2f} "
          f"pnl ${wq - u:+10,.0f}")

# cheap-ticket flip economics by horizon
print("\n== cheap buys (<=0.10) P&L by series ==")
cb = defaultdict(lambda: [0.0, 0.0, 0.0])
for cid, fills in buy_fills.items():
    w = winner.get(cid)
    if w is None or meta[cid][2] > NOW - 120:
        continue
    a, h, _ = meta[cid]
    for f in fills:
        if f["px"] <= 0.10:
            cb[(a, h)][1] += f["qty"]
            cb[(a, h)][2] += f["usd"]
            if f["out"] == w:
                cb[(a, h)][0] += f["qty"]
for k in sorted(cb, key=lambda k: -(cb[k][0] - cb[k][2])):
    wq, t, u = cb[k]
    print(f"  {k[0]}-{k[1]:4s} sh {t:10,.0f} usd {u:9,.0f} flip% {100*wq/max(t,1):6.2f} "
          f"pnl ${wq - u:+10,.0f}")

# recycle-paired cheap tickets flip rate
cheap = json.load(open(f"{DIR}/cheap_recycle_buys.json"))
wq = tq = uu = 0.0
nk = 0
for c in cheap:
    w = winner.get(c["cid"])
    if w is None:
        nk += 1
        continue
    tq += c["qty"]; uu += c["usd"]
    # oi -> outcome name: oi 0 = Up? verify via any rec
    # use outcome name stored? cheap_recycle stored oi only; map via pos keys
for c in cheap:
    pass
print(f"\nrecycle-paired cheap tickets: {len(cheap)} fills (winner unknown for {nk}); "
      f"see fine-bucket table for flip economics")

# P&L split: cheap tickets vs >=0.85 favourites vs mid
print("\n== P&L attribution by entry bucket (winner-known mkts) ==")
att = defaultdict(float)
for cid, fills in buy_fills.items():
    w = winner.get(cid)
    if w is None or meta[cid][2] > NOW - 120:
        continue
    for f in fills:
        seg = ("tail<=0.10" if f["px"] <= 0.10 else
               "0.10-0.50" if f["px"] < 0.50 else
               "0.50-0.85" if f["px"] < 0.85 else "fav>=0.85")
        att[seg] += (f["qty"] if f["out"] == w else 0.0) - f["usd"]
# note: ignores sells (treats every buy as held to expiry) -> shows raw bet quality
for k, v in sorted(att.items()):
    print(f"  {k:12s} {v:+12,.0f}   (held-to-expiry counterfactual)")

# equity curve: cash flows + imputed payouts at expiry
print("\n== equity curve (cash + imputed payouts) ==")
ev = []
for r in recs:
    ev.append((r["ts"], -r["usd"] if r["side"] == "BUY" else r["usd"]))
for cid, (a, h, exp) in meta.items():
    w = winner.get(cid)
    if w is None or exp > NOW - 120:
        continue
    payout = max(pos[cid].get(w, [0, 0, 0, 0])[0] - pos[cid].get(w, [0, 0, 0, 0])[2], 0.0)
    if payout > 0:
        ev.append((exp, payout))
ev.sort()
eq = 0.0
lo = hi = 0.0
lo_ts = hi_ts = 0
peak = 0.0
maxdd = 0.0
for ts, v in ev:
    eq += v
    if eq < lo:
        lo, lo_ts = eq, ts
    if eq > hi:
        hi, hi_ts = eq, ts
    peak = max(peak, eq)
    maxdd = max(maxdd, peak - eq)
print(f"min equity ${lo:,.0f} at {datetime.fromtimestamp(lo_ts, timezone.utc)} "
      f"(capital required from own cash); final ${eq:,.0f}")
print(f"max equity ${hi:,.0f} at {datetime.fromtimestamp(hi_ts, timezone.utc)}; "
      f"max drawdown ${maxdd:,.0f}")
