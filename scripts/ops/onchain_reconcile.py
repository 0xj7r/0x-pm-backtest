#!/usr/bin/env python3
"""On-chain P&L reconciliation from the Polymarket data-api activity feed.

Ground truth for live P&L: per-UTC-day and per-market net USDC cash flow
(BUY=-usdcSize, SELL=+usdcSize, REDEEM=+usdcSize, TAKER_REBATE=+usdcSize).
Optionally compares against ledger-estimated P&L and flags DIVERGENCE when
|ledger - onchain| > max($20, 10% of |onchain|).

Usage:
  python3 scripts/ops/onchain_reconcile.py
  python3 scripts/ops/onchain_reconcile.py --day 2026-06-17
  python3 scripts/ops/onchain_reconcile.py --day 2026-06-17 --ledger-pnl 278.0
  python3 scripts/ops/onchain_reconcile.py --ledger-tsv ledger_days.tsv

Note: the data-api 403s python urllib user agents, so pages are fetched with
curl via subprocess. stdlib only.
"""
from __future__ import annotations

import argparse
import json
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone

API = "https://data-api.polymarket.com/activity"
DEFAULT_USER = "0x00190179D84224687aDfB93e4A499D36AAD0FF59"
PAGE_LIMIT = 500
MAX_OFFSET = 3000

# Signed cash flow per activity record (positive = USDC into the wallet)
FLOW_KINDS = ("BUY", "SELL", "REDEEM", "REBATE")


def fetch_page(user: str, offset: int, timeout: int = 30) -> list[dict]:
    url = f"{API}?user={user}&limit={PAGE_LIMIT}&offset={offset}"
    r = subprocess.run(
        ["curl", "-sf", "--max-time", str(timeout), url],
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        raise RuntimeError(f"curl failed (rc={r.returncode}) for offset={offset}: {r.stderr.strip()}")
    data = json.loads(r.stdout)
    if not isinstance(data, list):
        raise RuntimeError(f"unexpected response at offset={offset}: {r.stdout[:200]}")
    return data


def fetch_activity(user: str) -> list[dict]:
    rows: list[dict] = []
    seen: set[tuple] = set()
    offset = 0
    while offset <= MAX_OFFSET:
        page = fetch_page(user, offset)
        for r in page:
            key = (
                r.get("transactionHash"),
                r.get("type"),
                r.get("asset"),
                r.get("side"),
                r.get("usdcSize"),
                r.get("timestamp"),
            )
            if key in seen:
                continue
            seen.add(key)
            rows.append(r)
        if len(page) < PAGE_LIMIT:
            break
        offset += PAGE_LIMIT
    return rows


def classify(rec: dict) -> tuple[str | None, float]:
    """Returns (flow kind, signed usdc) or (None, 0) for non-cash-flow types."""
    typ = (rec.get("type") or "").upper()
    usdc = float(rec.get("usdcSize") or 0.0)
    if typ == "TRADE":
        side = (rec.get("side") or "").upper()
        if side == "BUY":
            return "BUY", -usdc
        if side == "SELL":
            return "SELL", usdc
        return None, 0.0
    if typ == "REDEEM":
        return "REDEEM", usdc
    if typ == "TAKER_REBATE":
        return "REBATE", usdc
    return None, 0.0


def utc_day(ts: int | float) -> str:
    return datetime.fromtimestamp(int(ts), tz=timezone.utc).strftime("%Y-%m-%d")


def market_key(rec: dict) -> str:
    return rec.get("slug") or rec.get("title") or (rec.get("type") or "?").lower()


def aggregate(rows: list[dict]):
    by_day: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    by_day_market: dict[str, dict[str, float]] = defaultdict(lambda: defaultdict(float))
    ignored: dict[str, int] = defaultdict(int)
    for r in rows:
        ts = r.get("timestamp")
        if ts is None:
            continue
        kind, flow = classify(r)
        if kind is None:
            ignored[(r.get("type") or "?").upper()] += 1
            continue
        day = utc_day(ts)
        d = by_day[day]
        d[kind] += flow
        d["NET"] += flow
        d[f"n_{kind}"] += 1
        by_day_market[day][market_key(r)] += flow
    return by_day, by_day_market, ignored


def load_ledger_tsv(path: str) -> dict[str, float]:
    out: dict[str, float] = {}
    with open(path, encoding="utf-8") as fh:
        for line in fh:
            parts = line.split()
            if len(parts) < 2 or not parts[0][:4].isdigit():
                continue
            try:
                out[parts[0]] = float(parts[1].replace("$", "").replace(",", ""))
            except ValueError:
                continue
    return out


def divergence_flag(ledger: float, onchain: float) -> str:
    threshold = max(20.0, 0.10 * abs(onchain))
    return "DIVERGENCE" if abs(ledger - onchain) > threshold else "ok"


def print_table(
    by_day: dict[str, dict[str, float]],
    ledger_by_day: dict[str, float],
    only_day: str | None,
) -> None:
    days = sorted(by_day)
    if only_day:
        days = [d for d in days if d == only_day]
        if not days:
            print(f"(no on-chain activity for {only_day})")
    header = (
        f"{'day':<12}{'buys':>12}{'sells':>12}{'redeems':>12}{'rebates':>10}"
        f"{'NET':>12}{'trades':>8}"
    )
    if ledger_by_day:
        header += f"{'ledger':>12}{'diff':>10}  flag"
    print("On-chain net USDC cash flow per UTC day (data-api activity, ground truth)")
    print("-" * len(header))
    print(header)
    print("-" * len(header))
    for day in days:
        d = by_day[day]
        n_trades = int(d.get("n_BUY", 0) + d.get("n_SELL", 0))
        line = (
            f"{day:<12}{d.get('BUY', 0):>+12,.2f}{d.get('SELL', 0):>+12,.2f}"
            f"{d.get('REDEEM', 0):>+12,.2f}{d.get('REBATE', 0):>+10,.2f}"
            f"{d.get('NET', 0):>+12,.2f}{n_trades:>8}"
        )
        if ledger_by_day:
            if day in ledger_by_day:
                led = ledger_by_day[day]
                net = d.get("NET", 0.0)
                line += (
                    f"{led:>+12,.2f}{led - net:>+10,.2f}  "
                    f"{divergence_flag(led, net)}"
                )
            else:
                line += f"{'-':>12}{'-':>10}"
        print(line)
    print("-" * len(header))
    total = sum(by_day[d].get("NET", 0.0) for d in days)
    print(f"{'TOTAL':<12}{'':>12}{'':>12}{'':>12}{'':>10}{total:>+12,.2f}")


def print_market_breakdown(day: str, by_day_market: dict[str, dict[str, float]]) -> None:
    markets = by_day_market.get(day)
    if not markets:
        return
    print(f"\nPer-market net cash flow for {day} (UTC):")
    for mkt, net in sorted(markets.items(), key=lambda kv: kv[1]):
        print(f"  {net:>+10,.2f}  {mkt}")


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--user", default=DEFAULT_USER, help="proxy wallet address")
    ap.add_argument("--day", help="restrict output to one UTC day (YYYY-MM-DD)")
    ap.add_argument(
        "--ledger-tsv",
        help="TSV of 'YYYY-MM-DD <pnl>' ledger-estimated days to compare",
    )
    ap.add_argument(
        "--ledger-pnl",
        type=float,
        help="single ledger-estimated P&L to compare (requires --day)",
    )
    ap.add_argument(
        "--per-market",
        action="store_true",
        help="print per-market breakdown (automatic when --day given)",
    )
    args = ap.parse_args()

    if args.ledger_pnl is not None and not args.day:
        ap.error("--ledger-pnl requires --day")

    rows = fetch_activity(args.user)
    if not rows:
        print(f"(no activity returned for {args.user})", file=sys.stderr)
        return 1
    by_day, by_day_market, ignored = aggregate(rows)

    ledger_by_day: dict[str, float] = {}
    if args.ledger_tsv:
        ledger_by_day = load_ledger_tsv(args.ledger_tsv)
    if args.ledger_pnl is not None:
        ledger_by_day[args.day] = args.ledger_pnl

    print_table(by_day, ledger_by_day, args.day)

    if args.day:
        net = by_day.get(args.day, {}).get("NET", 0.0)
        if args.ledger_pnl is not None:
            flag = divergence_flag(args.ledger_pnl, net)
            print(
                f"\n{args.day}: ledger-estimated ${args.ledger_pnl:+,.2f} vs "
                f"on-chain ${net:+,.2f} (diff ${args.ledger_pnl - net:+,.2f}) -> {flag}"
            )
        print_market_breakdown(args.day, by_day_market)
    elif args.per_market:
        for day in sorted(by_day_market):
            print_market_breakdown(day, by_day_market)

    if ignored:
        counts = ", ".join(f"{k}={v}" for k, v in sorted(ignored.items()))
        print(f"\nnote: ignored non-cash-flow activity types: {counts}", file=sys.stderr)

    oldest = min(int(r["timestamp"]) for r in rows if r.get("timestamp") is not None)
    print(
        f"note: activity window starts {utc_day(oldest)} "
        f"(api pagination capped at offset {MAX_OFFSET}); earlier days may be partial",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
