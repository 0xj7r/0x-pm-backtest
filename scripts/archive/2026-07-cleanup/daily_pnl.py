#!/usr/bin/env python3
"""Aggregate walk-forward per-market JSONL into daily PnL rows."""

import argparse
import datetime as dt
import json
import sys
from collections import OrderedDict
from pathlib import Path
from typing import Any, Dict, Iterable, TextIO


def open_input(path: str) -> TextIO:
    if path == "-":
        return sys.stdin
    return Path(path).open()


def iter_rows(f: Iterable[str]) -> Iterable[Dict[str, Any]]:
    for line in f:
        line = line.strip()
        if line:
            yield json.loads(line)


def day_key(row: Dict[str, Any]) -> str:
    close_ts = row.get("close_ts")
    if close_ts is None:
        slug = row.get("slug", "")
        try:
            close_ts = int(slug.rsplit("-", 1)[1]) + 300
        except (IndexError, TypeError, ValueError):
            raise ValueError(f"cannot infer timestamp for row slug={slug!r}") from None
    return dt.datetime.fromtimestamp(int(close_ts), tz=dt.timezone.utc).date().isoformat()


def strategy_result(row: Dict[str, Any], strategy: str) -> Dict[str, Any]:
    per_strategy = row.get("per_strategy") or {}
    return per_strategy.get(strategy) or {}


def fmt(value: float, decimals: int = 2) -> str:
    return f"{value:.{decimals}f}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl", help="per-market walk-forward JSONL, or '-' for stdin")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--csv", action="store_true", help="emit CSV instead of a markdown table")
    args = parser.parse_args()

    days: "OrderedDict[str, Dict[str, Any]]" = OrderedDict()
    with open_input(args.markets_jsonl) as f:
        for row in iter_rows(f):
            strat = strategy_result(row, args.strategy)
            if not strat:
                continue
            key = day_key(row)
            day = days.setdefault(
                key,
                {
                    "markets": 0,
                    "active": 0,
                    "fills": 0,
                    "pnl": 0.0,
                    "start": strat.get("start_equity_usdc"),
                    "end": strat.get("start_equity_usdc"),
                    "peak": strat.get("start_equity_usdc"),
                    "max_dd": 0.0,
                },
            )
            day["markets"] += 1
            fills = int(strat.get("fills") or strat.get("orders_filled") or 0)
            day["fills"] += fills
            if fills > 0:
                day["active"] += 1
            day["pnl"] += float(strat.get("pnl_usdc") or 0.0)
            end = float(strat.get("end_equity_usdc") or day["end"] or 0.0)
            day["end"] = end
            peak = max(float(day["peak"] or end), end)
            day["peak"] = peak
            if peak > 0.0:
                day["max_dd"] = max(float(day["max_dd"]), (peak - end) / peak * 100.0)

    rows = []
    for key, day in days.items():
        start = float(day["start"] or 0.0)
        end = float(day["end"] or start)
        ret = ((end / start) - 1.0) * 100.0 if start > 0.0 else 0.0
        rows.append((key, day, ret))

    if args.csv:
        print("date,markets,active_markets,fills,pnl_usdc,start_equity,end_equity,return_pct,max_intraday_dd_pct")
        for key, day, ret in rows:
            print(
                ",".join(
                    [
                        key,
                        str(day["markets"]),
                        str(day["active"]),
                        str(day["fills"]),
                        fmt(day["pnl"]),
                        fmt(float(day["start"] or 0.0)),
                        fmt(float(day["end"] or 0.0)),
                        fmt(ret, 4),
                        fmt(float(day["max_dd"]), 4),
                    ]
                )
            )
        return 0

    print("| Date | Markets | Active | Fills | PnL | Start | End | Return % | Max DD % |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for key, day, ret in rows:
        print(
            f"| {key} | {day['markets']} | {day['active']} | {day['fills']} | "
            f"{fmt(day['pnl'])} | {fmt(float(day['start'] or 0.0))} | "
            f"{fmt(float(day['end'] or 0.0))} | {fmt(ret, 4)} | {fmt(float(day['max_dd']), 4)} |"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
