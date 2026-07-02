#!/usr/bin/env python3
"""Trade-by-trade equity simulation with fractional sizing and a cash constraint.

Replays harness per-trade dumps (--trades-out JSONL, $50-notional records) with
clip = frac x running equity, capital tied up from fill to market close,
entries skipped when cash is exhausted. Reports max drawdown and worst runs at
small-bankroll scale, which daily aggregates at fixed $50 clips hide.

Usage:
  python3 scripts/research/equity_drawdown_sim.py \
    --runs data/runs/may_trades_daily data/runs/june_trades_daily \
    --equity 850 --fracs 0.005,0.01,0.02 --flat-clips 10
"""
from __future__ import annotations

import argparse
import glob
import json
from pathlib import Path

REF_NOTIONAL = 50.0


def load_trades(run_dirs: list[str]) -> list[dict]:
    rows: list[dict] = []
    for rd in run_dirs:
        for fp in sorted(glob.glob(str(Path(rd) / "*_trades.jsonl"))):
            for line in open(fp):
                if not line.strip():
                    continue
                t = json.loads(line)
                open_ns = t["open_ts_ns"]
                t["_close_ns"] = open_ns + int(t.get("window_secs") or 300) * 1_000_000_000
                t["_cost"] = t["avg_price"] * t["shares"] + t.get("fee", 0.0)
                rows.append(t)
    rows.sort(key=lambda t: t["fill_ts_ns"])
    return rows


def simulate(
    trades: list[dict],
    equity0: float,
    frac: float | None,
    flat_clip: float | None,
    ceiling: float = 50.0,
):
    """frac: clip = min(frac x equity, ceiling), the executor policy;
    flat_clip: fixed dollar clip. Ceiling reflects touch depth (~$50, see
    docs/fill-model-calibration-2026-07.md); larger clips do not fill at
    modeled prices."""
    equity = equity0
    peak = equity0
    max_dd = 0.0
    max_dd_pct = 0.0
    deployed = 0.0
    open_positions: list[tuple[int, float, float]] = []  # (close_ns, scale, pnl)
    skipped_no_cash = 0
    taken = 0
    day_pnl: dict[str, float] = {}
    trough_day = ""

    def settle_due(now_ns: int):
        nonlocal equity, peak, max_dd, max_dd_pct, deployed, trough_day
        due = [p for p in open_positions if p[0] <= now_ns]
        if not due:
            return
        due.sort(key=lambda p: p[0])
        for close_ns, scale, ref in due:
            open_positions.remove((close_ns, scale, ref))
            pnl = scale * ref["pnl"]
            cost = scale * ref["_cost"]
            deployed -= cost
            equity += pnl
            day = ref.get("_day", "")
            day_pnl[day] = day_pnl.get(day, 0.0) + pnl
            peak = max(peak, equity)
            dd = peak - equity
            if dd > max_dd:
                max_dd = dd
                max_dd_pct = dd / peak * 100 if peak > 0 else 0.0
                trough_day = day

    for t in trades:
        t["_day"] = str(t["fill_ts_ns"] // 1_000_000_000 // 86400)
        settle_due(t["fill_ts_ns"])
        clip = flat_clip if flat_clip is not None else min(frac * equity, ceiling)
        cash = equity - deployed
        clip = min(clip, cash)
        if clip < 1.0:
            skipped_no_cash += 1
            continue
        scale = clip / REF_NOTIONAL
        cost = scale * t["_cost"]
        if cost > cash:
            scale = cash / t["_cost"] if t["_cost"] > 0 else 0.0
            if scale * REF_NOTIONAL < 1.0:
                skipped_no_cash += 1
                continue
        deployed += scale * t["_cost"]
        open_positions.append((t["_close_ns"], scale, t))
        taken += 1

    settle_due(1 << 62)
    worst_day = min(day_pnl.values()) if day_pnl else 0.0
    return {
        "final": equity,
        "return_pct": (equity / equity0 - 1) * 100,
        "max_dd_usd": max_dd,
        "max_dd_pct": max_dd_pct,
        "taken": taken,
        "skipped_no_cash": skipped_no_cash,
        "worst_day_usd": worst_day,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--runs", nargs="+", required=True)
    ap.add_argument("--equity", type=float, default=850.0)
    ap.add_argument("--fracs", default="0.005,0.01,0.02")
    ap.add_argument("--flat-clips", default="10")
    ap.add_argument("--ceiling", type=float, default=50.0)
    args = ap.parse_args()

    trades = load_trades(args.runs)
    print(f"loaded {len(trades)} trades from {args.runs}")
    print(
        f"{'policy':<14}{'final $':>10}{'ret %':>8}{'maxDD $':>9}{'maxDD %':>9}"
        f"{'worst day':>11}{'taken':>7}{'no-cash':>8}"
    )
    for frac in [float(x) for x in args.fracs.split(",") if x]:
        r = simulate(trades, args.equity, frac, None, args.ceiling)
        print(
            f"{'frac ' + format(frac, '.3f'):<14}{r['final']:>10.0f}{r['return_pct']:>8.1f}"
            f"{r['max_dd_usd']:>9.0f}{r['max_dd_pct']:>9.1f}{r['worst_day_usd']:>11.0f}"
            f"{r['taken']:>7}{r['skipped_no_cash']:>8}"
        )
    for clip in [float(x) for x in args.flat_clips.split(",") if x]:
        r = simulate(trades, args.equity, None, clip, args.ceiling)
        print(
            f"{'flat $' + format(clip, 'g'):<14}{r['final']:>10.0f}{r['return_pct']:>8.1f}"
            f"{r['max_dd_usd']:>9.0f}{r['max_dd_pct']:>9.1f}{r['worst_day_usd']:>11.0f}"
            f"{r['taken']:>7}{r['skipped_no_cash']:>8}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
