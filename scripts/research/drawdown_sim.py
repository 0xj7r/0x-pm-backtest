#!/usr/bin/env python3
"""Equity-curve simulation for the fade at $850, with the Polymarket 5-share floor.

Reproduces the drawdown-handling analysis in docs/drawdown-handling-plan-2026-07.md.
Reads per-trade dumps at truthful 1250ms latency across the validated months and
builds a daily P&L series, then simulates fractional sizing with the venue's
5-share minimum order constraint (which puts a hard floor on clip size and breaks
the "shrink smoothly to zero" property below a critical equity).

NOTE (2026-07 hardening, per reviews/drawdown-glm-review.md): the floor entry price
now defaults to the MAX observed entry price (previously the median). The floor
binds on the highest price actually paid (the strategy enters up to ~0.95), so the
median understated where the floor first bites. Also added in this pass: an
asymmetric loss haircut, a clustered (regime-persistent) day-ordering adversary
alongside the uniform shuffle, a floor-band-breach headline metric, and a committed
CSV snapshot of the daily P&L series so the analysis re-runs from the repo alone.

Model assumptions (all stated so a reviewer can challenge them):
  - Daily P&L at $50 telemetry clips is treated as linearly scalable to any clip
    size: pnl(clip) = pnl_at_50 * clip/50. Ignores market impact (clips are $2-10,
    book depth is far larger) and treats a day's fill mix as clip-invariant.
  - REALIZATION haircut multiplies positive daily P&L; --loss-haircut multiplies
    negative daily P&L (default = --haircut, i.e. symmetric = the prior behavior).
    Live losses can exceed replay, so --loss-haircut can be set larger to model a
    pessimistic-loss case without touching the win side.
  - 5-share floor: minimum order notional = 5 * entry_price. Below the equity
    where frac*E < floor, the clip is forced UP to the floor (flat-min betting).
  - Floor entry price: MAX observed entry price by default (--floor-price max),
    i.e. the binding price; override with --price or --floor-price median.
  - Day ordering: the uniform Monte Carlo shuffle is exchangeable and destroys loss
    clustering; --cluster adds a regime-sticky 2-state model that preserves the
    exact red-fraction while clustering reds, exposing the tail the uniform hides.

Usage:
  python3 scripts/research/drawdown_sim.py
  python3 scripts/research/drawdown_sim.py --cluster --start 600
  python3 scripts/research/drawdown_sim.py --loss-haircut 1.0 --haircut 0.6
  python3 scripts/research/drawdown_sim.py --dump-series data/research/daily_pnl_series.csv
"""
from __future__ import annotations

import argparse
import csv
import datetime
import json
import os
import random

TRADE_FILES = [
    "data/runs/tune_validation/feb_lat1250_ungated_trades.jsonl",
    "data/runs/tune_validation/mar_lat1250_ungated_trades.jsonl",
    "data/runs/tune_validation/apr_lat1250_ungated_trades.jsonl",
    "data/runs/may_dwell_sweep/may_lat1250_ungated_trades.jsonl",
    "data/runs/latency_truth/btc5m_lat1250_trades.jsonl",
]
SERIES_CSV = "data/research/daily_pnl_series.csv"
MIN_SHARES = 5.0
CEIL_DEFAULT = 10.0
TELEMETRY_CLIP = 50.0


def _load_raw():
    """Aggregate the per-trade dumps into sorted (date, pnl) rows and the list of
    entry prices. Returns ([], []) when the gitignored dumps are absent."""
    daily, prices = {}, []
    for fp in TRADE_FILES:
        try:
            f = open(fp)
        except FileNotFoundError:
            continue
        with f:
            for line in f:
                t = json.loads(line)
                d = datetime.datetime.fromtimestamp(
                    t["fill_ts_ns"] / 1e9, datetime.timezone.utc).date().isoformat()
                daily[d] = daily.get(d, 0.0) + t["pnl"]
                if t.get("avg_price"):
                    prices.append(t["avg_price"])
    rows = [(d, daily[d]) for d in sorted(daily)]
    return rows, prices


def _load_series_csv(path):
    """Read the committed daily P&L snapshot (date, pnl_at_50, med_price,
    max_price). Fallback used when the raw trade dumps are absent."""
    try:
        f = open(path)
    except FileNotFoundError:
        return [], 0.59, 0.95
    med, mx = 0.59, 0.95
    seq = []
    with f:
        r = csv.reader(f)
        next(r, None)  # header
        for row in r:
            if not row:
                continue
            seq.append(float(row[1]))
            if len(row) >= 4:
                med, mx = float(row[2]), float(row[3])
    return seq, med, mx


def load_daily(series_csv=SERIES_CSV):
    """Return (daily_pnl_series, median_price, max_price). Reads the raw trade
    dumps when present; otherwise falls back to the committed CSV snapshot so the
    sim is reproducible from the repo with no external data."""
    rows, prices = _load_raw()
    if rows:
        prices.sort()
        med = prices[len(prices) // 2] if prices else 0.59
        mx = prices[-1] if prices else 0.95
        return [p for _, p in rows], med, mx
    return _load_series_csv(series_csv)


def dump_series(path):
    """Write the daily P&L series (date, pnl_at_50, med_price, max_price) to CSV.
    Requires the raw trade dumps. Returns rows written (0 = no data)."""
    rows, prices = _load_raw()
    if not rows:
        return 0
    prices.sort()
    med = prices[len(prices) // 2] if prices else 0.59
    mx = prices[-1] if prices else 0.95
    d = os.path.dirname(path)
    if d:
        os.makedirs(d, exist_ok=True)
    with open(path, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["date", "pnl_at_50", "med_price", "max_price"])
        for date, pnl in rows:
            w.writerow([date, f"{pnl:.6f}", f"{med:.6f}", f"{mx:.6f}"])
    return len(rows)


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


def run(seq, start, frac, ceil, price, haircut, loss_haircut=None,
        throttle=False, thr=0.80, mult=0.5, floor_mode="forced"):
    """Walk one ordered daily-P&L sequence through the sizing model. `haircut`
    scales positive days; `loss_haircut` (default = haircut, i.e. symmetric)
    scales negative days, so an asymmetric pessimistic-loss case can be modeled.
    Returns (end, trough, ruined). Ruin = equity too low for even one min order."""
    if loss_haircut is None:
        loss_haircut = haircut
    E = peak = trough = start
    for x in seq:
        clip = clip_for(E, peak, frac, ceil, price, throttle, thr, mult, floor_mode)
        if clip <= 0.0:      # skip-mode: cannot size, sit out
            continue
        h = loss_haircut if x < 0 else haircut
        E += h * x * clip / TELEMETRY_CLIP
        if E <= MIN_SHARES * price:  # cannot place even one min order -> dead
            return 0.0, 0.0, True
        peak = max(peak, E)
        trough = min(trough, E)
    return E, trough, False


def longest_red_run(order):
    """Longest streak of consecutive losing (pnl < 0) days in an ordering."""
    best = cur = 0
    for x in order:
        if x < 0:
            cur += 1
            if cur > best:
                best = cur
        else:
            cur = 0
    return best


def uniform_order(seq, rng):
    """Exchangeable shuffle: resamples without replacement, preserving the win/loss
    mix but destroying autocorrelation (understates clustered-regime tails)."""
    s = list(seq)
    rng.shuffle(s)
    return s


def clustered_order(seq, p_stick, rng):
    """Permute `seq` without replacement (so the red-fraction is preserved exactly)
    with a 2-state persistence bias: after a red day the next draw is red with
    probability p_stick, otherwise uniform over the remaining days. Clusters losses
    the way a real adverse regime does, unlike an exchangeable shuffle."""
    remaining = list(seq)
    rng.shuffle(remaining)
    order = []
    cur = remaining.pop(rng.randrange(len(remaining)))
    order.append(cur)
    while remaining:
        reds = [j for j, x in enumerate(remaining) if x < 0]
        if cur < 0 and reds and rng.random() < p_stick:
            j = rng.choice(reds)
        else:
            j = rng.randrange(len(remaining))
        cur = remaining.pop(j)
        order.append(cur)
    return order


def _mc(seq, n, seed, order_fn, starts, runkw, bite, primary_start):
    """Run n Monte Carlo paths under one ordering model. Reports the longest-red-run
    distribution, the trough distribution at primary_start, ruin count at
    primary_start, and the floor-band-breach fraction (trough < bite) per start."""
    rng = random.Random(seed)
    troughs = {s: [] for s in starts}
    runs, ruins = [], 0
    breach = {s: 0 for s in starts}
    for _ in range(n):
        o = order_fn(seq, rng)
        runs.append(longest_red_run(o))
        for s in starts:
            _, t, r = run(o, s, **runkw)
            troughs[s].append(t)
            if t < bite:
                breach[s] += 1
            if s == primary_start:
                ruins += r
    runs.sort()
    pt = sorted(troughs[primary_start])
    return {
        "runs_p50": runs[len(runs) // 2],
        "runs_p95": runs[min(len(runs) - 1, int(0.95 * len(runs)))],
        "runs_max": runs[-1],
        "ruins": ruins,
        "breach": {s: breach[s] / n for s in starts},
        "trough_p5": pt[int(0.05 * len(pt))],
        "trough_worst": pt[0],
    }


def _print_mc(label, m, n, bite, starts, primary_start):
    print(f"{label} {n}: ruin {m['ruins']}/{n} | longest-red-run "
          f"p50={m['runs_p50']} p95={m['runs_p95']} max={m['runs_max']} | "
          f"{primary_start:.0f} p5 trough ${m['trough_p5']:,.0f} "
          f"worst ${m['trough_worst']:,.0f}")
    print(f"  floor-band breach (equity < ${bite:.0f}): "
          + " ".join(f"${s:.0f} {m['breach'][s] * 100:.1f}%" for s in starts))


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--start", type=float, default=850.0)
    ap.add_argument("--frac", type=float, default=0.0075)
    ap.add_argument("--ceil", type=float, default=CEIL_DEFAULT)
    ap.add_argument("--haircut", type=float, default=0.6,
                    help="realization haircut on positive daily P&L")
    ap.add_argument("--loss-haircut", type=float, default=None,
                    help="haircut on negative daily P&L (default = --haircut)")
    ap.add_argument("--price", type=float, default=None,
                    help="entry price override for the 5-share floor")
    ap.add_argument("--floor-price", choices=("median", "max"), default="max",
                    help="entry price used for the floor (default max = binding price)")
    ap.add_argument("--shuffles", type=int, default=3000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--cluster", action="store_true",
                    help="also run the clustered (regime-sticky) day-ordering adversary")
    ap.add_argument("--cluster-stick", type=float, default=0.6,
                    help="p(next day red | current red) for the clustered model")
    ap.add_argument("--dump-series", metavar="PATH", default=None,
                    help="write the daily P&L series to CSV and exit")
    args = ap.parse_args()

    if args.dump_series:
        n = dump_series(args.dump_series)
        if not n:
            print("no trade data found; run the TUNE/latency sweeps first")
            return 1
        print(f"wrote {n} daily rows -> {args.dump_series}")
        return 0

    seq, med_price, max_price = load_daily()
    if not seq:
        print("no trade data found; run the TUNE/latency sweeps first "
              f"(or commit {SERIES_CSV})")
        return 1

    floor_price = max_price if args.floor_price == "max" else med_price
    price = args.price if args.price is not None else floor_price
    loss_hc = args.loss_haircut if args.loss_haircut is not None else args.haircut
    floor = MIN_SHARES * price
    bite = floor / args.frac
    starts = (850.0, 600.0, 400.0)
    runkw = dict(frac=args.frac, ceil=args.ceil, price=price,
                 haircut=args.haircut, loss_haircut=loss_hc)

    print(f"days={len(seq)} green={sum(1 for p in seq if p>0)}/{len(seq)} "
          f"mean=${sum(seq)/len(seq):+,.0f}/day@$50 | floor=${floor:.2f} "
          f"(5 sh @ {price:.2f}, {args.floor_price}) bites at ${bite:.0f} | "
          f"haircut={args.haircut} loss-haircut={loss_hc}")

    end, tr, ruin = run(seq, args.start, **runkw)
    print(f"chronological: end ${end:,.0f} trough ${tr:,.0f}"
          f"{' RUINED' if ruin else ''}")

    worst8 = sorted(range(len(seq)), key=lambda i: seq[i])[:8]
    adv = [seq[i] for i in worst8] + [seq[i] for i in range(len(seq)) if i not in worst8]
    end, tr, ruin = run(adv, args.start, **runkw)
    print(f"adversarial(worst-8-first): end ${end:,.0f} trough ${tr:,.0f}"
          f"{' RUINED' if ruin else ''}")

    u = _mc(seq, args.shuffles, args.seed, uniform_order,
            starts, runkw, bite, args.start)
    _print_mc("uniform MC", u, args.shuffles, bite, starts, args.start)

    if args.cluster:
        c = _mc(seq, args.shuffles, args.seed,
                lambda s, r: clustered_order(s, args.cluster_stick, r),
                starts, runkw, bite, args.start)
        _print_mc(f"clustered MC (p_stick={args.cluster_stick})", c,
                  args.shuffles, bite, starts, args.start)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
