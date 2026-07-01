#!/usr/bin/env python3
"""Score the bonereaper favourite play from an alpha trades-out JSONL.

Reports per asset, sigma>=4 floored and window_secs==300 only:
  - taker: NET, $/day, hit, Sharpe (daily), avg entry price
  - maker-equivalent: NET = taker_gross + fees_saved + rebate
  - taker breakeven hit = p + fee/shares (here p + 0.07*p*(1-p))
  - maker breakeven hit = p - rebate (rebate = 0.2*0.07*p*(1-p))
  - verdict vs each breakeven

Usage: bonereaper_score.py <trades.jsonl> [sigma_floor=4.0] [label]
Pooled multi-asset: bonereaper_score.py --pool f1.jsonl f2.jsonl ...
"""
import json
import sys
import math
from collections import defaultdict
from datetime import datetime, timezone

REBATE_FRAC = 0.20  # 20% of the taker fee pool returned to the maker
FEE_RATE = 0.07


def load(path):
    rows = []
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def day_of(ns):
    return datetime.fromtimestamp(ns / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")


def fee_per_share(p):
    return FEE_RATE * p * (1.0 - p)


def score(rows, label):
    if not rows:
        print(f"{label}: NO TRADES")
        return None
    per_day_taker = defaultdict(float)
    per_day_maker = defaultdict(float)
    n_win = 0
    taker_net = 0.0
    fees_saved = 0.0
    rebate_est = 0.0
    price_w = 0.0  # share-weighted entry price
    shares_tot = 0.0
    for r in rows:
        d = day_of(r["decision_ts_ns"])
        pnl = r["pnl"]
        p = r["avg_price"]
        sh = r["shares"]
        fee = r.get("fee", fee_per_share(p) * sh)
        rebate = REBATE_FRAC * fee_per_share(p) * sh
        per_day_taker[d] += pnl
        # maker pays zero taker fee (add it back) and earns a rebate
        per_day_maker[d] += pnl + fee + rebate
        taker_net += pnl
        fees_saved += fee
        rebate_est += rebate
        price_w += p * sh
        shares_tot += sh
        if r.get("won"):
            n_win += 1
    days = sorted(per_day_taker)
    n_days = len(days)
    n = len(rows)
    hit = n_win / n
    avg_p = price_w / shares_tot if shares_tot else float("nan")

    # breakevens at the share-weighted avg entry price
    be_taker = avg_p + FEE_RATE * avg_p * (1.0 - avg_p)
    be_maker = avg_p - REBATE_FRAC * FEE_RATE * avg_p * (1.0 - avg_p)

    def sharpe(per_day):
        daily = [per_day[d] for d in days]
        m = sum(daily) / n_days
        sd = math.sqrt(sum((x - m) ** 2 for x in daily) / n_days) if n_days > 1 else 0.0
        s = (m / sd) if sd > 0 else float("nan")
        green = sum(1 for x in daily if x > 0)
        return m, sd, s, green, daily

    tk_m, tk_sd, tk_s, tk_green, tk_daily = sharpe(per_day_taker)
    mk_m, mk_sd, mk_s, mk_green, mk_daily = sharpe(per_day_maker)
    maker_net = taker_net + fees_saved + rebate_est

    print(f"{label}:  trades={n} days={n_days} hit={hit*100:.2f}% avg_entry={avg_p:.4f}")
    print(f"  breakeven hit: taker={be_taker*100:.2f}%  maker={be_maker*100:.2f}%")
    tk_v = "CLEARS" if hit >= be_taker else "below"
    mk_v = "CLEARS" if hit >= be_maker else "below"
    print(f"  TAKER: NET=${taker_net:,.0f} $/day=${taker_net/n_days:,.0f} "
          f"Sharpe={tk_s:.2f} green={tk_green}/{n_days}  [{tk_v} taker BE by {(hit-be_taker)*100:+.2f}pp]")
    print(f"  MAKER: NET=${maker_net:,.0f} $/day=${maker_net/n_days:,.0f} "
          f"Sharpe={mk_s:.2f} green={mk_green}/{n_days}  [{mk_v} maker BE by {(hit-be_maker)*100:+.2f}pp]")
    print(f"  (fees_saved=${fees_saved:,.0f} rebate_est=${rebate_est:,.0f})")
    return dict(label=label, n=n, days=n_days, hit=hit, avg_p=avg_p,
                be_taker=be_taker, be_maker=be_maker,
                taker_net=taker_net, maker_net=maker_net,
                tk_sharpe=tk_s, mk_sharpe=mk_s, tk_green=tk_green, mk_green=mk_green,
                tk_daily=dict(zip(days, tk_daily)), mk_daily=dict(zip(days, mk_daily)))


def main():
    args = sys.argv[1:]
    if args and args[0] == "--pool":
        # Pool trades across assets, score combined daily series (taker + maker).
        paths = args[1:]
        allrows = []
        for p in paths:
            rows = [r for r in load(p) if r.get("window_secs") == 300 and r.get("sigma_bar_bps", 0.0) >= 4.0]
            allrows.extend(rows)
        score(allrows, "POOLED sigma>=4 win300")
        return
    path = args[0]
    sigma_floor = float(args[1]) if len(args) > 1 else 4.0
    label = args[2] if len(args) > 2 else path
    rows = load(path)
    w300 = [r for r in rows if r.get("window_secs") == 300]
    flt = [r for r in w300 if r.get("sigma_bar_bps", 0.0) >= sigma_floor]
    print(f"== {label} (raw rows={len(rows)}, win300={len(w300)}) ==")
    score(w300, f"{label} win300 no-floor")
    score(flt, f"{label} win300 sigma>={sigma_floor:g}")


if __name__ == "__main__":
    main()
