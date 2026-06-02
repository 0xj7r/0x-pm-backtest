#!/usr/bin/env python3
"""Summarize Polymarket wallet activity for crypto up/down strategy forensics.

The Polymarket public activity API is enough to diagnose the broad operating
style: asset/horizon mix, both-leg accumulation, cheap-tail/late-cert buying,
and nested 5m/15m/1h exposure. This script intentionally avoids making PnL
claims unless a settled outcome source is joined later.
"""

from __future__ import annotations

import argparse
import json
import math
import re
import statistics
import sys
import time
import urllib.parse
import urllib.request
from collections import Counter, defaultdict
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Iterable

CRYPTO_SLUG_RE = re.compile(r"^(?P<asset>btc|eth|sol|xrp|doge)-updown-(?P<horizon>5m|15m|1h)-(?P<start>\d+)$")
PRICE_BUCKETS = (
    (0.00, 0.05, "00-05c"),
    (0.05, 0.10, "05-10c"),
    (0.10, 0.25, "10-25c"),
    (0.25, 0.45, "25-45c"),
    (0.45, 0.55, "45-55c"),
    (0.55, 0.75, "55-75c"),
    (0.75, 0.90, "75-90c"),
    (0.90, 1.01, "90-100c"),
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wallet", action="append", required=True, help="Wallet address. Repeatable.")
    parser.add_argument("--limit", type=int, default=500, help="Rows per Polymarket activity request.")
    parser.add_argument("--pages", type=int, default=4, help="Number of activity pages per wallet.")
    parser.add_argument("--start", type=int, help="Optional Unix start timestamp.")
    parser.add_argument("--end", type=int, help="Optional Unix end timestamp.")
    parser.add_argument("--sleep", type=float, default=0.15, help="Delay between API requests.")
    parser.add_argument("--activity-root", type=Path, help="Optional local whale activity root with wallet=*/activity/date=* parquet files.")
    parser.add_argument("--output", type=Path, help="Write JSON summary to this path.")
    args = parser.parse_args()

    result = {"generated_at": datetime.now(UTC).isoformat(), "wallets": {}}
    for wallet in args.wallet:
        if args.activity_root:
            rows = load_local_activity(args.activity_root, wallet)
        else:
            rows = pull_activity(wallet, limit=args.limit, pages=args.pages, start=args.start, end=args.end, sleep_s=args.sleep)
        result["wallets"][wallet.lower()] = analyze_wallet(rows)

    text = json.dumps(result, indent=2, sort_keys=True)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(text + "\n")
    else:
        print(text)
    return 0


def pull_activity(wallet: str, *, limit: int, pages: int, start: int | None, end: int | None, sleep_s: float) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    base_url = "https://data-api.polymarket.com/activity"
    for page in range(pages):
        query = {"user": wallet, "limit": str(limit), "offset": str(page * limit)}
        if start is not None:
            query["start"] = str(start)
        if end is not None:
            query["end"] = str(end)
        url = f"{base_url}?{urllib.parse.urlencode(query)}"
        req = urllib.request.Request(url, headers={"User-Agent": "wallet-activity-forensics/1.0"})
        with urllib.request.urlopen(req, timeout=30.0) as resp:
            payload = json.loads(resp.read().decode("utf-8"))
        batch = payload if isinstance(payload, list) else payload.get("data", [])
        if not batch:
            break
        rows.extend(dict(row) for row in batch)
        if len(batch) < limit:
            break
        time.sleep(sleep_s)
    return rows


def load_local_activity(root: Path, wallet: str) -> list[dict[str, Any]]:
    try:
        import pyarrow.parquet as pq  # type: ignore[import-not-found]
    except ImportError as exc:
        raise SystemExit("pyarrow is required for --activity-root") from exc

    wallet_dir = root / f"wallet={wallet.lower()}" / "activity"
    if not wallet_dir.exists():
        raise SystemExit(f"activity directory not found: {wallet_dir}")
    rows: list[dict[str, Any]] = []
    for path in sorted(wallet_dir.glob("date=*/*.parquet")):
        rows.extend(pq.ParquetFile(path).read().to_pylist())
    return rows


def analyze_wallet(raw_rows: Iterable[dict[str, Any]]) -> dict[str, Any]:
    rows = [normalize(row) for row in raw_rows]
    rows = [row for row in rows if row["type"] == "TRADE" and row["side"] == "BUY" and row["slug_meta"]]
    rows.sort(key=lambda row: (row["timestamp"], row["slug"], row["outcome"], row["price"]))
    if not rows:
        return {"rows": 0, "note": "no crypto up/down BUY trade rows found"}

    by_market: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for row in rows:
        by_market[row["market_key"]].append(row)

    return {
        "rows": len(rows),
        "time_range": {"start": iso(rows[0]["timestamp"]), "end": iso(rows[-1]["timestamp"])},
        "markets": len(by_market),
        "asset_mix": shares(rows, "asset"),
        "horizon_mix": shares(rows, "horizon"),
        "asset_horizon_mix": shares(rows, "asset_horizon"),
        "price_buckets": price_buckets(rows),
        "clip_usdc": numeric_summary([row["usdc_size"] for row in rows if row["usdc_size"] > 0]),
        "phase": phase_summary(rows),
        "market_shape": market_shape(by_market),
        "same_second_bursts": same_second_bursts(rows),
        "nested_horizon": nested_horizon(rows),
        "strategy_class_mix": strategy_class_mix(rows, by_market),
        "top_recent_markets": recent_market_examples(by_market),
    }


def normalize(row: dict[str, Any]) -> dict[str, Any]:
    slug = str(pick(row, "slug", "eventSlug", "marketSlug", "conditionSlug", default="") or "").lower()
    meta = parse_slug(slug)
    timestamp = int(float(pick(row, "timestamp", "time", "createdAt", "created_at", default=0) or 0))
    price = to_float(pick(row, "price", "avgPrice", "matchedPrice", default=0.0))
    size = to_float(pick(row, "size", "shares", "amount", default=0.0))
    usdc = to_float(pick(row, "usdcSize", "usdc_size", "notional", "value", default=0.0))
    if usdc == 0.0 and price and size:
        usdc = price * size
    outcome = str(pick(row, "outcome", "outcomeName", default="unknown") or "unknown")
    asset = meta["asset"].upper() if meta else infer_asset(row, slug)
    horizon = meta["horizon"] if meta else "unknown"
    start = meta["start"] if meta else None
    duration = horizon_seconds(horizon)
    phase = None if start is None or duration <= 0 else (timestamp - start) / duration
    return {
        "timestamp": timestamp,
        "type": str(pick(row, "type", "action", default="") or "").upper(),
        "side": str(pick(row, "side", default="") or "").upper(),
        "outcome": outcome,
        "outcome_index": int(to_float(pick(row, "outcomeIndex", "outcome_index", default=-1))),
        "price": price,
        "size": size,
        "usdc_size": usdc,
        "slug": slug,
        "condition_id": str(pick(row, "conditionId", "condition_id", default="") or ""),
        "market_key": str(pick(row, "conditionId", "condition_id", default="") or slug),
        "asset": asset,
        "horizon": horizon,
        "asset_horizon": f"{asset}_{horizon}",
        "market_start": start,
        "phase": phase,
        "slug_meta": meta,
    }


def parse_slug(slug: str) -> dict[str, Any] | None:
    match = CRYPTO_SLUG_RE.match(slug)
    if not match:
        return None
    return {
        "asset": match.group("asset"),
        "horizon": match.group("horizon"),
        "start": int(match.group("start")),
    }


def horizon_seconds(horizon: str) -> int:
    return {"5m": 300, "15m": 900, "1h": 3600}.get(horizon, 0)


def infer_asset(row: dict[str, Any], slug: str) -> str:
    text = " ".join(str(pick(row, key, default="") or "") for key in ("title", "eventSlug", "slug")) + " " + slug
    text = text.lower()
    for token, label in (("bitcoin", "BTC"), ("btc", "BTC"), ("ethereum", "ETH"), ("eth", "ETH"), ("solana", "SOL"), ("sol", "SOL"), ("xrp", "XRP"), ("doge", "DOGE")):
        if token in text:
            return label
    return "UNKNOWN"


def shares(rows: list[dict[str, Any]], key: str) -> dict[str, Any]:
    counts: Counter[str] = Counter()
    notionals: Counter[str] = Counter()
    for row in rows:
        counts[str(row[key])] += 1
        notionals[str(row[key])] += row["usdc_size"]
    total_count = sum(counts.values())
    total_notional = sum(notionals.values())
    labels = sorted(counts.keys(), key=lambda k: (-notionals[k], k))
    return {
        label: {
            "count": counts[label],
            "count_share": round(counts[label] / total_count, 4) if total_count else 0.0,
            "notional": round(notionals[label], 4),
            "notional_share": round(notionals[label] / total_notional, 4) if total_notional else 0.0,
        }
        for label in labels
    }


def price_buckets(rows: list[dict[str, Any]]) -> dict[str, Any]:
    out = {label: {"count": 0, "notional": 0.0} for _, _, label in PRICE_BUCKETS}
    out["other"] = {"count": 0, "notional": 0.0}
    for row in rows:
        label = "other"
        for lo, hi, candidate in PRICE_BUCKETS:
            if lo <= row["price"] < hi:
                label = candidate
                break
        out[label]["count"] += 1
        out[label]["notional"] += row["usdc_size"]
    total = sum(v["notional"] for v in out.values())
    for stats in out.values():
        stats["notional"] = round(stats["notional"], 4)
        stats["notional_share"] = round(stats["notional"] / total, 4) if total else 0.0
    return out


def phase_summary(rows: list[dict[str, Any]]) -> dict[str, Any]:
    buckets = {
        "early_0_40": lambda p: p is not None and p < 0.40,
        "mid_40_75": lambda p: p is not None and 0.40 <= p < 0.75,
        "late_75_95": lambda p: p is not None and 0.75 <= p < 0.95,
        "terminal_95_plus": lambda p: p is not None and p >= 0.95,
        "unknown": lambda p: p is None,
    }
    out = {}
    for label, predicate in buckets.items():
        selected = [row for row in rows if predicate(row["phase"])]
        out[label] = {"count": len(selected), "notional": round(sum(row["usdc_size"] for row in selected), 4)}
    return out


def market_shape(by_market: dict[str, list[dict[str, Any]]]) -> dict[str, Any]:
    both = 0
    balanced = 0
    combined_prices = []
    residual_shares = []
    for market_rows in by_market.values():
        by_outcome: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for row in market_rows:
            by_outcome[row["outcome"]].append(row)
        if len(by_outcome) < 2:
            continue
        both += 1
        stats = []
        for outcome_rows in by_outcome.values():
            size = sum(row["size"] for row in outcome_rows)
            avg_px = weighted_avg([(row["price"], row["size"]) for row in outcome_rows])
            stats.append((size, avg_px))
        stats.sort(reverse=True)
        if len(stats) >= 2:
            s0, p0 = stats[0]
            s1, p1 = stats[1]
            combined_prices.append(p0 + p1)
            residual = abs(s0 - s1) / max(s0 + s1, 1e-9)
            residual_shares.append(residual)
            if residual <= 0.40:
                balanced += 1
    return {
        "two_sided_markets": both,
        "two_sided_rate": round(both / len(by_market), 4) if by_market else 0.0,
        "balanced_within_40pct_markets": balanced,
        "combined_avg_pair_cost": numeric_summary(combined_prices),
        "residual_share_ratio": numeric_summary(residual_shares),
    }


def same_second_bursts(rows: list[dict[str, Any]]) -> dict[str, Any]:
    by_second: Counter[tuple[int, str]] = Counter((row["timestamp"], row["market_key"]) for row in rows)
    counts = list(by_second.values())
    return {
        "seconds_with_multiple_fills": sum(1 for count in counts if count >= 2),
        "max_fills_same_market_second": max(counts) if counts else 0,
        "fills_per_market_second": numeric_summary(counts),
    }


def nested_horizon(rows: list[dict[str, Any]]) -> dict[str, Any]:
    groups: dict[tuple[str, int], set[str]] = defaultdict(set)
    notionals: Counter[str] = Counter()
    for row in rows:
        start = row["market_start"]
        if start is None:
            continue
        parent_hour = start - (start % 3600)
        key = (row["asset"], parent_hour)
        groups[key].add(row["horizon"])
        notionals[f"{row['asset']}_{row['horizon']}"] += row["usdc_size"]
    multi = {f"{asset}_{hour}": sorted(horizons) for (asset, hour), horizons in groups.items() if len(horizons) >= 2}
    return {
        "asset_hour_groups": len(groups),
        "multi_horizon_asset_hours": len(multi),
        "multi_horizon_rate": round(len(multi) / len(groups), 4) if groups else 0.0,
        "examples": dict(list(multi.items())[:12]),
        "notional_by_asset_horizon": {k: round(v, 4) for k, v in notionals.most_common()},
    }


def strategy_class_mix(rows: list[dict[str, Any]], by_market: dict[str, list[dict[str, Any]]]) -> dict[str, Any]:
    two_sided_keys = {
        key
        for key, market_rows in by_market.items()
        if len({row["outcome"] for row in market_rows}) >= 2
    }
    classes: Counter[str] = Counter()
    notional: Counter[str] = Counter()
    for row in rows:
        label = "mid_directional"
        if row["market_key"] in two_sided_keys and 0.25 <= row["price"] <= 0.75:
            label = "two_sided_mid_accumulation"
        if row["price"] >= 0.90 and row["phase"] is not None and row["phase"] >= 0.70:
            label = "late_cert_discount_to_par"
        elif row["price"] <= 0.10:
            label = "cheap_tail_convex"
        elif row["phase"] is not None and row["phase"] >= 0.75 and row["price"] >= 0.75:
            label = "late_favourite_ladder"
        classes[label] += 1
        notional[label] += row["usdc_size"]
    total = sum(notional.values())
    return {
        label: {
            "count": classes[label],
            "notional": round(notional[label], 4),
            "notional_share": round(notional[label] / total, 4) if total else 0.0,
        }
        for label, _ in notional.most_common()
    }


def recent_market_examples(by_market: dict[str, list[dict[str, Any]]]) -> list[dict[str, Any]]:
    market_rows = sorted(by_market.values(), key=lambda rows: max(row["timestamp"] for row in rows), reverse=True)
    out = []
    for rows in market_rows[:10]:
        by_outcome: dict[str, dict[str, float]] = defaultdict(lambda: {"shares": 0.0, "notional": 0.0})
        for row in rows:
            by_outcome[row["outcome"]]["shares"] += row["size"]
            by_outcome[row["outcome"]]["notional"] += row["usdc_size"]
        first = rows[0]
        out.append({
            "slug": first["slug"],
            "asset": first["asset"],
            "horizon": first["horizon"],
            "fills": len(rows),
            "last_ts": iso(max(row["timestamp"] for row in rows)),
            "outcomes": {k: {"shares": round(v["shares"], 4), "notional": round(v["notional"], 4)} for k, v in by_outcome.items()},
        })
    return out


def numeric_summary(values: Iterable[float]) -> dict[str, float | int | None]:
    xs = sorted(float(v) for v in values if v is not None and math.isfinite(float(v)))
    if not xs:
        return {"count": 0, "min": None, "p50": None, "mean": None, "p90": None, "p95": None, "max": None}
    return {
        "count": len(xs),
        "min": round(xs[0], 6),
        "p50": round(percentile(xs, 0.50), 6),
        "mean": round(statistics.fmean(xs), 6),
        "p90": round(percentile(xs, 0.90), 6),
        "p95": round(percentile(xs, 0.95), 6),
        "max": round(xs[-1], 6),
    }


def percentile(xs: list[float], q: float) -> float:
    if not xs:
        return float("nan")
    idx = min(len(xs) - 1, max(0, int(round((len(xs) - 1) * q))))
    return xs[idx]


def weighted_avg(items: Iterable[tuple[float, float]]) -> float:
    total_w = 0.0
    total = 0.0
    for value, weight in items:
        w = max(weight, 0.0)
        total += value * w
        total_w += w
    return total / total_w if total_w else 0.0


def pick(row: dict[str, Any], *keys: str, default: Any = None) -> Any:
    lower = {str(k).lower(): v for k, v in row.items()}
    for key in keys:
        if key in row and row[key] not in (None, ""):
            return row[key]
        value = lower.get(key.lower())
        if value not in (None, ""):
            return value
    return default


def to_float(value: Any) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def iso(ts: int) -> str:
    return datetime.fromtimestamp(ts, UTC).isoformat()


if __name__ == "__main__":
    sys.exit(main())
