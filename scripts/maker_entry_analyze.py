#!/usr/bin/env python3
"""Maker-entry study: per-cell fee-true stats from harness trades JSONL.

Usage: maker_entry_analyze.py LABEL=path.jsonl [LABEL=path.jsonl ...]
Reports, per cell and for sigma>=4bps / ungated: trades, net P&L (total and
per day), hit rate of filled orders vs breakeven (avg fill price), daily
Sharpe and worst day, plus the maker-vs-taker adverse-selection comparison.
"""
import json
import math
import sys
from collections import defaultdict
from datetime import datetime, timezone

SIGMA_MIN = 4.0


def load(path):
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def day_of(row):
    return datetime.fromtimestamp(row["decision_ts_ns"] / 1e9, tz=timezone.utc).strftime(
        "%Y-%m-%d"
    )


def stats(rows):
    if not rows:
        return None
    n = len(rows)
    pnl = sum(r["pnl"] for r in rows)
    hits = sum(1 for r in rows if r["won"])
    avg_px = sum(r["avg_price"] for r in rows) / n
    fees = sum(r["fee"] for r in rows)
    daily = defaultdict(float)
    for r in rows:
        daily[day_of(r)] += r["pnl"]
    days = sorted(daily)
    vals = [daily[d] for d in days]
    mean = sum(vals) / len(vals)
    sd = math.sqrt(sum((v - mean) ** 2 for v in vals) / max(len(vals) - 1, 1))
    sharpe = mean / sd if sd > 0 else float("nan")
    return {
        "n": n,
        "pnl": pnl,
        "per_day": pnl / len(vals),
        "hit": hits / n,
        "avg_px": avg_px,
        "hit_minus_be": hits / n - avg_px,
        "fees": fees,
        "n_days": len(vals),
        "sharpe_d": sharpe,
        "worst_day": min(vals),
        "neg_days": sum(1 for v in vals if v < 0),
    }


def fmt(label, s, extra=""):
    if s is None:
        print(f"{label:>24}: no trades")
        return
    print(
        f"{label:>24}: n={s['n']:>5} pnl=${s['pnl']:>9.2f} (${s['per_day']:>7.2f}/d)"
        f" hit={s['hit'] * 100:5.2f}% avgpx={s['avg_px']:.4f}"
        f" hit-BE={s['hit_minus_be'] * 100:+5.2f}pp fees=${s['fees']:7.2f}"
        f" Sharpe(d)={s['sharpe_d']:5.2f} worst=${s['worst_day']:8.2f}"
        f" neg={s['neg_days']}/{s['n_days']}{extra}"
    )


def main():
    cells = {}
    for arg in sys.argv[1:]:
        label, path = arg.split("=", 1)
        cells[label] = load(path)

    for label, rows in cells.items():
        entries = [r for r in rows if not r.get("is_completion")]
        print(f"\n== {label} ({len(entries)} entry trades) ==")
        fmt("ungated", stats(entries))
        gated = [r for r in entries if r.get("sigma_bar_bps", 0.0) >= SIGMA_MIN]
        fmt(f"sigma>={SIGMA_MIN:g}bps", stats(gated))

    # Adverse selection: on the same (market, side) the resolution outcome is
    # identical, so the toxic part is WHICH signals get filled. Split the
    # taker control by whether the maker bid would have filled there.
    ctrl = next((rows for label, rows in cells.items() if label.startswith("taker")), None)
    if ctrl is None:
        return
    ctrl_entries = [r for r in ctrl if not r.get("is_completion")]
    print("\n== adverse-selection diagnostic (taker control split by maker fill) ==")
    for label, rows in cells.items():
        if label.startswith("taker"):
            continue
        fills = [r for r in rows if r.get("maker_entry")]
        if not fills:
            continue
        keys = {(r["open_ts_ns"], r["side"]) for r in fills}
        t_fill = [r for r in ctrl_entries if (r["open_ts_ns"], r["side"]) in keys]
        t_nofill = [r for r in ctrl_entries if (r["open_ts_ns"], r["side"]) not in keys]
        mpx = sum(r["avg_price"] for r in fills) / len(fills)
        m_pnl = sum(r["pnl"] for r in fills)
        sf, sn = stats(t_fill), stats(t_nofill)
        print(f"\n{label}: maker filled n={len(fills)} px={mpx:.4f} pnl=${m_pnl:.2f}")
        fmt("  taker | would-fill", sf)
        fmt("  taker | no-fill", sn)
        if sf:
            px_gain = sf["avg_px"] - mpx
            print(
                f"{'':>24}  filled-subset toxicity: taker hit there {sf['hit'] * 100:.2f}% vs"
                f" full-population {stats(ctrl_entries)['hit'] * 100:.2f}%;"
                f" maker gains {px_gain * 100:+.2f}c px + ${sf['fees'] / sf['n']:.3f} fee/trade,"
                f" maker-vs-taker pnl on same signals ${m_pnl - sf['pnl']:+,.2f}"
            )


if __name__ == "__main__":
    main()
