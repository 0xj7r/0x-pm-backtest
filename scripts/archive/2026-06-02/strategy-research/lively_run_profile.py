#!/usr/bin/env python3
"""Profile buy-only taker runs against the target wallet execution shape."""

from __future__ import annotations

import argparse
import json
import math
from collections import defaultdict
from pathlib import Path
from statistics import mean, median, pstdev
from typing import Any


def quantile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, int(round((len(ordered) - 1) * q))))
    return ordered[idx]


def lane_for_slug(slug: str) -> str:
    s = slug.lower()
    asset = "other"
    if s.startswith(("btc-", "bitcoin-")):
        asset = "btc"
    elif s.startswith(("eth-", "ethereum-")):
        asset = "eth"

    horizon = "unknown"
    if "-5m-" in s:
        horizon = "5m"
    elif "-15m-" in s:
        horizon = "15m"
    elif "-4h-" in s:
        horizon = "4h"
    elif "up-or-down" in s:
        horizon = "hourly"
    return f"{asset}_{horizon}"


def fill_pnl(fill: dict[str, Any], yes_resolved: bool) -> float:
    side = str(fill.get("side", ""))
    shares = float(fill.get("shares") or 0.0)
    price = float(fill.get("price") or 0.0)
    if side == "BuyYes":
        return shares * ((1.0 - price) if yes_resolved else -price)
    if side == "BuyNo":
        return shares * ((1.0 - price) if not yes_resolved else -price)
    return 0.0


def empty_market() -> dict[str, float]:
    return {
        "yes_shares": 0.0,
        "yes_notional": 0.0,
        "no_shares": 0.0,
        "no_notional": 0.0,
        "pnl": 0.0,
        "notional": 0.0,
        "fills": 0.0,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--markets-jsonl", required=True)
    parser.add_argument("--strategy", default="lively_momentum_taker")
    parser.add_argument("--out", default=None)
    args = parser.parse_args()

    path = Path(args.markets_jsonl)
    by_market: dict[str, dict[str, float]] = {}
    by_lane: dict[str, dict[str, float]] = defaultdict(empty_market)
    clip_usd: list[float] = []
    market_order: list[tuple[int, str]] = []
    maker_count = 0
    taker_count = 0

    with path.open() as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            slug = str(row.get("slug") or "")
            strategy = (row.get("per_strategy") or {}).get(args.strategy)
            if not strategy:
                continue
            fills = strategy.get("fills_detail") or []
            if not fills:
                continue
            yes_resolved = bool(strategy.get("yes_resolved"))
            lane = lane_for_slug(slug)
            rec = by_market.setdefault(slug, empty_market())
            market_order.append((int(row.get("close_ts") or 0), slug))
            for fill in fills:
                side = str(fill.get("side") or "")
                shares = float(fill.get("shares") or 0.0)
                notional = float(fill.get("notional") or 0.0)
                pnl = fill_pnl(fill, yes_resolved)
                if shares <= 0.0 or notional <= 0.0:
                    continue
                if side == "BuyYes":
                    rec["yes_shares"] += shares
                    rec["yes_notional"] += notional
                    by_lane[lane]["yes_shares"] += shares
                    by_lane[lane]["yes_notional"] += notional
                elif side == "BuyNo":
                    rec["no_shares"] += shares
                    rec["no_notional"] += notional
                    by_lane[lane]["no_shares"] += shares
                    by_lane[lane]["no_notional"] += notional
                else:
                    continue
                rec["pnl"] += pnl
                rec["notional"] += notional
                rec["fills"] += 1.0
                by_lane[lane]["pnl"] += pnl
                by_lane[lane]["notional"] += notional
                by_lane[lane]["fills"] += 1.0
                if bool(fill.get("maker")):
                    maker_count += 1
                else:
                    taker_count += 1
                clip_usd.append(notional)

    two_sided = 0
    arb_locked = 0
    summed_entries: list[float] = []
    active_markets = 0
    pnls_by_market: dict[str, float] = {}
    for slug, rec in by_market.items():
        if rec["fills"] <= 0.0:
            continue
        active_markets += 1
        pnls_by_market[slug] = rec["pnl"]
        if rec["yes_shares"] > 0.0 and rec["no_shares"] > 0.0:
            two_sided += 1
            yes_avg = rec["yes_notional"] / rec["yes_shares"]
            no_avg = rec["no_notional"] / rec["no_shares"]
            summed = yes_avg + no_avg
            summed_entries.append(summed)
            if summed < 1.0:
                arb_locked += 1

    equity = 0.0
    peak = 0.0
    max_dd = 0.0
    for _, slug in sorted(set(market_order)):
        equity += pnls_by_market.get(slug, 0.0)
        peak = max(peak, equity)
        max_dd = min(max_dd, equity - peak)

    lane_summary = {}
    for lane, rec in sorted(by_lane.items()):
        lane_summary[lane] = {
            "pnl": round(rec["pnl"], 4),
            "notional": round(rec["notional"], 4),
            "fills": int(rec["fills"]),
            "roi": round(rec["pnl"] / rec["notional"], 6) if rec["notional"] > 0 else 0.0,
        }

    out = {
        "strategy": args.strategy,
        "path": str(path),
        "market_count": active_markets,
        "fill_count": len(clip_usd),
        "maker_fill_count": maker_count,
        "taker_fill_count": taker_count,
        "taker_fill_fraction": round(taker_count / len(clip_usd), 4) if clip_usd else 0.0,
        "pnl": round(sum(pnls_by_market.values()), 4),
        "notional": round(sum(clip_usd), 4),
        "max_drawdown_usd": round(max_dd, 4),
        "two_sided_markets": two_sided,
        "two_sided_fraction": round(two_sided / active_markets, 4) if active_markets else 0.0,
        "arb_locked_markets": arb_locked,
        "arb_locked_fraction_of_two_sided": round(arb_locked / two_sided, 4) if two_sided else 0.0,
        "summed_avg_entry": {
            "count": len(summed_entries),
            "mean": round(mean(summed_entries), 4) if summed_entries else 0.0,
            "median": round(median(summed_entries), 4) if summed_entries else 0.0,
            "p25": round(quantile(summed_entries, 0.25), 4),
            "p75": round(quantile(summed_entries, 0.75), 4),
        },
        "sizing": {
            "count": len(clip_usd),
            "min_usd": round(min(clip_usd), 4) if clip_usd else 0.0,
            "p25_usd": round(quantile(clip_usd, 0.25), 4),
            "median_usd": round(median(clip_usd), 4) if clip_usd else 0.0,
            "mean_usd": round(mean(clip_usd), 4) if clip_usd else 0.0,
            "p75_usd": round(quantile(clip_usd, 0.75), 4),
            "p95_usd": round(quantile(clip_usd, 0.95), 4),
            "max_usd": round(max(clip_usd), 4) if clip_usd else 0.0,
            "stdev_usd": round(pstdev(clip_usd), 4) if len(clip_usd) > 1 else 0.0,
        },
        "lanes": lane_summary,
    }

    payload = json.dumps(out, indent=2, sort_keys=True)
    if args.out:
        Path(args.out).write_text(payload + "\n")
    print(payload)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
