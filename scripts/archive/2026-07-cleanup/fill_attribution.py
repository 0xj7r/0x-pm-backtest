#!/usr/bin/env python3
"""Summarize per-fill attribution from walk-forward market JSONL."""

import argparse
import datetime as dt
import json
import os
import subprocess
import tempfile
import sys
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any, Dict, Iterable, TextIO


AWS_PROFILE: str | None = None


def materialize(path: str) -> tuple[Path, bool]:
    if not path.startswith("s3://"):
        return Path(path), False
    tmp = tempfile.NamedTemporaryFile(prefix="pm-markets-", suffix=".jsonl", delete=False)
    tmp.close()
    cmd = ["aws", "s3", "cp", path, tmp.name]
    env = os.environ.copy()
    if AWS_PROFILE:
        env["AWS_PROFILE"] = AWS_PROFILE
    try:
        subprocess.run(cmd, check=True, capture_output=True, text=True, env=env)
    except subprocess.CalledProcessError as exc:
        Path(tmp.name).unlink(missing_ok=True)
        stderr = exc.stderr.strip()
        raise RuntimeError(f"aws s3 cp failed for {path}: {stderr}") from exc
    return Path(tmp.name), True


def open_input(path: str) -> tuple[TextIO, bool]:
    if path == "-":
        return sys.stdin, False
    materialized, should_delete = materialize(path)
    return materialized.open(), should_delete


def iter_rows(f: Iterable[str]) -> Iterable[Dict[str, Any]]:
    for line in f:
        line = line.strip()
        if line:
            yield json.loads(line)


def strategy_result(row: Dict[str, Any], strategy: str) -> Dict[str, Any]:
    per_strategy = row.get("per_strategy") or {}
    return per_strategy.get(strategy) or {}


def fill_won(row: Dict[str, Any], side: str) -> bool | None:
    outcome = str(row.get("outcome_label") or "").lower()
    if not outcome:
        return None
    if side == "BuyYes":
        return outcome in ("yes", "up")
    if side == "BuyNo":
        return outcome in ("no", "down")
    return None


def price_bucket(price: float) -> str:
    if price < 0.01:
        return "<1c"
    if price < 0.04:
        return "1-4c"
    if price < 0.06:
        return "4-6c"
    if price < 0.08:
        return "6-8c"
    if price < 0.10:
        return "8-10c"
    if price < 0.20:
        return "10-20c"
    if price < 0.50:
        return "20-50c"
    if price < 0.70:
        return "50-70c"
    if price < 0.85:
        return "70-85c"
    if price < 0.95:
        return "85-95c"
    return "95c+"


def ts_iso(fill: Dict[str, Any]) -> str:
    ts_ns = int(fill.get("ts_ns") or 0)
    if ts_ns <= 0:
        return "n/a"
    return dt.datetime.fromtimestamp(ts_ns / 1e9, tz=dt.timezone.utc).isoformat()


def fmt(value: float, decimals: int = 2) -> str:
    return f"{value:.{decimals}f}"


def settled_fill_pnl(row: Dict[str, Any], fill: Dict[str, Any]) -> float | None:
    won = fill_won(row, str(fill.get("side") or "unknown"))
    if won is None:
        return None
    shares = float(fill.get("shares") or 0.0)
    notional = float(fill.get("notional") or 0.0)
    return (shares if won else 0.0) - notional


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl", help="per-market walk-forward JSONL, or '-' for stdin")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--recent", type=int, default=12)
    parser.add_argument("--aws-profile", help="AWS profile for s3:// inputs")
    args = parser.parse_args()
    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    rows = 0
    fills = []
    f, should_delete = open_input(args.markets_jsonl)
    path_to_delete = Path(f.name) if should_delete else None
    try:
        with f:
            for row in iter_rows(f):
                rows += 1
                strat = strategy_result(row, args.strategy)
                for fill in strat.get("fills_detail") or []:
                    fills.append((row, fill))
    finally:
        if path_to_delete is not None:
            path_to_delete.unlink(missing_ok=True)

    by_tag: Dict[str, Dict[str, Any]] = defaultdict(
        lambda: {
            "fills": 0,
            "notional": 0.0,
            "shares": 0.0,
            "prices": [],
            "yes": 0,
            "no": 0,
            "wins": 0,
            "losses": 0,
            "settled_pnl": 0.0,
            "settled_count": 0,
            "buckets": defaultdict(
                lambda: {"fills": 0, "notional": 0.0, "wins": 0, "losses": 0, "settled_pnl": 0.0}
            ),
        }
    )
    side_counts: Counter[str] = Counter()
    buckets: Counter[str] = Counter()

    for row, fill in fills:
        tag = str(fill.get("tag") or "unknown")
        side = str(fill.get("side") or "unknown")
        price = float(fill.get("price") or 0.0)
        shares = float(fill.get("shares") or 0.0)
        notional = float(fill.get("notional") or 0.0)

        stat = by_tag[tag]
        stat["fills"] += 1
        stat["notional"] += notional
        stat["shares"] += shares
        stat["prices"].append(price)
        if side == "BuyYes":
            stat["yes"] += 1
        elif side == "BuyNo":
            stat["no"] += 1
        side_counts[side] += 1
        buckets[price_bucket(price)] += 1

        won = fill_won(row, side)
        if won is True:
            stat["wins"] += 1
        elif won is False:
            stat["losses"] += 1
        settled_pnl = settled_fill_pnl(row, fill)
        if settled_pnl is not None:
            stat["settled_pnl"] += settled_pnl
            stat["settled_count"] += 1
            bucket_stat = stat["buckets"][price_bucket(price)]
            bucket_stat["fills"] += 1
            bucket_stat["notional"] += notional
            bucket_stat["settled_pnl"] += settled_pnl
            if won is True:
                bucket_stat["wins"] += 1
            elif won is False:
                bucket_stat["losses"] += 1

    print(f"markets={rows} fills={len(fills)}")
    print(f"side_counts={dict(side_counts)}")
    print(f"price_buckets={dict(buckets)}")
    print("by_tag:")
    for tag, stat in sorted(by_tag.items(), key=lambda kv: kv[1]["notional"], reverse=True):
        prices = stat["prices"]
        avg_px = sum(prices) / len(prices) if prices else 0.0
        resolved = stat["wins"] + stat["losses"]
        hit_rate = stat["wins"] / resolved if resolved else 0.0
        print(
            f"  {tag}: fills={stat['fills']} yes={stat['yes']} no={stat['no']} "
            f"notional=${fmt(stat['notional'])} shares={fmt(stat['shares'], 1)} "
            f"avg_px={fmt(avg_px, 3)} side_wr={fmt(hit_rate * 100.0, 1)}% "
            f"settled_pnl=${fmt(stat['settled_pnl'])}"
        )
        for bucket, bucket_stat in sorted(stat["buckets"].items()):
            bucket_resolved = bucket_stat["wins"] + bucket_stat["losses"]
            bucket_hit = bucket_stat["wins"] / bucket_resolved if bucket_resolved else 0.0
            print(
                f"    {bucket}: fills={bucket_stat['fills']} notional=${fmt(bucket_stat['notional'])} "
                f"wr={fmt(bucket_hit * 100.0, 1)}% settled_pnl=${fmt(bucket_stat['settled_pnl'])}"
            )

    if fills and args.recent > 0:
        print("recent:")
        for row, fill in fills[-args.recent :]:
            print(
                f"  {ts_iso(fill)} {row.get('slug')} outcome={row.get('outcome_label')} "
                f"{fill.get('tag')} {fill.get('side')} px={fmt(float(fill.get('price') or 0.0), 3)} "
                f"shares={fmt(float(fill.get('shares') or 0.0), 1)} "
                f"notional=${fmt(float(fill.get('notional') or 0.0))}"
            )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
