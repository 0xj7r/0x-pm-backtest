#!/usr/bin/env python3
"""Screen last-second window-delta snipes on cached BTC 5m markets.

This is a hypothesis filter for the common "T-10s window delta is king" claim.
For each market and decision time, it:

  1. reads Binance spot at window open and at the decision timestamp,
  2. chooses the leading side if abs(window delta) exceeds a threshold,
  3. buys at the Polymarket visible ask proxy after an execution latency, and
  4. scores one-share gross resolution PnL.

The local cache is YES-token only, so NO ask is mirrored as 1 - YES bid. Use the
Rust engine / full S3 two-token book before treating any row as deployable.
"""

from __future__ import annotations

import argparse
import glob
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import numpy as np

import mm_paired_sim as S


@dataclass(frozen=True)
class SnipeRule:
    t_left: float
    min_abs_delta_bps: float
    max_price: float
    latency_s: float


def spot_at(ts: np.ndarray, px: np.ndarray, when: float) -> float | None:
    idx = np.searchsorted(ts, when, side="right") - 1
    if idx < 0:
        return None
    value = float(px[idx])
    return value if math.isfinite(value) and value > 0.0 else None


def book_at_tleft(book: dict[str, Any], t_left: float) -> tuple[float, float] | None:
    wall = book["wall"]
    idx = np.searchsorted(wall, -t_left, side="right") - 1
    if idx < 0:
        return None
    bid = float(book["bid"][idx])
    ask = float(book["ask"][idx])
    if not (math.isfinite(bid) and math.isfinite(ask)):
        return None
    if bid <= 0.0 or ask <= 0.0 or ask < bid:
        return None
    return bid, ask


def one_share_pnl(price: float, buy_yes: bool, yes_wins: bool) -> float:
    return (1.0 - price) if buy_yes == yes_wins else -price


def evaluate_rule(parsed: list[tuple[Any, int, Any, Any, str]], rule: SnipeRule) -> dict[str, Any]:
    trades = []
    for book, close, _trades, bin_day, date in parsed:
        yes_wins, spot_ts, spot_px = S.binance_outcome_and_vol(bin_day, close)
        if yes_wins is None:
            continue
        start_px = spot_at(spot_ts, spot_px, close - S.WINDOW)
        decision_px = spot_at(spot_ts, spot_px, close - rule.t_left)
        if start_px is None or decision_px is None:
            continue
        delta_bps = (decision_px / start_px - 1.0) * 10_000.0
        if abs(delta_bps) < rule.min_abs_delta_bps:
            continue
        buy_yes = delta_bps >= 0.0
        exec_t_left = max(0.0, rule.t_left - rule.latency_s)
        snap = book_at_tleft(book, exec_t_left)
        if snap is None:
            continue
        yes_bid, yes_ask = snap
        price = yes_ask if buy_yes else max(0.0, 1.0 - yes_bid)
        if price <= 0.0 or price > rule.max_price:
            continue
        pnl = one_share_pnl(price, buy_yes, bool(yes_wins))
        trades.append(
            {
                "date": date,
                "close": close,
                "pnl": pnl,
                "price": price,
                "delta_bps": delta_bps,
                "buy_yes": buy_yes,
                "yes_wins": bool(yes_wins),
            }
        )

    n = len(trades)
    pnl_total = sum(t["pnl"] for t in trades)
    wins = sum(1 for t in trades if t["pnl"] > 0.0)
    prices = [t["price"] for t in trades]
    deltas = [abs(t["delta_bps"]) for t in trades]
    return {
        "t_left": rule.t_left,
        "latency_s": rule.latency_s,
        "min_abs_delta_bps": rule.min_abs_delta_bps,
        "max_price": rule.max_price,
        "trades": n,
        "pnl_per_share": pnl_total,
        "pnl_per_100": pnl_total * 100.0,
        "hit_rate": wins / n if n else 0.0,
        "avg_price": sum(prices) / n if n else 0.0,
        "avg_abs_delta_bps": sum(deltas) / n if n else 0.0,
        "worst": min((t["pnl"] for t in trades), default=0.0),
        "best": max((t["pnl"] for t in trades), default=0.0),
    }


def markdown(rows: list[dict[str, Any]], n_markets: int, n_days: int) -> str:
    lines = [
        "# Last-Second Snipe Screen",
        "",
        f"Dataset: local cached May 7-20 BTC 5m markets, parsed markets={n_markets}, days={n_days}.",
        "",
        "Gross one-share resolution PnL. Decision uses Binance window delta; execution uses Polymarket visible ask proxy after latency. NO ask is mirrored from YES bid because local cache is YES-token only.",
        "",
        "| T-left | Latency | Min abs delta bps | Max price | Trades | PnL/share | PnL/100sh | Hit | Avg price | Avg abs delta bps | Worst |",
        "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for row in rows:
        lines.append(
            f"| {row['t_left']:.0f}s | {row['latency_s']:.1f}s | "
            f"{row['min_abs_delta_bps']:.1f} | {row['max_price']:.2f} | "
            f"{row['trades']} | {row['pnl_per_share']:.2f} | {row['pnl_per_100']:.2f} | "
            f"{100.0 * row['hit_rate']:.1f}% | {row['avg_price']:.3f} | "
            f"{row['avg_abs_delta_bps']:.2f} | {row['worst']:.3f} |"
        )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--out-md", type=Path, default=Path("docs/last_second_snipe_screen_2026-06-01.md"))
    parser.add_argument("--latency", type=float, default=1.0)
    args = parser.parse_args()

    book_files = sorted(glob.glob(f"{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet"))
    if args.limit:
        step = max(1, len(book_files) // args.limit)
        book_files = book_files[::step][: args.limit]
    dates = sorted({[p for p in f.split("/") if p.startswith("date=")][0].split("=")[1] for f in book_files})
    parsed = S.load_all_markets(book_files, {})

    rows = []
    for t_left in (30.0, 20.0, 10.0, 5.0):
        for threshold in (1.0, 2.0, 5.0, 10.0, 15.0):
            for max_price in (0.80, 0.90, 0.95, 0.98):
                rows.append(
                    evaluate_rule(
                        parsed,
                        SnipeRule(
                            t_left=t_left,
                            min_abs_delta_bps=threshold,
                            max_price=max_price,
                            latency_s=args.latency,
                        ),
                    )
                )
    rows.sort(key=lambda r: (r["pnl_per_share"], r["trades"]), reverse=True)
    args.out_md.parent.mkdir(parents=True, exist_ok=True)
    args.out_md.write_text(markdown(rows, len(parsed), len(dates)))
    print(f"wrote {args.out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
