#!/usr/bin/env python3
"""Audit timestamp coverage for Polymarket market/result JSONL files."""

import argparse
import datetime as dt
import json
import sys
from collections import Counter
from pathlib import Path
from typing import Any, Dict, Iterable, List, TextIO, Tuple


def open_input(path: str) -> TextIO:
    if path == "-":
        return sys.stdin
    return Path(path).open()


def iter_jsonl(f: Iterable[str]) -> Iterable[Dict[str, Any]]:
    for line in f:
        line = line.strip()
        if line:
            yield json.loads(line)


def row_ts(row: Dict[str, Any]) -> int:
    close_ts = row.get("close_ts")
    if close_ts is not None:
        return int(close_ts)
    slug = str(row.get("slug") or "")
    try:
        return int(slug.rsplit("-", 1)[1]) + 300
    except (IndexError, ValueError):
        raise ValueError(f"cannot infer close_ts for slug={slug!r}") from None


def iso(ts: int) -> str:
    return dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).isoformat()


def day(ts: int) -> str:
    return dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat()


def summarize_gaps(times: List[int], expected_step: int) -> Tuple[int, List[Tuple[int, int, int]]]:
    gaps: List[Tuple[int, int, int]] = []
    missing_steps = 0
    for prev, cur in zip(times, times[1:]):
        delta = cur - prev
        if delta > expected_step:
            missing = max(0, delta // expected_step - 1)
            missing_steps += missing
            gaps.append((prev, cur, missing))
    return missing_steps, gaps


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("jsonl", help="market/result JSONL, or '-' for stdin")
    parser.add_argument("--expected-step-seconds", type=int, default=300)
    parser.add_argument("--expected-full-day-markets", type=int, default=288)
    parser.add_argument("--show-days", action="store_true")
    parser.add_argument("--show-gaps", type=int, default=10)
    args = parser.parse_args()

    times: List[int] = []
    slugs: List[str] = []
    with open_input(args.jsonl) as f:
        for row in iter_jsonl(f):
            times.append(row_ts(row))
            slugs.append(str(row.get("slug") or ""))

    if not times:
        print("rows=0")
        return 1

    sorted_times = sorted(times)
    duplicates = sum(count - 1 for count in Counter(times).values() if count > 1)
    non_monotonic = sum(1 for prev, cur in zip(times, times[1:]) if cur < prev)
    missing_steps, gaps = summarize_gaps(sorted_times, args.expected_step_seconds)
    day_counts = Counter(day(ts) for ts in sorted_times)
    incomplete_days = {
        key: count
        for key, count in sorted(day_counts.items())
        if count != args.expected_full_day_markets
    }

    print(f"rows={len(times)}")
    print(f"first={iso(sorted_times[0])}")
    print(f"last={iso(sorted_times[-1])}")
    print(f"unique_timestamps={len(set(times))}")
    print(f"duplicates={duplicates}")
    print(f"non_monotonic_rows={non_monotonic}")
    print(f"gap_count={len(gaps)}")
    print(f"missing_expected_steps={missing_steps}")
    print(f"days={len(day_counts)}")
    print(f"incomplete_days={len(incomplete_days)}")

    if slugs and any(slugs):
        print(f"first_slug={slugs[0]}")
        print(f"last_slug={slugs[-1]}")

    for prev, cur, missing in gaps[: args.show_gaps]:
        print(f"gap from={iso(prev)} to={iso(cur)} missing_steps={missing}")

    if args.show_days:
        for key, count in sorted(day_counts.items()):
            marker = "" if count == args.expected_full_day_markets else " incomplete"
            print(f"day {key} markets={count}{marker}")
    elif incomplete_days:
        preview = list(incomplete_days.items())[:10]
        for key, count in preview:
            print(f"incomplete_day {key} markets={count}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
