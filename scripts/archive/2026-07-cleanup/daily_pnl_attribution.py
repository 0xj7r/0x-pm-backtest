#!/usr/bin/env python3
"""Daily PnL and fill-tag attribution from walk-forward markets JSONL."""

import argparse
import datetime as dt
import json
import os
import subprocess
import sys
from collections import OrderedDict, defaultdict
from pathlib import Path
from typing import Any, Iterable, Iterator, TextIO


AWS_PROFILE: str | None = None
TAGS = [
    "br2_late_favourite_load",
    "br2_late_confirm",
    "br2_high_skew_load",
    "br2_convex_tail",
]


def open_input(path: str) -> tuple[TextIO, subprocess.Popen[str] | None]:
    if path == "-":
        return sys.stdin, None
    if path.startswith("s3://"):
        env = os.environ.copy()
        if AWS_PROFILE:
            env["AWS_PROFILE"] = AWS_PROFILE
        proc = subprocess.Popen(
            ["aws", "s3", "cp", path, "-"],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
        assert proc.stdout is not None
        return proc.stdout, proc
    return Path(path).open(), None


def iter_rows(f: Iterable[str]) -> Iterator[dict[str, Any]]:
    for line in f:
        line = line.strip()
        if line:
            yield json.loads(line)


def close_ts(row: dict[str, Any]) -> int:
    value = row.get("close_ts")
    if value is not None:
        return int(value)
    return int(str(row.get("slug") or "").rsplit("-", 1)[1]) + 300


def strategy_result(row: dict[str, Any], strategy: str) -> dict[str, Any]:
    return ((row.get("per_strategy") or {}).get(strategy)) or {}


def fill_won(row: dict[str, Any], side: str) -> bool | None:
    outcome = str(row.get("outcome_label") or "").lower()
    if side == "BuyYes":
        return outcome in ("yes", "up")
    if side == "BuyNo":
        return outcome in ("no", "down")
    return None


def settled_fill_pnl(row: dict[str, Any], fill: dict[str, Any]) -> float:
    won = fill_won(row, str(fill.get("side") or ""))
    shares = float(fill.get("shares") or 0.0)
    notional = float(fill.get("notional") or 0.0)
    rebate = float(fill.get("rebate_usdc") or 0.0)
    return (shares if won else 0.0) - notional + rebate


def empty_day() -> dict[str, Any]:
    return {
        "markets": 0,
        "active": 0,
        "fills": 0,
        "pnl": 0.0,
        "start": None,
        "end": None,
        "peak": None,
        "maxdd": 0.0,
        "tags": defaultdict(float),
        "tag_fills": defaultdict(int),
    }


def fmt(value: float, decimals: int = 2) -> str:
    return f"{value:.{decimals}f}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl", help="local path, s3:// path, or '-'")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile")
    parser.add_argument("--csv-out")
    parser.add_argument("--md-out")
    parser.add_argument("--top", type=int, default=12)
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    days: "OrderedDict[str, dict[str, Any]]" = OrderedDict()
    f, proc = open_input(args.markets_jsonl)
    try:
        for row in iter_rows(f):
            strat = strategy_result(row, args.strategy)
            ts = close_ts(row)
            day_key = dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat()
            day = days.setdefault(day_key, empty_day())
            if day["start"] is None:
                day["start"] = float(strat.get("start_equity_usdc") or 0.0)
                day["peak"] = day["start"]
            fills = strat.get("fills_detail") or []
            day["markets"] += 1
            day["fills"] += len(fills)
            if fills:
                day["active"] += 1
            pnl = float(strat.get("pnl_usdc") or 0.0)
            day["pnl"] += pnl
            day["end"] = float(strat.get("end_equity_usdc") or day["start"] or 0.0)
            day["peak"] = max(float(day["peak"] or 0.0), float(day["end"] or 0.0))
            if day["peak"] and day["end"]:
                day["maxdd"] = max(float(day["maxdd"]), (float(day["peak"]) - float(day["end"])) / float(day["peak"]))
            for fill in fills:
                tag = str(fill.get("tag") or "unknown")
                day["tags"][tag] += settled_fill_pnl(row, fill)
                day["tag_fills"][tag] += 1
    finally:
        if proc is not None:
            assert proc.stderr is not None
            stderr = proc.stderr.read()
            rc = proc.wait()
            if rc != 0:
                raise RuntimeError(stderr.strip())
        elif f is not sys.stdin:
            f.close()

    rows = []
    for key, day in days.items():
        start = float(day["start"] or 0.0)
        end = float(day["end"] or start)
        ret = ((end / start) - 1.0) if start > 0.0 else 0.0
        rows.append((key, day, ret))

    if args.csv_out:
        header = [
            "date",
            "markets",
            "active_markets",
            "fills",
            "pnl_usdc",
            "start_equity",
            "end_equity",
            "return_pct",
            "max_intraday_dd_pct",
        ]
        header.extend(f"{tag}_pnl" for tag in TAGS)
        header.extend(f"{tag}_fills" for tag in TAGS)
        lines = [",".join(header)]
        for key, day, ret in rows:
            values = [
                key,
                str(day["markets"]),
                str(day["active"]),
                str(day["fills"]),
                fmt(float(day["pnl"])),
                fmt(float(day["start"] or 0.0)),
                fmt(float(day["end"] or 0.0)),
                fmt(ret * 100.0, 4),
                fmt(float(day["maxdd"]) * 100.0, 4),
            ]
            values.extend(fmt(float(day["tags"].get(tag, 0.0))) for tag in TAGS)
            values.extend(str(int(day["tag_fills"].get(tag, 0))) for tag in TAGS)
            lines.append(",".join(values))
        Path(args.csv_out).write_text("\n".join(lines) + "\n")

    md = []
    md.append("# BTC5m Daily PnL Attribution")
    md.append("")
    md.append(f"Source: `{args.markets_jsonl}`")
    md.append("")
    md.append("## Daily Table")
    md.append("")
    md.append("| Date | Markets | Active | Fills | PnL | Return | End Equity | Late Fav | Late Confirm | High Skew | Tail | Max DD |")
    md.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for key, day, ret in rows:
        md.append(
            f"| {key} | {day['markets']} | {day['active']} | {day['fills']} | "
            f"${fmt(float(day['pnl']))} | {fmt(ret * 100.0, 2)}% | ${fmt(float(day['end'] or 0.0))} | "
            f"${fmt(float(day['tags'].get('br2_late_favourite_load', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_late_confirm', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_high_skew_load', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_convex_tail', 0.0)))} | "
            f"{fmt(float(day['maxdd']) * 100.0, 2)}% |"
        )

    md.append("")
    md.append(f"## Worst {args.top} Days")
    md.append("")
    md.append("| Rank | Date | PnL | Late Fav | Late Confirm | High Skew | Tail | Active | Fills |")
    md.append("|---:|---|---:|---:|---:|---:|---:|---:|---:|")
    for idx, (key, day, _ret) in enumerate(sorted(rows, key=lambda item: item[1]["pnl"])[: args.top], 1):
        md.append(
            f"| {idx} | {key} | ${fmt(float(day['pnl']))} | "
            f"${fmt(float(day['tags'].get('br2_late_favourite_load', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_late_confirm', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_high_skew_load', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_convex_tail', 0.0)))} | "
            f"{day['active']} | {day['fills']} |"
        )

    md.append("")
    md.append(f"## Best {args.top} Days")
    md.append("")
    md.append("| Rank | Date | PnL | Late Fav | Late Confirm | High Skew | Tail | Active | Fills |")
    md.append("|---:|---|---:|---:|---:|---:|---:|---:|---:|")
    for idx, (key, day, _ret) in enumerate(sorted(rows, key=lambda item: item[1]["pnl"], reverse=True)[: args.top], 1):
        md.append(
            f"| {idx} | {key} | ${fmt(float(day['pnl']))} | "
            f"${fmt(float(day['tags'].get('br2_late_favourite_load', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_late_confirm', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_high_skew_load', 0.0)))} | "
            f"${fmt(float(day['tags'].get('br2_convex_tail', 0.0)))} | "
            f"{day['active']} | {day['fills']} |"
        )

    output = "\n".join(md) + "\n"
    if args.md_out:
        Path(args.md_out).write_text(output)
    else:
        print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
