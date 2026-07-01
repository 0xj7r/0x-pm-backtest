#!/usr/bin/env python3
"""ToD drawdown decomposition + structural sigma-floor overlay for the BTC-5m fade.

Two legitimate, non-overfit analyses distinct from the rejected F2 P&L-cell gate:

1. DESCRIPTIVE: per-UTC-hour and per-day-of-week, pooled W1+W2+W3, report mean
   pnl/trade, total NET, share of total drawdown, and trade count. A structural
   pattern repeats across all three windows; an overfit one does not. Quantify the
   overnight dead-hours effect.

2. STRUCTURAL OVERLAY: gate on sigma_bar_bps (the live vol/activity proxy that is
   low in dead hours), NOT the calendar. Sweep a min-sigma floor; fit on W3, freeze
   on W1/W2; report NET, daily Sharpe, max drawdown, entries removed, and the
   drawdown-per-dollar-of-NET-given-up.

3. CROSS-CHECK: how much of the bad-hours P&L is explained by low sigma alone.

Read-only over existing trades files. Never runs pm-app.
"""
import json
import math
import datetime as dt
from collections import defaultdict

DIR = "/Users/jackreid/go/polymarket-backtest/data/runs/alpha/feemin"
TESTLOOK = "/Users/jackreid/go/polymarket-backtest/data/runs/alpha/testlook0612/hold012_test.trades.jsonl"
# Sealed live window: never fit on or pool any trade in this range (testlook excepted, descriptive only).
SEAL_LO = int(dt.datetime(2026, 5, 19, tzinfo=dt.timezone.utc).timestamp() * 1e9)
SEAL_HI = int(dt.datetime(2026, 6, 30, tzinfo=dt.timezone.utc).timestamp() * 1e9)

# Fit/validate windows: base feemin config (W3) and its OOS siblings W1/W2.
FILES = {
    "W3": f"{DIR}/base.trades.jsonl",
    "W1": f"{DIR}/W1_base.trades.jsonl",
    "W2": f"{DIR}/W2_base.trades.jsonl",
}
DOW = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]


def load(path, allow_sealed=False):
    out = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if not line:
                continue
            d = json.loads(line)
            if d.get("window_secs") != 300:
                continue
            ts = d["decision_ts_ns"]
            if not allow_sealed and SEAL_LO <= ts <= SEAL_HI:
                raise AssertionError(f"trade inside sealed window: {ts}")
            t = dt.datetime.fromtimestamp(ts / 1e9, dt.timezone.utc)
            out.append({
                "pnl": d["pnl"],
                "fee": d.get("fee", 0.0),
                "won": d.get("won"),
                "sigma": d.get("sigma_bar_bps"),
                "hour": t.hour,
                "dow": t.weekday(),
                "date": t.date(),
            })
    return out


def max_drawdown(pnls):
    """Trade-sequence max drawdown of the cumulative pnl curve (in $)."""
    peak = 0.0
    cum = 0.0
    mdd = 0.0
    for p in pnls:
        cum += p
        peak = max(peak, cum)
        mdd = max(mdd, peak - cum)
    return mdd


def trade_drawdown_share(trades, key):
    """Share of total drawdown attributable to each cell.

    Drawdown is path-dependent and not additively decomposable, so we use the
    well-defined proxy: each cell's negative-pnl mass (sum of losing trade pnl),
    as a fraction of total negative-pnl mass. This is the 'where do the losses
    live' decomposition that drawdown share is meant to capture.
    """
    cell_neg = defaultdict(float)
    total_neg = 0.0
    for t in trades:
        if t["pnl"] < 0:
            cell_neg[key(t)] += t["pnl"]
            total_neg += t["pnl"]
    return {k: (v / total_neg if total_neg else 0.0) for k, v in cell_neg.items()}


def daily_sharpe(trades):
    daily = defaultdict(float)
    for t in trades:
        daily[t["date"]] += t["pnl"]
    if not daily:
        return float("nan")
    vals = list(daily.values())
    mean = sum(vals) / len(vals)
    if len(vals) < 2:
        return float("nan")
    var = sum((v - mean) ** 2 for v in vals) / (len(vals) - 1)
    return mean / math.sqrt(var) if var > 0 else float("nan")


def cell_table(trades, key, labels=None):
    agg = defaultdict(lambda: [0, 0.0])
    for t in trades:
        agg[key(t)][0] += 1
        agg[key(t)][1] += t["pnl"]
    dd = trade_drawdown_share(trades, key)
    rows = []
    for k in sorted(agg):
        n, p = agg[k]
        rows.append((k, n, p, p / n if n else 0.0, dd.get(k, 0.0)))
    return rows


def main():
    W = {w: load(p) for w, p in FILES.items()}
    pooled = W["W1"] + W["W2"] + W["W3"]
    testlook = load(TESTLOOK, allow_sealed=True)

    print("# data counts")
    for w in ["W1", "W2", "W3"]:
        print(f"  {w}: n={len(W[w])}  NET={sum(t['pnl'] for t in W[w]):.1f}")
    print(f"  pooled: n={len(pooled)}  NET={sum(t['pnl'] for t in pooled):.1f}")
    print(f"  testlook(sealed,descriptive only): n={len(testlook)}  NET={sum(t['pnl'] for t in testlook):.1f}")

    # ---- 1. DESCRIPTIVE: hour-of-day pooled, plus per-window sign for consistency ----
    print("\n# 1a. HOUR-OF-DAY (UTC), pooled W1+W2+W3")
    print("hour |    n |      NET | pnl/trd | dd_share | W1/W2/W3 pnl/trd  | sign-consistent-neg")
    HKEY = lambda t: t["hour"]
    pooled_rows = {r[0]: r for r in cell_table(pooled, HKEY)}
    per_win = {w: {r[0]: r for r in cell_table(W[w], HKEY)} for w in ["W1", "W2", "W3"]}
    struct_neg_hours = []
    for h in range(24):
        _, n, p, ppt, dds = pooled_rows.get(h, (h, 0, 0.0, 0.0, 0.0))
        wv = []
        for w in ["W1", "W2", "W3"]:
            r = per_win[w].get(h)
            wv.append(r[3] if r else float("nan"))
        all_neg = all((not math.isnan(x)) and x < 0 for x in wv)
        if all_neg:
            struct_neg_hours.append(h)
        print(f"  {h:02d} | {n:4d} | {p:8.1f} | {ppt:7.2f} | {dds:7.1%} | "
              f"{wv[0]:6.2f}/{wv[1]:6.2f}/{wv[2]:6.2f} | {'YES' if all_neg else ''}")
    print(f"\nstructurally-negative hours (neg in ALL of W1,W2,W3): "
          f"{struct_neg_hours if struct_neg_hours else 'NONE'}")

    # overnight dead-hours block (UTC 02-09 = Americas overnight / pre-Europe)
    night = list(range(2, 10))
    day = [h for h in range(24) if h not in night]
    nt = [t for t in pooled if t["hour"] in night]
    dt_ = [t for t in pooled if t["hour"] in day]
    print(f"\n# 1b. overnight block UTC {night[0]:02d}-{night[-1]:02d} vs rest (pooled)")
    for lbl, grp in [("overnight 02-09", nt), ("daytime", dt_)]:
        net = sum(t["pnl"] for t in grp)
        feenet = sum(t["fee"] for t in grp)
        ddsh = sum(t["pnl"] for t in grp if t["pnl"] < 0)
        totneg = sum(t["pnl"] for t in pooled if t["pnl"] < 0)
        wr = sum(1 for t in grp if t["won"]) / len(grp) if grp else 0
        msig = sum(t["sigma"] for t in grp) / len(grp) if grp else 0
        print(f"  {lbl:16s}: n={len(grp):5d}  NET={net:9.1f}  ppt={net/len(grp):6.2f}  "
              f"fees={feenet:8.1f}  dd_share={ddsh/totneg:5.1%}  win%={wr:5.1%}  mean_sigma={msig:5.2f}")

    # ---- 1c. DAY-OF-WEEK pooled + per-window consistency ----
    print("\n# 1c. DAY-OF-WEEK, pooled W1+W2+W3")
    print(" dow |    n |      NET | pnl/trd | dd_share | W1/W2/W3 pnl/trd")
    DKEY = lambda t: t["dow"]
    prows = {r[0]: r for r in cell_table(pooled, DKEY)}
    pw = {w: {r[0]: r for r in cell_table(W[w], DKEY)} for w in ["W1", "W2", "W3"]}
    for d in range(7):
        _, n, p, ppt, dds = prows.get(d, (d, 0, 0.0, 0.0, 0.0))
        wv = [pw[w].get(d, (0, 0, 0, float("nan")))[3] for w in ["W1", "W2", "W3"]]
        print(f" {DOW[d]} | {n:4d} | {p:8.1f} | {ppt:7.2f} | {dds:7.1%} | "
              f"{wv[0]:6.2f}/{wv[1]:6.2f}/{wv[2]:6.2f}")

    # testlook descriptive ToD (sealed window, NOT for fitting)
    print("\n# 1d. testlook (sealed live window, hold012) hour-of-day, DESCRIPTIVE ONLY")
    print("hour |    n |      NET | pnl/trd | mean_sigma")
    tl_rows = {r[0]: r for r in cell_table(testlook, HKEY)}
    tl_sig = defaultdict(list)
    for t in testlook:
        tl_sig[t["hour"]].append(t["sigma"])
    for h in range(24):
        r = tl_rows.get(h)
        if not r:
            continue
        ms = sum(tl_sig[h]) / len(tl_sig[h]) if tl_sig[h] else 0
        print(f"  {h:02d} | {r[1]:4d} | {r[2]:8.1f} | {r[3]:7.2f} | {ms:6.2f}")

    # ---- 2. STRUCTURAL OVERLAY: sigma_bar_bps floor, fit W3, freeze W1/W2 ----
    print("\n# 2. SIGMA-FLOOR OVERLAY (gate sigma_bar_bps >= floor). Fit W3, freeze W1/W2.")
    floors = [0.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
    base = {w: {
        "net": sum(t["pnl"] for t in W[w]),
        "sharpe": daily_sharpe(W[w]),
        "mdd": max_drawdown([t["pnl"] for t in sorted(W[w], key=lambda x: (x["date"], x["hour"]))]),
        "n": len(W[w]),
    } for w in ["W1", "W2", "W3"]}
    for w in ["W3", "W1", "W2"]:
        print(f"\n-- {w} (base NET={base[w]['net']:.1f} Sharpe={base[w]['sharpe']:.3f} "
              f"MDD={base[w]['mdd']:.1f} n={base[w]['n']}) --")
        print("floor | n_kept | n_rm | rm_pnl |     NET | dNET | Sharpe |   MDD | dMDD | dd_saved/$NET_given_up")
        for f in floors:
            kept = [t for t in W[w] if t["sigma"] is not None and t["sigma"] >= f]
            removed = [t for t in W[w] if not (t["sigma"] is not None and t["sigma"] >= f)]
            net = sum(t["pnl"] for t in kept)
            rmpnl = sum(t["pnl"] for t in removed)
            sh = daily_sharpe(kept)
            seq = [t["pnl"] for t in sorted(kept, key=lambda x: (x["date"], x["hour"]))]
            mdd = max_drawdown(seq)
            dnet = net - base[w]["net"]
            dmdd = mdd - base[w]["mdd"]
            # ratio: dollars of drawdown removed per dollar of NET given up (want >> 1)
            net_given_up = base[w]["net"] - net
            dd_saved = base[w]["mdd"] - mdd
            ratio = dd_saved / net_given_up if net_given_up > 1e-9 else float("inf")
            print(f" {f:4.1f} | {len(kept):6d} | {len(removed):4d} | {rmpnl:6.1f} | {net:7.1f} | "
                  f"{dnet:5.1f} | {sh:6.3f} | {mdd:5.1f} | {dmdd:5.1f} | {ratio:7.2f}")

    # ---- 3. CROSS-CHECK: is ToD just a proxy for sigma? ----
    print("\n# 3. CROSS-CHECK: low-sigma mass vs bad-hours mass (pooled)")
    # mean sigma by hour
    sig_by_hour = defaultdict(list)
    for t in pooled:
        sig_by_hour[t["hour"]].append(t["sigma"])
    print("hour | mean_sigma | %trades<3bps | %trades<4bps | pnl/trd")
    for h in range(24):
        ss = sig_by_hour[h]
        if not ss:
            continue
        msig = sum(ss) / len(ss)
        lo3 = sum(1 for x in ss if x < 3) / len(ss)
        lo4 = sum(1 for x in ss if x < 4) / len(ss)
        ppt = pooled_rows.get(h, (h, 1, 0, 0))[3]
        print(f"  {h:02d} | {msig:9.2f} | {lo3:11.1%} | {lo4:11.1%} | {ppt:6.2f}")

    # Decompose overnight loss: split overnight trades by sigma floor 3bps.
    print("\n# 3b. does a 3bps floor remove the overnight losses, pooled?")
    for f in [3.0, 4.0]:
        on_lo = [t for t in nt if t["sigma"] < f]
        on_hi = [t for t in nt if t["sigma"] >= f]
        day_lo = [t for t in dt_ if t["sigma"] < f]
        print(f"  floor {f:.0f}bps:")
        print(f"    overnight & sigma<{f:.0f}: n={len(on_lo):5d} NET={sum(t['pnl'] for t in on_lo):8.1f} "
              f"ppt={sum(t['pnl'] for t in on_lo)/len(on_lo) if on_lo else 0:6.2f}")
        print(f"    overnight & sigma>={f:.0f}: n={len(on_hi):5d} NET={sum(t['pnl'] for t in on_hi):8.1f} "
              f"ppt={sum(t['pnl'] for t in on_hi)/len(on_hi) if on_hi else 0:6.2f}")
        print(f"    daytime  & sigma<{f:.0f}: n={len(day_lo):5d} NET={sum(t['pnl'] for t in day_lo):8.1f} "
              f"ppt={sum(t['pnl'] for t in day_lo)/len(day_lo) if day_lo else 0:6.2f}  "
              f"(low-sigma loss exists in daytime too => sigma not pure ToD)")


if __name__ == "__main__":
    main()
