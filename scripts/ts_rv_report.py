#!/usr/bin/env python3
"""Summarize ts_rv_measure.py events: opportunity table, persistence, P&L,
and daily P&L correlation with a baseline fade run."""

import argparse
import datetime as dt
import json
import os
from collections import Counter, defaultdict

import numpy as np

DATA_ROOT = os.environ.get("PM_DATA_ROOT", "/Users/jackreid/go/polymarket-backtest")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--events", default=os.path.join(DATA_ROOT, "data/runs/ts_rv/events_binance.jsonl"))
    ap.add_argument("--baseline", default=os.path.join(
        DATA_ROOT, "data/runs/alpha/deadline/verify_sbc90_test.trades.jsonl"))
    args = ap.parse_args()

    ev = [json.loads(l) for l in open(args.events)]
    tr = [e for e in ev if e.get("traded")]
    print(f"events(onsets>=4c)={len(ev)}  unique-pair trades={len(tr)}")
    if not tr:
        return

    days = sorted({e["date"] for e in ev})

    def pct(a, q):
        return np.percentile(a, q).round(3)

    for name, arr in [
        ("comb_edge", [e["comb_edge"] for e in tr]),
        ("edge5", [e["edge5"] for e in tr]),
        ("edge15", [e["edge15"] for e in tr]),
        ("p5", [e["p5"] for e in tr]),
        ("p15", [e["p15"] for e in tr]),
        ("ask5", [e["ask5"] for e in tr]),
        ("ask15", [e["ask15"] for e in tr]),
        ("rem5", [e["rem5"] for e in tr]),
        ("rem15", [e["rem15"] for e in tr]),
        ("persist_s", [e["persist_s"] for e in tr]),
        ("depth5_usd", [e["depth5_sh"] * e["ask5"] for e in tr]),
        ("depth15_usd", [e["depth15_sh"] * e["ask15"] for e in tr]),
    ]:
        a = np.array(arr, dtype=float)
        print(f"{name:12s} p5={pct(a,5)} p25={pct(a,25)} med={pct(a,50)} p75={pct(a,75)} p95={pct(a,95)}")

    print("directions:", Counter((e["d5"], e["d15"]) for e in tr))
    pers = np.array([e["persist_s"] for e in ev], dtype=float)
    print(f"persistence all onsets: >=1s {(pers>=1).mean():.2f}  >=3s {(pers>=3).mean():.2f}"
          f"  >=5s {(pers>=5).mean():.2f}  >=10s {(pers>=10).mean():.2f}  >=30s {(pers>=30).mean():.2f}")

    pnl = np.array([e["pnl"] for e in tr], dtype=float)
    print(f"\ntrade pnl ($50/leg, hold-to-resolution): total={pnl.sum():.0f} mean={pnl.mean():.2f}"
          f" med={np.median(pnl):.2f} win={(pnl>0).mean():.3f} p5={pct(pnl,5)} p95={pct(pnl,95)}")

    # per-leg decomposition (outcomes from manifests; manifest close_ts == open ts)
    mani = os.path.join(DATA_ROOT, "data/manifests/canonical")
    outcome = {}
    for fn in ("btc-updown-5m_up.jsonl", "btc-updown-15m_up.jsonl"):
        for l in open(os.path.join(mani, fn)):
            r = json.loads(l)
            outcome[r["slug"]] = r["outcome"]
    leg_pnl = {"5": [], "15": []}
    leg_win = {"5": [], "15": []}
    both_lose = 0
    for e in tr:
        tot_check = 0.0
        for leg in ("5", "15"):
            d = e[f"d{leg}"]
            ask = e[f"ask{leg}"]
            won = (outcome[e[f"slug{leg}"]] == "Up") == (d == "up")
            p = (50.0 / ask) * ((1.0 if won else 0.0) - ask) if 0 < ask < 1 else 0.0
            leg_pnl[leg].append(p)
            leg_win[leg].append(won)
            tot_check += p
        if not leg_win["5"][-1] and not leg_win["15"][-1]:
            both_lose += 1
    for leg in ("5", "15"):
        a = np.array(leg_pnl[leg])
        w = np.array(leg_win[leg])
        print(f"  leg{leg:>2s}: pnl={a.sum():8.0f} mean={a.mean():6.2f} win_rate={w.mean():.3f}")
    print(f"  both-legs-lose rate: {both_lose/len(tr):.3f}"
          f"  (hedge: opposite windows can both win or both lose)")

    # tier sensitivity: trades whose entry comb_edge >= 8c
    hi = np.array([e["pnl"] for e in tr if e["comb_edge"] >= 0.08])
    if len(hi):
        print(f"  entry>=8c subset: n={len(hi)} pnl={hi.sum():.0f} mean={hi.mean():.2f}")

    r5 = np.array([e["rem5"] for e in tr])
    for lo, hi in [(15, 60), (60, 180), (180, 301)]:
        m = (r5 >= lo) & (r5 < hi)
        if m.sum():
            print(f"  rem5 [{lo:3d},{hi:3d}): n={m.sum():4d} pnl={pnl[m].sum():8.0f} mean={pnl[m].mean():7.2f}")

    daily = defaultdict(float)
    nd = defaultdict(int)
    for e in tr:
        daily[e["date"]] += e["pnl"]
        nd[e["date"]] += 1

    base = defaultdict(float)
    if os.path.exists(args.baseline):
        for l in open(args.baseline):
            r = json.loads(l)
            d = dt.datetime.fromtimestamp(r["open_ts_ns"] / 1e9, dt.timezone.utc).strftime("%Y-%m-%d")
            base[d] += r["pnl"]

    print("\ndate        trades  rv_pnl   baseline_fade_pnl")
    xs, ys = [], []
    for d in days:
        print(f"{d}  {nd[d]:5d}  {daily[d]:8.1f}  {base.get(d, float('nan')):10.1f}")
        if d in base:
            xs.append(daily[d])
            ys.append(base[d])
    p = np.array([daily[d] for d in days])
    print(f"\nrv daily: mean={p.mean():.1f} std={p.std(ddof=1):.1f} "
          f"sharpe_ann={p.mean()/p.std(ddof=1)*np.sqrt(365):.2f}" if len(p) > 1 and p.std(ddof=1) > 0 else "")
    if len(xs) >= 3:
        print(f"corr(rv, baseline) over {len(xs)} days: {np.corrcoef(xs, ys)[0,1]:.3f}")


if __name__ == "__main__":
    main()
