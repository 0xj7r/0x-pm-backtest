#!/usr/bin/env python3
"""Decompose wallet 0x2855 activity: universe, execution style, economics.

Archetype hypothesis: late-window pair accumulator on btc-updown-5m
(buy favourite ~0.95+ and underdog ~0.01-0.05 in the final minutes,
hold both to expiry, redeem winning leg at $1).
"""
import json
import re
import statistics as st
import time
from collections import defaultdict

PATH = "/Users/jackreid/go/polymarket-backtest/data/external/whale_2855/activity.jsonl"


def family(slug):
    if not slug:
        return "?"
    m = re.match(r"^(.*?)-(\d{9,})$", slug)
    if m:
        return m.group(1)
    return re.sub(r"-(january|february|march|april|may|june|july)-.*$", "-<date>", slug)


def pct(xs, q):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(q * len(xs)))]


def main():
    keep = ("timestamp", "conditionId", "type", "size", "usdcSize", "price",
            "asset", "side", "slug", "outcome")
    rows = []
    for l in open(PATH):
        r = json.loads(l)
        rows.append({k: r.get(k) for k in keep})
    rows.sort(key=lambda r: r["timestamp"])
    print(f"total rows: {len(rows)}")
    t0, t1 = rows[0]["timestamp"], rows[-1]["timestamp"]
    print(f"span: {time.strftime('%Y-%m-%d %H:%M', time.gmtime(t0))} -> "
          f"{time.strftime('%Y-%m-%d %H:%M', time.gmtime(t1))} UTC")

    cnt = defaultdict(lambda: [0, 0.0])
    for r in rows:
        k = r["type"] + ("/" + r.get("side", "") if r["type"] == "TRADE" else "")
        cnt[k][0] += 1
        cnt[k][1] += float(r.get("usdcSize", 0))
    print("\n== type/side counts ==")
    for k, (n, usd) in sorted(cnt.items()):
        print(f"  {k:14s} n={n:7d}  usd={usd:12.0f}")
    rebates = cnt.get("MAKER_REBATE", [0, 0.0])[1]

    fam = defaultdict(lambda: [0, 0.0, set()])
    for r in rows:
        if r["type"] != "TRADE":
            continue
        f = family(r.get("slug"))
        fam[f][0] += 1
        fam[f][1] += float(r.get("usdcSize", 0))
        fam[f][2].add(r.get("conditionId"))
    print("\n== universe (TRADE volume by family) ==")
    tot = sum(v[1] for v in fam.values())
    for f, (n, usd, mkts) in sorted(fam.items(), key=lambda kv: -kv[1][1])[:15]:
        print(f"  {f:40s} trades={n:7d} usd={usd:11.0f} ({usd/tot:5.1%}) mkts={len(mkts)}")

    print("\n== monthly trade volume ==")
    mon = defaultdict(lambda: [0, 0.0, 0.0, set()])
    for r in rows:
        if r["type"] != "TRADE":
            continue
        m = time.strftime("%Y-%m", time.gmtime(r["timestamp"]))
        mon[m][0] += 1
        u = float(r["usdcSize"])
        if r["side"] == "BUY":
            mon[m][1] += u
        else:
            mon[m][2] += u
        mon[m][3].add(time.strftime("%Y-%m-%d", time.gmtime(r["timestamp"])))
    for m, (n, b, s, days) in sorted(mon.items()):
        print(f"  {m}: trades={n:6d} buy_usd={b:10.0f} sell_usd={s:10.0f} "
              f"active_days={len(days)} trades/day={n/max(1,len(days)):.0f}")

    buy_usd = [float(r["usdcSize"]) for r in rows if r["type"] == "TRADE" and r["side"] == "BUY"]
    buy_px = [float(r["price"]) for r in rows if r["type"] == "TRADE" and r["side"] == "BUY"]
    sell_px = [float(r["price"]) for r in rows if r["type"] == "TRADE" and r["side"] == "SELL"]
    sell_usd = [float(r["usdcSize"]) for r in rows if r["type"] == "TRADE" and r["side"] == "SELL"]
    print("\n== clip sizes (USD per fill) ==")
    for name, xs in [("BUY", buy_usd), ("SELL", sell_usd)]:
        if not xs:
            continue
        print(f"  {name}: p10={pct(xs,.1):.1f} p25={pct(xs,.25):.1f} med={pct(xs,.5):.1f} "
              f"p75={pct(xs,.75):.1f} p90={pct(xs,.9):.1f} p99={pct(xs,.99):.0f} max={max(xs):.0f}")
    sh = [float(r["size"]) for r in rows if r["type"] == "TRADE" and r["side"] == "BUY"]
    print(f"  BUY shares/fill: med={st.median(sh):.0f} p90={pct(sh,.9):.0f} max={max(sh):.0f}")

    print("\n== price distribution ==")
    buckets = [(0, .05), (.05, .15), (.15, .3), (.3, .45), (.45, .55), (.55, .7),
               (.7, .85), (.85, .95), (.95, 1.01)]
    for name, xs in [("BUY", buy_px), ("SELL", sell_px)]:
        if not xs:
            continue
        line = "  " + name + " (fills): "
        for lo, hi in buckets:
            share = sum(1 for x in xs if lo <= x < hi) / len(xs)
            if share >= 0.001:
                line += f"[{lo:.2f}-{hi:.2f}):{share:5.1%} "
        print(line)
        print(f"    median={st.median(xs):.3f} mean={st.mean(xs):.3f}")
    bw = defaultdict(float)
    for r in rows:
        if r["type"] == "TRADE" and r["side"] == "BUY":
            p = float(r["price"])
            for lo, hi in buckets:
                if lo <= p < hi:
                    bw[(lo, hi)] += float(r["usdcSize"])
    totb = sum(bw.values())
    print("  BUY usd-weighted: " + " ".join(
        f"[{lo:.2f}-{hi:.2f}):{bw[(lo,hi)]/totb:5.1%}"
        for lo, hi in buckets if bw[(lo, hi)] / totb >= 0.001))

    mkts = defaultdict(list)
    for r in rows:
        if r.get("conditionId"):
            mkts[r["conditionId"]].append(r)

    n_both_sides = n_mkt = 0
    n_redeem_mkt = n_sell_exit_mkt = 0
    fills_per_pos = []
    entry_offsets, entry_offsets_w = [], []
    recycle_events = 0
    pnl_fam_mon = defaultdict(float)
    pnl_mkt = {}
    open_flow = []
    fee_est = 0.0
    hold_secs = []
    pair_stats = []   # (paired_shares, pair_cost_per_share, excess_shares, excess_won, mkt_pnl)
    leg_vwaps = []    # (cheap_leg_vwap, exp_leg_vwap)
    locked_pnl = directional_pnl = 0.0
    n_winner_known = n_excess_won = n_excess = 0
    daily_pnl = defaultdict(float)
    cls = defaultdict(lambda: [0, 0.0, []])  # fav_won/dog_won/unknown -> [n, pnl, pnls]
    dog_fav_ratio = []

    for cid, fs in mkts.items():
        fs.sort(key=lambda r: r["timestamp"])
        slug = fs[0].get("slug", "")
        f = family(slug)
        m = re.match(r"^.*-(\d{9,})$", slug or "")
        wstart = int(m.group(1)) if m else None
        is5m = "updown-5m" in (slug or "")
        n_mkt += 1
        buys = defaultdict(lambda: [0.0, 0.0])
        sells = defaultdict(lambda: [0.0, 0.0])
        redeem_usd = 0.0
        first_buy = {}
        cash = 0.0
        for r in fs:
            ts = r["timestamp"]
            u = float(r.get("usdcSize", 0))
            a = r.get("asset")
            if r["type"] == "TRADE":
                shr = float(r["size"])
                p = float(r["price"])
                fee_est += 0.07 * p * (1 - p) * shr
                if r["side"] == "BUY":
                    buys[a][0] += shr
                    buys[a][1] += u
                    cash -= u
                    open_flow.append((ts, u))
                    first_buy.setdefault(a, ts)
                    if wstart and is5m:
                        entry_offsets.append(ts - wstart)
                        entry_offsets_w.append((ts - wstart, u))
                else:
                    sells[a][0] += shr
                    sells[a][1] += u
                    cash += u
                    open_flow.append((ts, -u))
                    if a in first_buy:
                        hold_secs.append(ts - first_buy[a])
            elif r["type"] == "REDEEM":
                redeem_usd += u
                cash += u
                open_flow.append((ts, -u))
            elif r["type"] == "SPLIT":
                cash -= u
                open_flow.append((ts, u))
            elif r["type"] in ("MERGE", "CONVERSION"):
                cash += u
                open_flow.append((ts, -u))
        bought = [a for a, b in buys.items() if b[0] > 0.01]
        if len(bought) >= 2:
            n_both_sides += 1
        if redeem_usd > 0:
            n_redeem_mkt += 1
        elif any(s[0] > 0.01 for s in sells.values()):
            n_sell_exit_mkt += 1
        for a in bought:
            nb = sum(1 for r in fs if r["type"] == "TRADE" and r["side"] == "BUY"
                     and r["asset"] == a)
            fills_per_pos.append(nb)
        trades = [r for r in fs if r["type"] == "TRADE"]
        for i, r in enumerate(trades):
            if r["side"] == "SELL" and float(r["price"]) >= 0.97:
                for r2 in trades[i + 1:]:
                    if r2["timestamp"] - r["timestamp"] > 60:
                        break
                    if (r2["side"] == "BUY" and r2["asset"] != r["asset"]
                            and float(r2["price"]) <= 0.03):
                        recycle_events += 1
                        break

        # pair decomposition (only when exactly 2 assets bought)
        if len(bought) == 2:
            a1, a2 = bought
            net1 = buys[a1][0] - sells[a1][0]
            net2 = buys[a2][0] - sells[a2][0]
            v1 = buys[a1][1] / buys[a1][0]
            v2 = buys[a2][1] / buys[a2][0]
            cheap, exp = (v1, v2) if v1 < v2 else (v2, v1)
            leg_vwaps.append((cheap, exp))
            paired = min(net1, net2)
            excess = abs(net1 - net2)
            excess_asset = a1 if net1 > net2 else a2
            excess_vwap = v1 if net1 > net2 else v2
            # winner inference from redeem payout: payout = winning shares held
            winner = None
            if redeem_usd > 0.01:
                if abs(redeem_usd - max(net1, net2)) < 0.02 * max(net1, net2) + 2:
                    winner = a1 if net1 > net2 else a2
                elif abs(redeem_usd - min(net1, net2)) < 0.02 * max(net1, net2) + 2:
                    winner = a2 if net1 > net2 else a1
            if paired > 0:
                mon_k = time.strftime("%Y-%m", time.gmtime(fs[0]["timestamp"]))
                cps = v1 + v2
                locked_pnl += paired * (1 - cps)
                pair_stats.append((paired, cps, excess, mon_k, cash))
            if winner is not None:
                n_winner_known += 1
                if excess > 0.5:
                    n_excess += 1
                    won = (excess_asset == winner)
                    if won:
                        n_excess_won += 1
                        directional_pnl += excess * (1 - excess_vwap)
                    else:
                        directional_pnl -= excess * excess_vwap
            # fav/dog classification: fav = higher-vwap leg
            fav_a = a1 if v1 > v2 else a2
            if buys[a1][0] > 0 and buys[a2][0] > 0:
                dog_sh = buys[a2][0] if fav_a == a1 else buys[a1][0]
                fav_sh = buys[a1][0] if fav_a == a1 else buys[a2][0]
                dog_fav_ratio.append(dog_sh / fav_sh)
            ckey = ("fav_won" if winner == fav_a else
                    "dog_won" if winner is not None else "unknown")
            cls[ckey][0] += 1
            cls[ckey][1] += cash
            cls[ckey][2].append(cash)

        mon_key = time.strftime("%Y-%m", time.gmtime(fs[0]["timestamp"]))
        pnl_fam_mon[(f, mon_key)] += cash
        daily_pnl[time.strftime("%Y-%m-%d", time.gmtime(fs[0]["timestamp"]))] += cash
        pnl_mkt[cid] = (cash, f, slug)

    print(f"\n== per-market structure ({n_mkt} markets) ==")
    print(f"  bought BOTH outcomes: {n_both_sides} ({n_both_sides/n_mkt:.1%})")
    print(f"  has REDEEM (hold-to-expiry): {n_redeem_mkt} ({n_redeem_mkt/n_mkt:.1%})")
    print(f"  exited via SELL only: {n_sell_exit_mkt} ({n_sell_exit_mkt/n_mkt:.1%})")
    print(f"  buy fills per position: med={st.median(fills_per_pos):.0f} "
          f"p90={pct(fills_per_pos,.9)} max={max(fills_per_pos)}")
    print(f"  99c-sell + 1c-opposite-buy recycle events: {recycle_events}")
    if hold_secs:
        print(f"  buy->sell hold: med={st.median(hold_secs):.0f}s p90={pct(hold_secs,.9):.0f}s n={len(hold_secs)}")
    if entry_offsets:
        print(f"  5m-window buy-fill offset: p10={pct(entry_offsets,.1):.0f}s "
              f"med={st.median(entry_offsets):.0f}s p90={pct(entry_offsets,.9):.0f}s (window=300s)")
        for lo, hi, lbl in [(-1e9, 0, "pre-window"), (0, 60, "0-60s"),
                            (60, 180, "60-180s"), (180, 240, "180-240s"),
                            (240, 300, "240-300s"), (300, 1e9, "post-close")]:
            n = sum(1 for x in entry_offsets if lo <= x < hi)
            w = sum(u for x, u in entry_offsets_w if lo <= x < hi)
            wtot = sum(u for _, u in entry_offsets_w)
            print(f"    {lbl:11s}: fills {n/len(entry_offsets):5.1%}  usd {w/wtot:5.1%}")

    print("\n== pair economics ==")
    if leg_vwaps:
        ch = [c for c, _ in leg_vwaps]
        ex = [e for _, e in leg_vwaps]
        print(f"  cheap-leg vwap: med={st.median(ch):.3f} p10={pct(ch,.1):.3f} p90={pct(ch,.9):.3f}")
        print(f"  expensive-leg vwap: med={st.median(ex):.3f} p10={pct(ex,.1):.3f} p90={pct(ex,.9):.3f}")
    if pair_stats:
        cps = [c for _, c, _, _, _ in pair_stats]
        paired = [p for p, _, _, _, _ in pair_stats]
        exc = [e for _, _, e, _, _ in pair_stats]
        print(f"  combined leg vwap (pair cost/share): med={st.median(cps):.3f} "
              f"p10={pct(cps,.1):.3f} p90={pct(cps,.9):.3f} "
              f"share<1.00: {sum(1 for c in cps if c<1)/len(cps):.0%} "
              f"share<0.99: {sum(1 for c in cps if c<0.99)/len(cps):.0%}")
        print(f"  paired shares/mkt: med={st.median(paired):.0f} p90={pct(paired,.9):.0f}")
        print(f"  unpaired excess shares/mkt: med={st.median(exc):.0f} p90={pct(exc,.9):.0f}")
        print(f"  dog/fav share ratio: med={st.median(dog_fav_ratio):.2f} "
              f"p90={pct(dog_fav_ratio,.9):.2f}")
        print(f"  locked (pair) P&L est: {locked_pnl:.0f}")
        print(f"  directional (excess leg) P&L est: {directional_pnl:.0f} "
              f"(excess-leg hit rate {n_excess_won}/{n_excess}"
              f"={n_excess_won/max(1,n_excess):.1%}, winner known in {n_winner_known} mkts)")
    print("\n== market outcome classes (both-sides mkts, actual cash P&L) ==")
    for k in ("fav_won", "dog_won", "unknown"):
        n, p, ps = cls[k]
        if not n:
            continue
        print(f"  {k:8s}: n={n:5d} ({n/max(1,sum(c[0] for c in cls.values())):5.1%}) "
              f"total={p:8.0f} avg={p/n:7.1f} med={st.median(ps):7.1f} "
              f"p10={pct(ps,.1):7.1f} p90={pct(ps,.9):7.1f}")
    print("\n== monthly pair-cost trend ==")
    by_m = defaultdict(list)
    for paired_sh, c, e, mk, csh in pair_stats:
        by_m[mk].append((c, paired_sh, csh))
    for mk in sorted(by_m):
        cs = [c for c, _, _ in by_m[mk]]
        pnls = [x for _, _, x in by_m[mk]]
        print(f"  {mk}: mkts={len(cs):5d} combined-cost med={st.median(cs):.4f} "
              f"p90={pct(cs,.9):.4f} avg_mkt_pnl={st.mean(pnls):6.1f} "
              f"med_mkt_pnl={st.median(pnls):6.1f}")

    print("\n== realized P&L per family per month (cash flow, excl rebates) ==")
    fams = sorted({k[0] for k in pnl_fam_mon}, key=lambda f: -sum(
        v for (ff, _), v in pnl_fam_mon.items() if ff == f))
    mons = sorted({k[1] for k in pnl_fam_mon})
    print("  " + "family".ljust(36) + "".join(m.rjust(10) for m in mons) + "     total")
    for f in fams[:12]:
        vals = [pnl_fam_mon.get((f, m), 0.0) for m in mons]
        print("  " + f.ljust(36) + "".join(f"{v:10.0f}" for v in vals)
              + f"{sum(vals):10.0f}")
    print("  " + "TOTAL".ljust(36) + "".join(
        f"{sum(pnl_fam_mon.get((f, m), 0.0) for f in fams):10.0f}" for m in mons)
        + f"{sum(pnl_fam_mon.values()):10.0f}")
    print(f"  + maker rebates: {rebates:.0f}")

    dp = sorted(daily_pnl.items())
    green = sum(1 for _, v in dp if v > 0)
    print(f"\n== daily P&L ==  {green}/{len(dp)} days green; "
          f"med={st.median([v for _,v in dp]):.0f} worst={min(v for _,v in dp):.0f} "
          f"best={max(v for _,v in dp):.0f}")
    wk = defaultdict(float)
    for d, v in dp:
        t = time.strptime(d, "%Y-%m-%d")
        wk[time.strftime("%Y-W%W", t)] += v
    print("  weekly: " + "  ".join(f"{w}:{v:.0f}" for w, v in sorted(wk.items())))
    hod = defaultdict(int)
    for r in rows:
        if r["type"] == "TRADE":
            hod[time.gmtime(r["timestamp"]).tm_hour] += 1
    tot_h = sum(hod.values())
    print("  trade share by UTC hour: " + " ".join(
        f"{h:02d}:{hod.get(h,0)/tot_h:.1%}" for h in range(24)))

    print(f"\n== fees ==")
    print(f"  est fee if ALL fills taker (0.07*p*(1-p)/share): {fee_est:.0f} USD")
    print(f"  maker rebates actually received: {rebates:.0f} USD")

    open_flow.sort()
    cum = peak = 0.0
    for ts, d in open_flow:
        cum += d
        peak = max(peak, cum)
    tot_buy = sum(buy_usd)
    tot_pnl = sum(pnl_fam_mon.values())
    print(f"\n== capital ==")
    print(f"  total buy turnover: {tot_buy:.0f} USD; total P&L (excl rebates): {tot_pnl:.0f}")
    print(f"  peak open exposure (cumulative cost basis): {peak:.0f} USD")
    print(f"  P&L per $ turnover: {tot_pnl/tot_buy:.4f}; per $ peak capital: {tot_pnl/peak:.2f}")

    print("\n== top 5 / bottom 5 markets by P&L ==")
    ranked = sorted(pnl_mkt.items(), key=lambda kv: -kv[1][0])
    for cid, (p, f, slug) in ranked[:5] + ranked[-5:]:
        print(f"  {p:9.0f}  {slug}")


if __name__ == "__main__":
    main()
