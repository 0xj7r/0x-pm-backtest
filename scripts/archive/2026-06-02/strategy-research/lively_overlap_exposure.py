#!/usr/bin/env python3
"""Estimate overlapping crypto-direction exposure from a walk-forward JSONL run."""

from __future__ import annotations

import argparse
import json
import re
from collections import defaultdict
from pathlib import Path
from typing import Any


EPOCH_RE = re.compile(r"-(\d{10})$")


def duration_secs(slug: str) -> int:
    s = slug.lower()
    if "-updown-15m-" in s:
        return 900
    if "-updown-4h-" in s:
        return 14_400
    return 300


def asset_from_slug(slug: str) -> str:
    s = slug.lower()
    if s.startswith(("btc-", "bitcoin-")):
        return "btc"
    if s.startswith(("eth-", "ethereum-")):
        return "eth"
    if s.startswith(("sol-", "solana-")):
        return "sol"
    if s.startswith("xrp-"):
        return "xrp"
    return "other"


def horizon_from_slug(slug: str) -> str:
    s = slug.lower()
    if "-updown-5m-" in s:
        return "5m"
    if "-updown-15m-" in s:
        return "15m"
    if "-updown-4h-" in s:
        return "4h"
    if "up-or-down" in s:
        return "hourly"
    return "unknown"


def side_sign(side: str) -> int:
    if side == "BuyYes":
        return 1
    if side == "BuyNo":
        return -1
    return 0


def add_metric_max(metrics: dict[str, float], key: str, value: float) -> None:
    if value > metrics.get(key, 0.0):
        metrics[key] = value


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--markets-jsonl", required=True)
    parser.add_argument("--strategy", default="lively_wallet_taker")
    parser.add_argument("--out")
    args = parser.parse_args()

    events: list[tuple[int, int, str, str, str, float, float]] = []
    # (ts_ns, kind, asset, horizon, side, signed_cost, gross_cost). kind: +1 fill, -1 expiry.
    fills = 0
    with Path(args.markets_jsonl).open() as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            slug = str(row.get("slug") or "")
            close_ts = int(row.get("close_ts") or 0)
            if not slug or close_ts <= 0:
                continue
            strategy = (row.get("per_strategy") or {}).get(args.strategy)
            if not strategy:
                continue
            asset = asset_from_slug(slug)
            horizon = horizon_from_slug(slug)
            close_ns = close_ts * 1_000_000_000
            for fill in strategy.get("fills_detail") or []:
                side = str(fill.get("side") or "")
                sign = side_sign(side)
                notional = float(fill.get("notional") or 0.0)
                ts_ns = int(fill.get("ts_ns") or 0)
                if sign == 0 or notional <= 0.0 or ts_ns <= 0:
                    continue
                signed = sign * notional
                events.append((ts_ns, 1, asset, horizon, side, signed, notional))
                events.append((close_ns, -1, asset, horizon, side, -signed, -notional))
                fills += 1

    events.sort()
    signed_by_asset: dict[str, float] = defaultdict(float)
    gross_by_asset: dict[str, float] = defaultdict(float)
    signed_by_lane: dict[str, float] = defaultdict(float)
    gross_by_lane: dict[str, float] = defaultdict(float)
    metrics: dict[str, float] = {}

    for ts_ns, kind, asset, horizon, _side, signed_delta, gross_delta in events:
        lane = f"{asset}_{horizon}"
        signed_by_asset[asset] += signed_delta
        gross_by_asset[asset] += gross_delta
        signed_by_lane[lane] += signed_delta
        gross_by_lane[lane] += gross_delta

        total_gross = sum(v for v in gross_by_asset.values() if v > 0.0)
        crypto_signed = signed_by_asset["btc"] + signed_by_asset["eth"]
        crypto_gross = max(0.0, gross_by_asset["btc"]) + max(0.0, gross_by_asset["eth"])
        add_metric_max(metrics, "peak_total_open_gross_usd", total_gross)
        add_metric_max(metrics, "peak_btc_open_gross_usd", max(0.0, gross_by_asset["btc"]))
        add_metric_max(metrics, "peak_eth_open_gross_usd", max(0.0, gross_by_asset["eth"]))
        add_metric_max(metrics, "peak_crypto_open_gross_usd", crypto_gross)
        add_metric_max(metrics, "peak_abs_crypto_signed_cost_usd", abs(crypto_signed))
        add_metric_max(metrics, "peak_abs_btc_signed_cost_usd", abs(signed_by_asset["btc"]))
        add_metric_max(metrics, "peak_abs_eth_signed_cost_usd", abs(signed_by_asset["eth"]))
        for key, value in gross_by_lane.items():
            add_metric_max(metrics, f"peak_{key}_open_gross_usd", max(0.0, value))

    out = {
        "path": args.markets_jsonl,
        "strategy": args.strategy,
        "fills": fills,
        "note": "cost-notional proxy; binary payoff convexity and mark-to-market are not modeled",
        **{k: round(v, 4) for k, v in sorted(metrics.items())},
    }
    payload = json.dumps(out, indent=2, sort_keys=True)
    if args.out:
        Path(args.out).write_text(payload + "\n")
    print(payload)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
