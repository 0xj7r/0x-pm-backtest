#!/usr/bin/env python3
"""Offline fill-level rescoring for CompetitorRecycler experiments.

This does not re-run the engine. It recomputes settlement PnL from the fills
already emitted by a walk-forward run, optionally filtering fills by live-safe
features recorded at fill time. Use it for hypothesis screening only.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
from pathlib import Path


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser()
    p.add_argument("--markets", required=True, type=Path)
    p.add_argument("--max-abs-flow-imbal-30s", type=float)
    p.add_argument("--max-side-edge-vs-fill", type=float)
    p.add_argument("--min-seconds-to-close", type=float)
    return p.parse_args()


def fill_passes(fill: dict, args: argparse.Namespace) -> bool:
    if (
        args.max_abs_flow_imbal_30s is not None
        and abs(fill.get("binance_flow_imbal_30s", 0.0)) > args.max_abs_flow_imbal_30s
    ):
        return False
    if (
        args.max_side_edge_vs_fill is not None
        and fill.get("side_edge_vs_fill", 0.0) > args.max_side_edge_vs_fill
    ):
        return False
    if (
        args.min_seconds_to_close is not None
        and fill.get("seconds_to_close", 0.0) < args.min_seconds_to_close
    ):
        return False
    return True


def main() -> int:
    args = parse_args()
    equity = 1000.0
    peak = equity
    max_dd = 0.0
    total = 0.0
    kept_fills = 0
    total_fills = 0
    by_date: dict[str, float] = {}
    worst_market = ("", 0.0)
    best_market = ("", 0.0)

    with args.markets.open() as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            date = dt.datetime.fromtimestamp(row["close_ts"], dt.UTC).date().isoformat()
            yes_win = row["outcome_label"] in ("Up", "Yes")
            market_pnl = 0.0
            fills = row["per_strategy"]["competitor_recycler"].get("fills_detail", [])
            total_fills += len(fills)
            for fill in fills:
                if not fill_passes(fill, args):
                    continue
                kept_fills += 1
                win = (fill["side"] == "BuyYes" and yes_win) or (
                    fill["side"] == "BuyNo" and not yes_win
                )
                market_pnl += (
                    (fill["shares"] if win else 0.0)
                    - fill["notional"]
                    + fill.get("rebate_usdc", 0.0)
                )
            total += market_pnl
            by_date[date] = by_date.get(date, 0.0) + market_pnl
            equity += market_pnl
            peak = max(peak, equity)
            max_dd = max(max_dd, (peak - equity) / peak if peak else 0.0)
            if market_pnl < worst_market[1]:
                worst_market = (row["slug"], market_pnl)
            if market_pnl > best_market[1]:
                best_market = (row["slug"], market_pnl)

    print(f"total_pnl={total:.2f}")
    print(f"end_equity={equity:.2f}")
    print(f"max_drawdown_pct={max_dd * 100:.2f}")
    print(f"kept_fills={kept_fills}")
    print(f"total_fills={total_fills}")
    print(f"fill_keep_rate={kept_fills / total_fills if total_fills else 0.0:.4f}")
    print(f"worst_market={worst_market[0]} {worst_market[1]:.2f}")
    print(f"best_market={best_market[0]} {best_market[1]:.2f}")
    print("by_date:")
    for date in sorted(by_date):
        print(f"  {date}: {by_date[date]:+.2f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
