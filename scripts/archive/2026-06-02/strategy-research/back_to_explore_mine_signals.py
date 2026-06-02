#!/usr/bin/env python3
"""
Mine proper, data-driven signals for the BackToExploreTaker strategy
from real Polymarket activity API data for the target wallet.

Usage:
  python scripts/back_to_explore_mine_signals.py \
    --input data/runs/wallet_forensics/back_to_explore_raw_activity.json \
    --output data/signals/back_to_explore/priors_btc_eth.json

Output is designed to be consumed (or manually transcribed) into the Rust
BackToExploreConfig and time_prior logic for signal-driven development.
"""

from __future__ import annotations

import argparse
import json
import math
import re
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import median, quantiles
from typing import Any


BTC_ETH_RE = re.compile(r"^(btc|eth|bitcoin|ethereum)", re.IGNORECASE)
HORIZON_RE = re.compile(r"-(5m|15m|1h|4h)-", re.IGNORECASE)


def parse_horizon(slug: str) -> str:
    m = HORIZON_RE.search(slug or "")
    return m.group(1).lower() if m else "other"


def is_btc_eth(slug: str, title: str) -> bool:
    text = f"{slug} {title}".lower()
    return bool(BTC_ETH_RE.search(text))


def utc_hour(ts: int) -> int:
    return datetime.fromtimestamp(ts, tz=timezone.utc).hour


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()

    rows: list[dict[str, Any]] = json.loads(args.input.read_text())

    # Filter to trades only + BTC/ETH
    trades = [
        r
        for r in rows
        if r.get("type") == "TRADE"
        and is_btc_eth(r.get("slug", ""), r.get("title", ""))
    ]

    total_trades = len(trades)
    unique_conditions = len({r.get("conditionId") for r in trades if r.get("conditionId")})

    # Clip sizes (usdcSize)
    sizes = [float(r["usdcSize"]) for r in trades if float(r.get("usdcSize", 0)) > 0]
    size_stats = {}
    if sizes:
        qs = quantiles(sizes, n=100)
        size_stats = {
            "count": len(sizes),
            "min": round(min(sizes), 4),
            "p25": round(qs[24], 4),
            "median": round(median(sizes), 4),
            "mean": round(sum(sizes) / len(sizes), 4),
            "p75": round(qs[74], 4),
            "p95": round(qs[94], 4),
            "p99": round(qs[98], 4),
            "max": round(max(sizes), 4),
        }

    # Hourly participation (raw counts + normalized multipliers relative to median hour)
    hour_counts: Counter[int] = Counter()
    for r in trades:
        hour_counts[utc_hour(r["timestamp"])] += 1

    if hour_counts:
        median_count = median(list(hour_counts.values()))
        hourly_mult = {
            str(h): round(c / median_count, 3) if median_count > 0 else 1.0
            for h, c in sorted(hour_counts.items())
        }
    else:
        hourly_mult = {}

    # Horizon mix (BTC/ETH 5m/15m focus)
    horizon_counts: Counter[str] = Counter()
    for r in trades:
        horizon_counts[parse_horizon(r.get("slug", ""))] += 1

    # Rough two-sided signal: markets with multiple trades + both outcomeIndex values
    by_cond: dict[str, list[int]] = defaultdict(list)
    for r in trades:
        cond = r.get("conditionId")
        if cond:
            by_cond[cond].append(int(r.get("outcomeIndex", -1)))

    multi_trade_markets = 0
    both_legs_markets = 0
    for cond, legs in by_cond.items():
        if len(legs) > 1:
            multi_trade_markets += 1
            if len(set(legs)) >= 2:
                both_legs_markets += 1

    two_sided_rate = (
        both_legs_markets / multi_trade_markets if multi_trade_markets > 0 else 0.0
    )

    # Recommended config values derived from data (for easy copy into Rust)
    recommended = {
        "base_clip_usdc": round(size_stats.get("median", 8.8), 1),
        "high_activity_hours": sorted(
            [int(h) for h, mult in hourly_mult.items() if mult >= 1.2]
        ),
        "refresh_secs": 2.8,
        "two_sided_preference": round(1.0 + two_sided_rate, 2),
        "min_pair_cost_for_two_sided": 0.96,
    }

    out = {
        "source": "polymarket data-api /activity",
        "wallet": "0xb55fa1296e6ec55d0ce53d93b9237389f11764d4",
        "pseudonym": "Lively-Authenticity",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "raw_trades_analyzed": total_trades,
        "unique_markets": unique_conditions,
        "size_stats": size_stats,
        "hourly_participation_multiplier": hourly_mult,
        "horizon_mix": dict(horizon_counts),
        "two_sided": {
            "multi_trade_markets": multi_trade_markets,
            "both_legs_markets": both_legs_markets,
            "rate": round(two_sided_rate, 4),
        },
        "recommended_config_defaults": recommended,
        "notes": "Use hourly_participation_multiplier to drive time_prior. Use size_stats for variable sizing model. High both_legs rate justifies strong pair/two-sided logic.",
    }

    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(out, indent=2, sort_keys=True) + "\n")
    print(f"Wrote signal priors to {args.output}")
    print(f"  Trades: {total_trades}, Markets: {unique_conditions}")
    print(f"  Two-sided rate on repeats: {two_sided_rate:.1%}")
    print(f"  Recommended base_clip: {recommended['base_clip_usdc']}")
    print(f"  High activity hours: {recommended['high_activity_hours']}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
