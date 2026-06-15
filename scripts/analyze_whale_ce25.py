#!/usr/bin/env python3
"""Decompose whale ce25 activity: universe, execution, economics, archetype."""
import json, re, sys
from collections import defaultdict, Counter
from datetime import datetime, timezone

PATH = sys.argv[1] if len(sys.argv) > 1 else \
    "/Users/jackreid/go/polymarket-backtest/data/external/whale_ce25/activity_raw.jsonl"

def day(ts): return datetime.fromtimestamp(ts, timezone.utc).strftime("%Y-%m-%d")
def week(ts): return datetime.fromtimestamp(ts, timezone.utc).strftime("%G-W%V")

slug_re = re.compile(r"^(.*?)-(\d{9,11})$")
def series_of(slug):
    m = slug_re.match(slug or "")
    if m: return m.group(1), int(m.group(2))
    return (slug or "?"), None

def pct(lst, q):
    if not lst: return float("nan")
    l = sorted(lst); return l[int(q * (len(l) - 1))]

rows_n = 0
types = Counter()
series_usdc = defaultdict(float); series_trades = Counter()
series_week_usdc = defaultdict(lambda: defaultdict(float))
day_trades = Counter(); day_usdc = defaultdict(float)
buy_prices, buy_usdc_sizes = [], []
buy_price_hist = Counter()
entry_offsets = defaultdict(list)  # series-class -> offsets
price_granularity = Counter()
rebate_total = 0.0; rebate_rows = 0
rebate_by_day = defaultdict(float)
first_ts = last_ts = None
mkts = {}

with open(PATH) as f:
    for line in f:
        r = json.loads(line)
        rows_n += 1
        ts = r["timestamp"]
        first_ts = ts if first_ts is None else min(first_ts, ts)
        last_ts = ts if last_ts is None else max(last_ts, ts)
        t = r.get("type"); types[t] += 1
        if t == "MAKER_REBATE":
            rebate_total += float(r.get("usdcSize") or 0); rebate_rows += 1
            rebate_by_day[day(ts)] += float(r.get("usdcSize") or 0)
            continue
        slug = r.get("slug") or "?"
        ser, mstart = series_of(slug)
        usdc = float(r.get("usdcSize") or 0)
        size = float(r.get("size") or 0)
        price = float(r.get("price") or 0)
        cid = r.get("conditionId"); oi = r.get("outcomeIndex")
        m = mkts.get(cid)
        if m is None:
            m = mkts[cid] = {"ser": ser, "mstart": mstart, "buy": [0.0, 0.0],
                             "buy_sh": [0.0, 0.0], "sell": [0.0, 0.0], "sell_sh": [0.0, 0.0],
                             "redeem": 0.0, "n": 0, "first": ts, "last": ts,
                             "fee_est": 0.0, "buys": []}
        m["n"] += 1
        m["last"] = max(m["last"], ts); m["first"] = min(m["first"], ts)
        if t == "TRADE":
            day_trades[day(ts)] += 1; day_usdc[day(ts)] += usdc
            series_usdc[ser] += usdc; series_trades[ser] += 1
            series_week_usdc[ser][week(ts)] += usdc
            m["fee_est"] += 0.07 * price * (1 - price) * size
            ps = f"{price:.6f}".rstrip("0").rstrip(".")
            price_granularity[min(len(ps.split(".")[1]) if "." in ps else 0, 6)] += 1
            if oi in (0, 1):
                if r.get("side") == "BUY":
                    m["buy"][oi] += usdc; m["buy_sh"][oi] += size
                    m["buys"].append((price, size, oi, ts))
                    buy_prices.append(price); buy_usdc_sizes.append(usdc)
                    buy_price_hist[round(price, 1)] += 1
                    if mstart:
                        entry_offsets[ser.rsplit("-", 1)[-1]].append(ts - mstart)
                else:
                    m["sell"][oi] += usdc; m["sell_sh"][oi] += size
        elif t == "REDEEM":
            m["redeem"] += usdc if usdc else size

print(f"rows={rows_n} span={day(first_ts)}..{day(last_ts)}")
print("types:", dict(types))
print(f"maker rebates: {rebate_rows} rows totalling ${rebate_total:,.0f}")
print()

print("== SERIES (by USDC buy+sell volume) ==")
tot_usdc = sum(series_usdc.values())
for ser, v in sorted(series_usdc.items(), key=lambda x: -x[1])[:15]:
    print(f"  {ser:24s} ${v:>13,.0f} ({100*v/tot_usdc:5.1f}%) trades={series_trades[ser]:>8}")
print(f"  TOTAL traded notional ${tot_usdc:,.0f}")
print()

print("== weekly USDC volume by series ==")
all_series = [s for s, _ in sorted(series_usdc.items(), key=lambda x: -x[1])[:8]]
weeks = sorted({w for s in all_series for w in series_week_usdc[s]})
print("week     " + "".join(f"{s.replace('-updown',''):>14}" for s in all_series))
for w in weeks:
    print(w + "" + "".join(f"{series_week_usdc[s].get(w,0):>14,.0f}" for s in all_series))
print()

print("== EXECUTION ==")
print(f"buys={len(buy_prices)} (sells: {sum(1 for m in mkts.values() if m['sell_sh'][0]+m['sell_sh'][1]>0)} mkts)")
print(f"buy price pct: p10={pct(buy_prices,.1):.2f} p25={pct(buy_prices,.25):.2f} p50={pct(buy_prices,.5):.2f} p75={pct(buy_prices,.75):.2f} p90={pct(buy_prices,.9):.2f}")
print(f"buy clip $: p10={pct(buy_usdc_sizes,.1):.2f} p25={pct(buy_usdc_sizes,.25):.2f} p50={pct(buy_usdc_sizes,.5):.2f} p75={pct(buy_usdc_sizes,.75):.2f} p90={pct(buy_usdc_sizes,.9):.2f} p99={pct(buy_usdc_sizes,.99):.2f} max={max(buy_usdc_sizes):.0f}")
nd = len(day_trades)
print(f"active days={nd} trades/day mean={sum(day_trades.values())/max(nd,1):.0f} max={max(day_trades.values())}")
print("buy price hist (0.1 buckets):", {k: buy_price_hist[k] for k in sorted(buy_price_hist)})
print("price decimals:", dict(sorted(price_granularity.items())))
for cls in sorted(entry_offsets):
    v = entry_offsets[cls]
    print(f"entry offset vs slug-ts [{cls}] n={len(v)}: p10={pct(v,.1):.0f}s p25={pct(v,.25):.0f} p50={pct(v,.5):.0f} p75={pct(v,.75):.0f} p90={pct(v,.9):.0f} p99={pct(v,.99):.0f}")
print()

# pairing + economics per market
both_sides = one_side = 0
paired_costs = []          # combined cost of paired share
imbalances = []            # |sh0-sh1| / max
pnl_real = defaultdict(float)        # series -> realized pnl
pnl_flip = defaultdict(float)        # pnl if winner flipped (directionality test)
ser_fee = defaultdict(float)
ser_week_pnl = defaultdict(lambda: defaultdict(float))
week_pnl = defaultdict(float); week_vol = defaultdict(float)
day_pnl = defaultdict(float)
bucket = defaultdict(lambda: [0, 0, 0.0, 0.0])  # n, wins, $staked, $pnl
tot_buy = tot_redeem = tot_sell = 0.0
unresolved_mkts = 0; resolved = 0
neutral_pnl_total = direct_pnl_total = 0.0

for cid, m in mkts.items():
    b0, b1 = m["buy_sh"][0], m["buy_sh"][1]
    if b0 + b1 <= 0: continue
    cost = m["buy"][0] + m["buy"][1]
    tot_buy += cost; tot_redeem += m["redeem"]; tot_sell += m["sell"][0] + m["sell"][1]
    if b0 > 0 and b1 > 0:
        both_sides += 1
        paired = min(b0, b1)
        c0 = m["buy"][0] / b0; c1 = m["buy"][1] / b1
        paired_costs.append(c0 + c1)
        imbalances.append(abs(b0 - b1) / max(b0, b1))
    else:
        one_side += 1
    pnl = m["sell"][0] + m["sell"][1] + m["redeem"] - cost
    w = week(m["last"])
    pnl_real[m["ser"]] += pnl; ser_fee[m["ser"]] += m["fee_est"]
    ser_week_pnl[m["ser"]][w] += pnl
    week_pnl[w] += pnl; week_vol[w] += cost
    day_pnl[day(m["last"])] += pnl
    # winner inference: redeem payout matches winning-side share count
    if m["redeem"] > 0:
        woi = 0 if abs(m["redeem"] - b0) <= abs(m["redeem"] - b1) else 1
        resolved += 1
        # flip test: payout if other side had won
        alt = (b1 if woi == 0 else b0)
        pnl_alt = m["sell"][0] + m["sell"][1] + alt - cost
        neutral_pnl_total += min(pnl, pnl_alt)
        direct_pnl_total += pnl - min(pnl, pnl_alt)
        pnl_flip[m["ser"]] += pnl_alt
        for price, size, oi, ts in m["buys"]:
            bk = round(price, 1); st = bucket[bk]
            st[0] += 1; st[2] += price * size
            if oi == woi: st[1] += 1; st[3] += (1 - price) * size
            else: st[3] -= price * size
    else:
        unresolved_mkts += 1

print("== PER-MARKET PATTERNS ==")
n_m = both_sides + one_side
print(f"markets traded={n_m} both-sides={both_sides} ({100*both_sides/n_m:.0f}%) one-side={one_side}")
print(f"resolved-with-redeem={resolved} unresolved(no redeem; incl lost-everything or open)={unresolved_mkts}")
print(f"paired combined cost (sum of avg buy px both sides): p10={pct(paired_costs,.1):.3f} p25={pct(paired_costs,.25):.3f} p50={pct(paired_costs,.5):.3f} p75={pct(paired_costs,.75):.3f} p90={pct(paired_costs,.9):.3f}")
print(f"share imbalance |b0-b1|/max: p10={pct(imbalances,.1):.2f} p50={pct(imbalances,.5):.2f} p90={pct(imbalances,.9):.2f}")
print()

print("== ECONOMICS ==")
fee_tot = sum(ser_fee.values())
cf = tot_sell + tot_redeem - tot_buy
print(f"buys=${tot_buy:,.0f} sells=${tot_sell:,.0f} redeems=${tot_redeem:,.0f}")
print(f"market cashflow P&L=${cf:,.0f}  + rebates ${rebate_total:,.0f} = ${cf+rebate_total:,.0f}")
print(f"hypothetical all-taker fee load (0.07*p*(1-p)*sh) = ${fee_tot:,.0f}")
print(f"neutral (min-of-both-outcomes) pnl=${neutral_pnl_total:,.0f}  directional residual=${direct_pnl_total:,.0f}")
print()
print("pnl by series (cashflow, excl rebates):")
for ser, v in sorted(pnl_real.items(), key=lambda x: -abs(x[1]))[:10]:
    print(f"  {ser:24s} ${v:>11,.0f}   flip-winner pnl ${pnl_flip[ser]:>11,.0f}   feeest ${ser_fee[ser]:>9,.0f}")
print()
print("weekly: pnl / buy-notional / pnl-per-$:")
for w in sorted(week_pnl):
    v = week_vol[w]
    print(f"  {w}  pnl ${week_pnl[w]:>9,.0f}  buys ${v:>12,.0f}  ret {100*week_pnl[w]/max(v,1):.2f}%")
print()
print("hit rate by buy-price bucket (resolved mkts, hold-to-expiry):")
print(f"{'bucket':>7} {'n':>8} {'hit%':>6} {'implied%':>9} {'$staked':>12} {'$pnl':>11} {'edge/$':>8}")
for b in sorted(bucket):
    n, wins, stk, pnl = bucket[b]
    if n == 0: continue
    print(f"{b:>7.1f} {n:>8} {100*wins/n:>5.1f}% {100*b:>8.0f}% {stk:>12,.0f} {pnl:>11,.0f} {100*pnl/max(stk,1):>7.1f}%")
print()

# capital usage timeline
flows = []
with open(PATH) as f:
    for line in f:
        r = json.loads(line)
        t = r.get("type")
        if t == "TRADE":
            amt = -float(r["usdcSize"]) if r.get("side") == "BUY" else float(r["usdcSize"])
        elif t in ("REDEEM", "MAKER_REBATE"):
            amt = float(r.get("usdcSize") or 0) or float(r.get("size") or 0)
        else: continue
        flows.append((r["timestamp"], amt))
flows.sort()
cash = 0.0; min_cash = 0.0; min_day = None
for ts, amt in flows:
    cash += amt
    if cash < min_cash: min_cash = cash; min_day = day(ts)
print(f"cum cashflow end=${cash:,.0f}; min cash=${min_cash:,.0f} on {min_day} (peak external capital needed, net of accrued pnl)")
print(f"total buy notional=${tot_buy:,.0f}; recycle vs peak capital = {tot_buy/max(abs(min_cash),1):,.0f}x")
print()
print("daily: trades / buy+sell vol / mkt pnl / rebates:")
for d in sorted(day_trades):
    print(f"  {d}  {day_trades[d]:>6}  ${day_usdc[d]:>11,.0f}  ${day_pnl.get(d,0):>9,.0f}  ${rebate_by_day.get(d,0):>7,.0f}")
