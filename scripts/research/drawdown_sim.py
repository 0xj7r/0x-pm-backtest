#!/usr/bin/env python3
"""Equity-curve simulation for the fade at $850, with the Polymarket 5-share floor.

Reproduces the drawdown-handling analysis in docs/drawdown-handling-plan-2026-07.md.
Reads per-trade dumps at truthful 1250ms latency across the validated months and
builds a daily P&L series, then simulates fractional sizing with the venue's
5-share minimum order constraint (which puts a hard floor on clip size and breaks
the "shrink smoothly to zero" property below a critical equity).

Model assumptions (all stated so a reviewer can challenge them):
  - Daily P&L at $50 telemetry clips is treated as linearly scalable to any clip
    size: pnl(clip) = pnl_at_50 * clip/50. Ignores market impact (clips are $2-10,
    book depth is far larger) and treats a day's fill mix as clip-invariant.
  - REALIZATION haircut multiplies every day's P&L (default 1.0 = raw replay;
    0.6 = conservative live capture). Applied symmetrically to wins and losses.
  - 5-share floor: minimum order notional = 5 * entry_price. Below the equity
    where frac*E < floor, the clip is forced UP to the floor (flat-min betting).
  - Median entry price used for the floor unless --price given; real entries span
    ~0.45 (min_entry_ask) to ~0.85 (v1 p_side cap).
  - Monte Carlo shuffles the day order to estimate drawdown/ruin distribution;
    the daily P&L values are resampled without replacement (a permutation), which
    preserves the realized win/loss mix but destroys autocorrelation.

Usage:
  python3 scripts/research/drawdown_sim.py
  python3 scripts/research/drawdown_sim.py --start 850 --frac 0.0075 --haircut 0.6
"""
from __future__ import annotations

import argparse
import datetime
import glob
import json
import random

TRADE_FILES = [
    "data/runs/tune_validation/feb_lat1250_ungated_trades.jsonl",
    "data/runs/tune_validation/mar_lat1250_ungated_trades.jsonl",
    "data/runs/tune_validation/apr_lat1250_ungated_trades.jsonl",
    "data/runs/may_dwell_sweep/may_lat1250_ungated_trades.jsonl",
    "data/runs/latency_truth/btc5m_lat1250_trades.jsonl",
]
MIN_SHARES = 5.0
CEIL_DEFAULT = 10.0
TELEMETRY_CLIP = 50.0


def load_daily():
    daily, prices = {}, []
    for fp in TRADE_FILES:
        try:
            f = open(fp)
        except FileNotFoundError:
            continue
        for line in f:
            t = json.loads(line)
            d = datetime.datetime.fromtimestamp(
                t["fill_ts_ns"] / 1e9, datetime.timezone.utc).date().isoformat()
            daily[d] = daily.get(d, 0.0) + t["pnl"]
            if t.get("avg_price"):
                prices.append(t["avg_price"])
    prices.sort()
    med_price = prices[len(prices) // 2] if prices else 0.59
    return [daily[d] for d in sorted(daily)], med_price


def clip_for(E, peak, frac, ceil, price, throttle, thr, mult, floor_mode):
    """Return the actual clip in USD after fractional sizing, throttle, ceiling,
    the 5-share floor, and the 0.01-share quantum. floor_mode: 'forced' (bet the
    minimum) or 'skip' (return 0 = cannot size)."""
    f = frac * mult if (throttle and E < thr * peak) else frac
    want = f * E
    floor = MIN_SHARES * price
    if want < floor:
        if floor_mode == "skip":
            return 0.0
        clip = floor
    else:
        clip = min(want, ceil)
    shares = max(MIN_SHARES, round(clip / price * 100) / 100)  # 0.01-share quantum
    return shares * price


def run(seq, start, frac, ceil, price, haircut,
        throttle=False, thr=0.80, mult=0.5, floor_mode="forced"):
    E = peak = trough = start
    ruined = False
    for x in seq:
        clip = clip_for(E, peak, frac, ceil, price, throttle, thr, mult, floor_mode)
        if clip <= 0.0:      # skip-mode: cannot size, sit out
            continue
        E += haircut * x * clip / TELEMETRY_CLIP
        if E <= MIN_SHARES * price:  # cannot place even one min order -> dead
            return 0.0, 0.0, True
        peak = max(peak, E)
        trough = min(trough, E)
    return E, trough, ruined


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--start", type=float, default=850.0)
    ap.add_argument("--frac", type=float, default=0.0075)
    ap.add_argument("--ceil", type=float, default=CEIL_DEFAULT)
    ap.add_argument("--haircut", type=float, default=0.6)
    ap.add_argument("--price", type=float, default=None, help="entry price for floor")
    ap.add_argument("--shuffles", type=int, default=3000)
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()

    pnl, med_price = load_daily()
    price = args.price or med_price
    if not pnl:
        print("no trade data found; run the TUNE/latency sweeps first")
        return 1
    floor = MIN_SHARES * price
    print(f"days={len(pnl)} green={sum(1 for p in pnl if p>0)}/{len(pnl)} "
          f"mean=${sum(pnl)/len(pnl):+,.0f}/day@$50 | floor=${floor:.2f} "
          f"(5 sh @ {price:.2f}) bites at equity ${floor/args.frac:.0f}")

    end, tr, ruin = run(pnl, args.start, args.frac, args.ceil, price, args.haircut)
    print(f"chronological: end ${end:,.0f} trough ${tr:,.0f}"
          f"{' RUINED' if ruin else ''}")

    worst8 = sorted(range(len(pnl)), key=lambda i: pnl[i])[:8]
    adv = [pnl[i] for i in worst8] + [pnl[i] for i in range(len(pnl)) if i not in worst8]
    end, tr, ruin = run(adv, args.start, args.frac, args.ceil, price, args.haircut)
    print(f"adversarial(worst-8-first): end ${end:,.0f} trough ${tr:,.0f}"
          f"{' RUINED' if ruin else ''}")

    rng = random.Random(args.seed)
    ruins, troughs = 0, []
    for _ in range(args.shuffles):
        s = pnl[:]
        rng.shuffle(s)
        _, t, r = run(s, args.start, args.frac, args.ceil, price, args.haircut)
        ruins += r
        troughs.append(t)
    troughs.sort()
    p5 = troughs[len(troughs) // 20]
    print(f"MC {args.shuffles} shuffles: ruin {ruins}/{args.shuffles} | "
          f"p5 trough ${p5:,.0f} | worst ${troughs[0]:,.0f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
