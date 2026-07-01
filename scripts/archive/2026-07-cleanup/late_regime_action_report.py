#!/usr/bin/env python3
"""Focused BTC5m late-regime diagnostics and candidate guardrail report."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any, Iterable, Iterator, TextIO


AWS_PROFILE: str | None = None
LANES = (
    "br2_late_favourite_load",
    "br2_late_confirm",
    "br2_high_skew_load",
    "br2_convex_tail",
)


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


def iter_rows(lines: Iterable[str]) -> Iterator[dict[str, Any]]:
    for line in lines:
        line = line.strip()
        if line:
            yield json.loads(line)


def close_ts(row: dict[str, Any]) -> int:
    if row.get("close_ts") is not None:
        return int(row["close_ts"])
    return int(str(row.get("slug") or "").rsplit("-", 1)[1]) + 300


def strat(row: dict[str, Any], strategy: str) -> dict[str, Any]:
    return ((row.get("per_strategy") or {}).get(strategy)) or {}


def resolved_yes(row: dict[str, Any], result: dict[str, Any]) -> bool:
    if "yes_resolved" in result:
        return bool(result["yes_resolved"])
    return str(row.get("outcome_label") or "").lower() in ("yes", "up")


def fill_won(fill: dict[str, Any], yes: bool) -> bool | None:
    side = str(fill.get("side") or "")
    if side == "BuyYes":
        return yes
    if side == "BuyNo":
        return not yes
    return None


def fill_pnl(fill: dict[str, Any], yes: bool) -> tuple[float, bool | None]:
    won = fill_won(fill, yes)
    shares = float(fill.get("shares") or 0.0)
    notional = float(fill.get("notional") or 0.0)
    rebate = float(fill.get("rebate_usdc") or 0.0)
    return (shares if won else 0.0) - notional + rebate, won


def money(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float) -> str:
    return f"{value:.2%}"


def day_key(ts: int) -> str:
    return dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat()


def week_key(ts: int) -> str:
    day = dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date()
    return (day - dt.timedelta(days=day.weekday())).isoformat()


def final_range_bucket(value: float) -> str:
    if value < 0.50:
        return "range_lt50"
    if value < 0.78:
        return "range_50_78"
    if value < 0.93:
        return "range_78_93_midwide"
    if value < 0.97:
        return "range_93_97"
    return "range_ge97"


def f(fill: dict[str, Any], key: str) -> float:
    return float(fill.get(key) or 0.0)


def market_acc() -> dict[str, Any]:
    return {
        "markets": 0,
        "active": 0,
        "fills": 0,
        "pnl": 0.0,
        "start": None,
        "end": None,
        "midwide_markets": 0,
        "midwide_pnl": 0.0,
        "non_midwide_pnl": 0.0,
    }


def fill_acc() -> dict[str, float]:
    return defaultdict(float)


def add_market(acc: dict[str, Any], market: dict[str, Any]) -> None:
    if acc["start"] is None:
        acc["start"] = market["start"]
    acc["end"] = market["end"]
    acc["markets"] += 1
    acc["active"] += 1 if market["fills"] else 0
    acc["fills"] += len(market["fills"])
    acc["pnl"] += market["pnl"]
    if market["range_bucket"] == "range_78_93_midwide":
        acc["midwide_markets"] += 1
        acc["midwide_pnl"] += market["pnl"]
    else:
        acc["non_midwide_pnl"] += market["pnl"]


def add_fill(acc: dict[str, float], fill: dict[str, Any]) -> None:
    acc["fills"] += 1
    acc["pnl"] += fill["pnl"]
    acc["cost"] += fill["notional"]
    acc["wins"] += 1 if fill["won"] else 0


def summarize_market(label: str, markets: list[dict[str, Any]]) -> str:
    acc = market_acc()
    for market in markets:
        add_market(acc, market)
    start = float(acc["start"] or 0.0)
    end = float(acc["end"] or start)
    return (
        f"| {label} | {acc['markets']} | {acc['active']} | {pct(acc['active'] / acc['markets']) if acc['markets'] else '0.00%'} | "
        f"{acc['fills']} | {money(acc['pnl'])} | {money(start)} | {money(end)} | "
        f"{pct(end / start - 1.0) if start > 0.0 else '0.00%'} | {acc['midwide_markets']} | "
        f"{pct(acc['midwide_markets'] / acc['markets']) if acc['markets'] else '0.00%'} | "
        f"{money(acc['midwide_pnl'])} | {money(acc['non_midwide_pnl'])} |"
    )


def quantile(values: list[float], q: float) -> float:
    if not values:
        return 0.0
    values = sorted(values)
    return values[min(len(values) - 1, max(0, round((len(values) - 1) * q)))]


def guardrails() -> list[tuple[str, str, bool, Any]]:
    return [
        (
            "posthoc:final_midwide",
            "all_loading",
            False,
            lambda fill: fill["tag"] in LANES[:3] and fill["range_bucket"] == "range_78_93_midwide",
        ),
        (
            "late_confirm_sign_flip_040_0457",
            "late_confirm",
            True,
            lambda fill: fill["tag"] == "br2_late_confirm"
            and 0.40 <= f(fill, "regime_sign_flip_rate") < 0.4571,
        ),
        (
            "late_confirm_reversal_024_034",
            "late_confirm",
            True,
            lambda fill: fill["tag"] == "br2_late_confirm"
            and 0.24 <= f(fill, "regime_reversal_pressure") < 0.34,
        ),
        (
            "late_confirm_low_conf_lt_081",
            "late_confirm",
            True,
            lambda fill: fill["tag"] == "br2_late_confirm" and f(fill, "confidence_score") < 0.81,
        ),
        (
            "late_fav_obs_040_050",
            "late_favourite",
            True,
            lambda fill: fill["tag"] == "br2_late_favourite_load"
            and 0.40 <= f(fill, "market_yes_range_so_far") < 0.50,
        ),
        (
            "late_fav_risk_036_039",
            "late_favourite",
            True,
            lambda fill: fill["tag"] == "br2_late_favourite_load" and 0.36 <= f(fill, "risk_score") < 0.3911,
        ),
        (
            "late_fav_model_q4",
            "late_favourite",
            True,
            lambda fill: fill["tag"] == "br2_late_favourite_load" and f(fill, "side_model_p") >= 0.8984,
        ),
    ]


def selected_daily_fields(day: dict[str, Any]) -> str:
    return (
        f"{day['markets']} | {day['active']} | {day['fills']} | {money(day['pnl'])} | "
        f"{money(day['midwide_pnl'])} | {money(day['non_midwide_pnl'])} | "
        f"{money(day['tags'].get('br2_late_favourite_load', 0.0))} | "
        f"{money(day['tags'].get('br2_late_confirm', 0.0))} | "
        f"{money(day['tags'].get('br2_high_skew_load', 0.0))} | "
        f"{money(day['tags'].get('br2_convex_tail', 0.0))}"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile")
    parser.add_argument("--recent-days", type=int, default=30)
    parser.add_argument("--source-label")
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    rows: list[dict[str, Any]] = []
    fills: list[dict[str, Any]] = []
    stream, proc = open_input(args.markets_jsonl)
    try:
        for row in iter_rows(stream):
            result = strat(row, args.strategy)
            ts = close_ts(row)
            yes = resolved_yes(row, result)
            market_fills: list[dict[str, Any]] = []
            bucket = final_range_bucket(float(row.get("volatility_range") or 0.0))
            for fill in result.get("fills_detail") or []:
                tag = str(fill.get("tag") or "unknown")
                if tag not in LANES:
                    continue
                pnl, won = fill_pnl(fill, yes)
                item = dict(fill)
                item.update(
                    {
                        "ts": ts,
                        "date": day_key(ts),
                        "week": week_key(ts),
                        "tag": tag,
                        "pnl": pnl,
                        "won": bool(won),
                        "notional": float(fill.get("notional") or 0.0),
                        "range_bucket": bucket,
                    }
                )
                fills.append(item)
                market_fills.append(item)
            rows.append(
                {
                    "ts": ts,
                    "date": day_key(ts),
                    "week": week_key(ts),
                    "pnl": float(result.get("pnl_usdc") or 0.0),
                    "start": float(result.get("start_equity_usdc") or 0.0),
                    "end": float(result.get("end_equity_usdc") or 0.0),
                    "fills": market_fills,
                    "range_bucket": bucket,
                }
            )
    finally:
        if proc is not None:
            assert proc.stderr is not None
            stderr = proc.stderr.read()
            rc = proc.wait()
            if rc != 0:
                raise RuntimeError(stderr.strip())
        elif stream is not sys.stdin:
            stream.close()

    rows.sort(key=lambda item: item["ts"])
    fills.sort(key=lambda item: item["ts"])
    if not rows:
        raise RuntimeError("no markets loaded")

    source = args.source_label or args.markets_jsonl
    first = dt.datetime.fromtimestamp(rows[0]["ts"], tz=dt.timezone.utc)
    last = dt.datetime.fromtimestamp(rows[-1]["ts"], tz=dt.timezone.utc)
    n = len(rows)
    recent_start = int((last - dt.timedelta(days=args.recent_days) + dt.timedelta(minutes=5)).timestamp())
    early_end = rows[0]["ts"] + args.recent_days * 86400 - 300
    windows = {
        "first_third": rows[: n // 3],
        "middle_third": rows[n // 3 : 2 * n // 3],
        "last_third": rows[2 * n // 3 :],
        f"first_{args.recent_days}d": [row for row in rows if row["ts"] <= early_end],
        f"last_{args.recent_days}d": [row for row in rows if row["ts"] >= recent_start],
    }
    early_fills = [fill for fill in fills if fill["ts"] <= early_end]
    late_fills = [fill for fill in fills if fill["ts"] >= recent_start]

    by_week: dict[str, dict[str, Any]] = defaultdict(market_acc)
    by_day: dict[str, dict[str, Any]] = defaultdict(
        lambda: {
            "markets": 0,
            "active": 0,
            "fills": 0,
            "pnl": 0.0,
            "midwide_pnl": 0.0,
            "non_midwide_pnl": 0.0,
            "tags": defaultdict(float),
        }
    )
    for market in rows:
        add_market(by_week[market["week"]], market)
        if market["ts"] >= recent_start:
            day = by_day[market["date"]]
            day["markets"] += 1
            day["active"] += 1 if market["fills"] else 0
            day["fills"] += len(market["fills"])
            day["pnl"] += market["pnl"]
            if market["range_bucket"] == "range_78_93_midwide":
                day["midwide_pnl"] += market["pnl"]
            else:
                day["non_midwide_pnl"] += market["pnl"]
            for fill in market["fills"]:
                day["tags"][fill["tag"]] += fill["pnl"]

    late_by_lane_range: dict[str, dict[str, float]] = defaultdict(fill_acc)
    for fill in late_fills:
        add_fill(late_by_lane_range[f"{fill['tag']}:{fill['range_bucket']}"], fill)

    lines = [
        "# BTC5m Late-Regime Action Report",
        "",
        f"Source: `{source}`",
        f"Range: `{first.isoformat()}` to `{last.isoformat()}`",
        "",
        "The `range_78_93_midwide` label is post-hoc final market range. It is useful for diagnosis, not directly deployable as a live gate.",
        "",
        "## Executive Read",
        "",
        "- The later regime problem is not that mid-wide markets became more common; their rate fell from the first window, but their expectancy stayed sharply negative while participation collapsed.",
        "- The strategy still offsets some mid-wide damage with non-midwide continuation wins, but the last window has far fewer active markets, so the offset is much weaker.",
        "- Replay-safe slices do identify late-window pain, but several were profitable early. Treat them as inputs to a regime/gated model, not fixed hard-coded throttles.",
        "",
        "## Window Drift",
        "",
        "| Window | Markets | Active | Active Rate | Fills | PnL | Start Eq | End Eq | Return | Mid-Wide Markets | Mid-Wide Rate | Mid-Wide PnL | Non-Mid-Wide PnL |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for name, market_rows in windows.items():
        lines.append(summarize_market(name, market_rows))

    lines.extend(
        [
            "",
            "## Weekly Evolution",
            "",
            "| Week | Markets | Active | Active Rate | Fills | PnL | Mid-Wide Markets | Mid-Wide PnL | Non-Mid-Wide PnL |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for week, acc in sorted(by_week.items()):
        lines.append(
            f"| {week} | {acc['markets']} | {acc['active']} | "
            f"{pct(acc['active'] / acc['markets']) if acc['markets'] else '0.00%'} | "
            f"{acc['fills']} | {money(acc['pnl'])} | {acc['midwide_markets']} | "
            f"{money(acc['midwide_pnl'])} | {money(acc['non_midwide_pnl'])} |"
        )

    lines.extend(
        [
            "",
            f"## Last {args.recent_days}d Daily Attribution",
            "",
            "| Date | Markets | Active | Fills | PnL | Mid-Wide PnL | Non-Mid-Wide PnL | Late Fav | Late Confirm | High Skew | Tail |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for day, row in sorted(by_day.items()):
        lines.append(f"| {day} | {selected_daily_fields(row)} |")

    lines.extend(
        [
            "",
            "## Worst Last-Window Days",
            "",
            "| Rank | Date | Markets | Active | Fills | PnL | Mid-Wide PnL | Non-Mid-Wide PnL | Late Fav | Late Confirm | High Skew | Tail |",
            "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for idx, (day, row) in enumerate(sorted(by_day.items(), key=lambda item: item[1]["pnl"])[:12], 1):
        lines.append(f"| {idx} | {day} | {selected_daily_fields(row)} |")

    lines.extend(
        [
            "",
            "## Last-Window Lane x Final-Range Attribution",
            "",
            "| Lane + Final Range | Fills | PnL | Cost | Win Rate | PnL/Fill |",
            "|---|---:|---:|---:|---:|---:|",
        ]
    )
    for label, acc in sorted(late_by_lane_range.items(), key=lambda item: item[1]["pnl"]):
        fills_n = int(acc["fills"])
        if fills_n == 0:
            continue
        lines.append(
            f"| {label} | {fills_n} | {money(acc['pnl'])} | {money(acc['cost'])} | "
            f"{pct(acc['wins'] / fills_n)} | {money(acc['pnl'] / fills_n)} |"
        )

    lines.extend(
        [
            "",
            "## Candidate Guardrail Stability",
            "",
            "Replay-safe rules are marked `yes`. Negative removed PnL means removing/throttling that slice would have helped that window. Positive early removed PnL means the rule would have damaged the strong early regime.",
            "",
            "| Rule | Lane | Replay Safe | Early Removed Fills | Early Removed PnL | Late Removed Fills | Late Removed PnL | Late Removed Win Rate | Late Removed Cost |",
            "|---|---|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for name, lane, replay_safe, predicate in guardrails():
        early_removed = [fill for fill in early_fills if predicate(fill)]
        late_removed = [fill for fill in late_fills if predicate(fill)]
        late_wins = sum(1 for fill in late_removed if fill["won"])
        lines.append(
            f"| {name} | {lane} | {'yes' if replay_safe else 'no'} | "
            f"{len(early_removed)} | {money(sum(fill['pnl'] for fill in early_removed))} | "
            f"{len(late_removed)} | {money(sum(fill['pnl'] for fill in late_removed))} | "
            f"{pct(late_wins / len(late_removed)) if late_removed else '0.00%'} | "
            f"{money(sum(fill['notional'] for fill in late_removed))} |"
        )

    key_features = [
        "market_yes_range_so_far",
        "side_model_p",
        "side_edge_vs_fill",
        "confidence_score",
        "risk_score",
        "regime_whipsaw_score",
        "regime_path_efficiency",
        "regime_reversal_pressure",
        "regime_sign_flip_rate",
        "regime_realized_vol_180s_bps",
    ]
    lines.extend(
        [
            "",
            "## Feature Drift By Lane",
            "",
            "| Lane | Feature | Early Median | Late Median | Delta | Early P25..P75 | Late P25..P75 |",
            "|---|---|---:|---:|---:|---:|---:|",
        ]
    )
    for lane in LANES[:3]:
        early_lane = [fill for fill in early_fills if fill["tag"] == lane]
        late_lane = [fill for fill in late_fills if fill["tag"] == lane]
        for feature in key_features:
            ev = [f(fill, feature) for fill in early_lane]
            lv = [f(fill, feature) for fill in late_lane]
            if not ev or not lv:
                continue
            em = quantile(ev, 0.50)
            lm = quantile(lv, 0.50)
            lines.append(
                f"| {lane} | {feature} | {em:.4f} | {lm:.4f} | {lm - em:.4f} | "
                f"{quantile(ev, 0.25):.4f}..{quantile(ev, 0.75):.4f} | "
                f"{quantile(lv, 0.25):.4f}..{quantile(lv, 0.75):.4f} |"
            )

    lines.extend(
        [
            "",
            "## Current Actionable Interpretation",
            "",
            "- Do not use final mid-wide range directly; it is a resolved-market diagnostic.",
            "- The next live-safe improvement should be a regime/gated sizing model trained to reduce late-confirm and late-favourite exposure only when replay-time features imply a late break is likely to fail.",
            "- Fixed hard gates are risky because the same slices that lose in the last window often made money in the early window.",
            "- The post-fill rerun should confirm whether the last-window mid-wide losses are the same `crossed_mid_after_fill` path seen in the early checkpoint.",
            "",
        ]
    )

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
