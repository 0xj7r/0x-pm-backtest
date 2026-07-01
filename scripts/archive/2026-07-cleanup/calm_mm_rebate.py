#!/usr/bin/env python3
"""Maker-rebate + stranding-risk overlay on the queue-aware paired-MM sim.

Patches scripts/mm_paired_sim.py at import time:

1. Rebate accrual -> Polymarket's actual fee-curve schedule (verified live on
   btc-updown-5m via gamma API, June 2026: feesEnabled, feeType crypto_fees_v2,
   rate 0.07, rebateRate 0.2, takerOnly):
       taker fee = C * 0.07 * p * (1-p);  maker rebate = 20% pool pro-rata
       => per maker fill: qty * 0.014 * p * (1-p)   (0.35c/share at p=0.5)
   (The sim's placeholder was 0.20 * 1.56% on price-notional; the real schedule
   pays ~2.2x that at p=0.5.)

2. QUEUE_SCALE knob multiplying the pro-rata fill fraction clip/(clip+ahead),
   to probe queue-position sensitivity. The live queue calibration
   (docs/mm_queue_model_2026-06-01.md) found the pro-rata sim overstates fill
   volume ~14x (only 1.6% of posted orders fill), so 0.07-0.1 is the calibrated
   capture, 0.5 an optimistic order-management-fixed case, 1.0 the sim default.

Reports, per (clip, queue_scale), on the GATED calm-window config:
  - paired income + rebate vs residual (stranded leg) P&L as separate lines
  - fraction of quoted markets ending with unpaired inventory
  - residual P&L distribution (mean / p5 / worst)
  - toxic-fill rate: residual ends on the LOSING side
  - verdict ratio: |residual losses| / (paired + rebate gross)

Tune window May 7-18 only (selection-safe). Run from the repo root.
"""

import glob
import sys
import types
from collections import defaultdict

import numpy as np

SIM_PATH = "scripts/mm_paired_sim.py"
FEE_RATE = 0.07
POOL_SHARE = 0.20
DATES = [f"2026-05-{d:02d}" for d in range(7, 19)]

# realized-vol terciles from calm_quietday_decomp.py (Binance BTCUSDT 1m RV)
TERC = {
    "2026-05-09": "quiet", "2026-05-16": "quiet", "2026-05-13": "quiet", "2026-05-12": "quiet",
    "2026-05-10": "mid", "2026-05-08": "mid", "2026-05-17": "mid", "2026-05-07": "mid",
    "2026-05-11": "loud", "2026-05-15": "loud", "2026-05-14": "loud", "2026-05-18": "loud",
}


def load_sim():
    src = open(SIM_PATH).read()
    src = src.replace(
        "rebate_usdc += qty * b * REBATE_FRAC",
        "rebate_usdc += qty * b * (1.0 - b) * REBATE_FRAC")
    src = src.replace(
        "rebate_usdc += qty * (1.0 - a) * REBATE_FRAC",
        "rebate_usdc += qty * (1.0 - a) * a * REBATE_FRAC")
    src = src.replace(
        "REBATE_FRAC = 0.20 * TAKER_FEE_FRAC",
        f"REBATE_FRAC = {POOL_SHARE} * {FEE_RATE}\nQUEUE_SCALE = 1.0")
    assert src.count("frac = clip / (clip + ahead)") == 2
    src = src.replace(
        "frac = clip / (clip + ahead)",
        "frac = QUEUE_SCALE * clip / (clip + ahead)")
    mod = types.ModuleType("mm_sim_patched")
    exec(compile(src, SIM_PATH, "exec"), mod.__dict__)
    return mod


def stranding_report(rows, label):
    quoted = [r for r in rows if r["filled_shares"] > 0]
    if not quoted:
        print(f"{label}: no filled markets")
        return None
    paired_inc = sum(r["pnl_pairs"] for r in quoted)
    rebate_inc = sum(r["rebate"] for r in quoted)
    resid = np.array([r["pnl_resid"] for r in quoted])
    res_mkts = [r for r in quoted if r["res_shares"] > 1e-9]
    toxic = 0
    onesided = 0
    for r in res_mkts:
        res_yes = r["yes_long"] - r["paired"]
        res_no = r["no_long"] - r["paired"]
        if res_yes > 1e-9 or res_no > 1e-9:
            onesided += 1
            lost = (res_yes > 1e-9 and not r["yes_wins"]) or (res_no > 1e-9 and r["yes_wins"])
            toxic += lost
    resid_losses = float(-resid[resid < 0].sum())
    gross_income = paired_inc + rebate_inc
    ratio = resid_losses / gross_income if gross_income > 0 else float("inf")
    nd = len({r["date"] for r in quoted})
    net = paired_inc + rebate_inc + float(resid.sum())
    print(f"{label}")
    print(f"  markets filled={len(quoted)}  days={nd}  shares/day={sum(r['filled_shares'] for r in quoted)/nd:.0f}")
    print(f"  income: paired=${paired_inc:8.1f}  rebate=${rebate_inc:7.1f}  gross=${gross_income:8.1f} (${gross_income/nd:.1f}/day)")
    print(f"  residual: net=${resid.sum():8.1f}  losses-only=-${resid_losses:.1f}  "
          f"mean=${resid.mean():.2f}  p5=${np.percentile(resid, 5):.2f}  worst=${resid.min():.2f}")
    print(f"  stranded markets: {len(res_mkts)}/{len(quoted)} = {len(res_mkts)/len(quoted)*100:.1f}%  "
          f"toxic(residual on losing side): {toxic}/{onesided} = {toxic/max(onesided,1)*100:.1f}%")
    print(f"  NET=${net:8.1f} (${net/nd:.1f}/day)   "
          f"VERDICT ratio |resid losses|/gross income = {ratio*100:.1f}%  "
          f"{'REJECT(>30%)' if ratio > 0.30 else 'ok(<=30%)'}")
    return ratio


def main():
    mod = load_sim()
    book_files = []
    for d in DATES:
        book_files += sorted(glob.glob(f"{mod.BOOK_ROOT}/date={d}/asset_id=*/*.parquet"))
    print(f"book files: {len(book_files)} over {len(DATES)} tune days", flush=True)
    print(f"rebate: {POOL_SHARE:.0%} x {FEE_RATE} x p(1-p)/share = {POOL_SHARE*FEE_RATE*0.25*100:.3f}c/sh at p=0.5", flush=True)
    parsed = mod.load_all_markets(book_files, {})
    print(f"parsed {len(parsed)} markets", flush=True)

    for clip in (10, 20):
        for qs in (1.0, 0.5, 0.1):
            mod.QUEUE_SCALE = qs
            rows = mod.run(parsed, clip, regime_gated=True, rebate_on=True)
            print(f"\n#### GATED clip={clip} queue_scale={qs} ####", flush=True)
            stranding_report(rows, f"clip{clip}/qs{qs}")
            if qs == 1.0:
                by_day = defaultdict(lambda: defaultdict(float))
                for r in rows:
                    d = by_day[r["date"]]
                    d["pairs"] += r["pnl_pairs"]
                    d["reb"] += r["rebate"]
                    d["resid"] += r["pnl_resid"]
                    d["sh"] += r["filled_shares"]
                terc = defaultdict(lambda: defaultdict(float))
                print(f"  {'day':12s}{'terc':6s}{'shares':>8s}{'pairs$':>8s}{'rebate$':>8s}{'resid$':>8s}{'net$':>8s}")
                for day in sorted(by_day):
                    d = by_day[day]
                    t = TERC.get(day, "?")
                    print(f"  {day:12s}{t:6s}{d['sh']:8.0f}{d['pairs']:8.1f}{d['reb']:8.1f}{d['resid']:8.1f}"
                          f"{d['pairs']+d['reb']+d['resid']:8.1f}")
                    a = terc[t]
                    for k in ("pairs", "reb", "resid", "sh"):
                        a[k] += d[k]
                    a["days"] += 1
                for t in ("quiet", "mid", "loud"):
                    a = terc[t]
                    if a.get("days"):
                        n = a["days"]
                        print(f"  tercile {t:5s}: net=${(a['pairs']+a['reb']+a['resid'])/n:7.1f}/day "
                              f"(pairs {a['pairs']/n:.1f} + rebate {a['reb']/n:.1f} + resid {a['resid']/n:.1f})", flush=True)


if __name__ == "__main__":
    main()
