#!/usr/bin/env python3
"""Daily regime classification from 1m klines, 2019-2026.

Per day: realized vol (bps, from 5m log returns), trend efficiency
(|net move| / sum |5m moves|), and a joint label:
  trend  : efficiency >= 0.15 and vol >= median
  chop   : efficiency <  0.08 and vol >= median  (June-2026 style)
  quiet  : vol < median
  mixed  : everything else
Used for regime-conditioned economics (join with truthful-latency daily P&L)
and for placing 2026 months in historical context. Read-only research.

Usage:
  python3 scripts/research/regime_history.py --out data/research/regime_daily.csv
"""
from __future__ import annotations

import argparse
import glob
import math
from pathlib import Path

import pyarrow.parquet as pq

ROOT = "data/cache/raw/binance/exchange=binance/channel=klines_1m/symbol=BTCUSDT"


def day_stats(fp: str):
    t = pq.ParquetFile(fp).read()
    cols = t.column_names
    ccol = "close" if "close" in cols else cols[5]
    closes = [float(x.as_py()) for x in t.column(ccol)]
    if len(closes) < 100:
        return None
    rets = []
    for i in range(5, len(closes), 5):
        if closes[i - 5] > 0:
            rets.append(math.log(closes[i] / closes[i - 5]))
    if not rets:
        return None
    net = abs(math.log(closes[-1] / closes[0]))
    path = sum(abs(r) for r in rets)
    eff = net / path if path > 0 else 0.0
    vol_bps = (sum(r * r for r in rets) / len(rets)) ** 0.5 * 1e4
    return vol_bps, eff


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", default="data/research/regime_daily.csv")
    args = ap.parse_args()

    rows = []
    for d in sorted(glob.glob(f"{ROOT}/date=*")):
        day = d.split("date=")[1]
        fps = glob.glob(f"{d}/*.parquet")
        if not fps:
            continue
        s = day_stats(fps[0])
        if s:
            rows.append((day, *s))

    vols = sorted(v for _, v, _ in rows)
    med_vol = vols[len(vols) // 2]

    def label(vol, eff):
        if vol < med_vol:
            return "quiet"
        if eff >= 0.15:
            return "trend"
        if eff < 0.08:
            return "chop"
        return "mixed"

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as f:
        f.write("date,vol_bps,efficiency,label\n")
        for day, vol, eff in rows:
            f.write(f"{day},{vol:.2f},{eff:.4f},{label(vol, eff)}\n")
    print(f"wrote {len(rows)} days to {out} (median vol {med_vol:.2f} bps)")

    # summary: label mix per year + where 2026 months rank
    from collections import Counter, defaultdict
    per_year = defaultdict(Counter)
    per_month_2026 = defaultdict(Counter)
    for day, vol, eff in rows:
        lab = label(vol, eff)
        per_year[day[:4]][lab] += 1
        if day.startswith("2026"):
            per_month_2026[day[:7]][lab] += 1
    print("\nlabel mix by year (%):")
    for y in sorted(per_year):
        c = per_year[y]
        n = sum(c.values())
        print(f"  {y}: " + "  ".join(f"{k} {100*c[k]/n:.0f}%" for k in ("trend", "chop", "mixed", "quiet")))
    print("\n2026 months:")
    for m in sorted(per_month_2026):
        c = per_month_2026[m]
        n = sum(c.values())
        print(f"  {m}: " + "  ".join(f"{k} {c[k]}" for k in ("trend", "chop", "mixed", "quiet")) + f"  (n={n})")

    # chop-share percentile of June 2026 vs all months in history
    monthly = defaultdict(Counter)
    for day, vol, eff in rows:
        monthly[day[:7]][label(vol, eff)] += 1
    shares = []
    for m, c in monthly.items():
        n = sum(c.values())
        if n >= 15:
            shares.append((c["chop"] / n, m))
    shares.sort()
    jun = next((s for s, m in shares if m == "2026-06"), None)
    if jun is not None:
        rank = sum(1 for s, _ in shares if s <= jun) / len(shares)
        print(f"\nJune 2026 chop share: {jun:.0%} = {rank:.0%}ile of {len(shares)} months since 2019")
        print("choppiest months on record:", [(m, f"{s:.0%}") for s, m in shares[-5:]])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
