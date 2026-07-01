#!/usr/bin/env python3
"""Explain BTC 5m regime drift and late-window PnL decay from market JSONL."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
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

FEATURES = (
    "price",
    "side_model_p",
    "side_edge_vs_fill",
    "confidence_score",
    "risk_score",
    "market_yes_range_so_far",
    "seconds_to_close",
    "regime_whipsaw_score",
    "regime_path_efficiency",
    "regime_reversal_pressure",
    "regime_sign_flip_rate",
    "regime_realized_vol_180s_bps",
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
    slug = str(row.get("slug") or "")
    return int(slug.rsplit("-", 1)[1]) + 300


def strategy_result(row: dict[str, Any], strategy: str) -> dict[str, Any]:
    return ((row.get("per_strategy") or {}).get(strategy)) or {}


def yes_resolved(row: dict[str, Any], strat: dict[str, Any]) -> bool:
    if "yes_resolved" in strat:
        return bool(strat["yes_resolved"])
    outcome = str(row.get("outcome_label") or "").lower()
    return outcome in ("yes", "up")


def fill_won(fill: dict[str, Any], resolved_yes: bool) -> bool | None:
    side = str(fill.get("side") or "")
    if side == "BuyYes":
        return resolved_yes
    if side == "BuyNo":
        return not resolved_yes
    return None


def fill_pnl(fill: dict[str, Any], resolved_yes: bool) -> tuple[float, bool | None]:
    won = fill_won(fill, resolved_yes)
    shares = float(fill.get("shares") or 0.0)
    notional = float(fill.get("notional") or 0.0)
    rebate = float(fill.get("rebate_usdc") or 0.0)
    return (shares if won else 0.0) - notional + rebate, won


def day(ts: int) -> dt.date:
    return dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date()


def week_key(ts: int) -> str:
    d = day(ts)
    start = d - dt.timedelta(days=d.weekday())
    return start.isoformat()


def final_range_bucket(value: float) -> str:
    if value < 0.50:
        return "range_lt_050"
    if value < 0.78:
        return "range_050_078"
    if value < 0.93:
        return "range_078_093_midwide"
    if value < 0.97:
        return "range_093_097"
    return "range_ge_097"


def observed_range_bucket(value: float) -> str:
    if value < 0.40:
        return "obs_lt_040"
    if value < 0.50:
        return "obs_040_050"
    if value < 0.65:
        return "obs_050_065"
    return "obs_ge_065"


def live_regime_label(fill: dict[str, Any]) -> str:
    observed_range = float(fill.get("market_yes_range_so_far") or 0.0)
    whipsaw = float(fill.get("regime_whipsaw_score") or 0.0)
    path_eff = float(fill.get("regime_path_efficiency") or 0.0)
    reversal = float(fill.get("regime_reversal_pressure") or 0.0)
    sign_flip = float(fill.get("regime_sign_flip_rate") or 0.0)

    if 0.40 <= observed_range < 0.65 and (sign_flip >= 0.40 or path_eff <= 0.15):
        return "expanded_not_decisive"
    if observed_range >= 0.65 and whipsaw >= 0.30:
        return "expanded_chop"
    if reversal >= 0.34 or sign_flip >= 0.45:
        return "reversal_pressure"
    if observed_range >= 0.65:
        return "expanded_continuation"
    return "neutral"


def post_fill_bucket(fill: dict[str, Any]) -> str | None:
    path = fill.get("post_fill_path")
    if not isinstance(path, dict):
        return None
    adverse = float(path.get("adverse_excursion") or 0.0)
    final_side_mid = float(path.get("final_side_mid") or 0.0)
    crossed = bool(path.get("crossed_mid_after_fill"))
    if crossed:
        return "crossed_mid_after_fill"
    if adverse >= 0.20 and final_side_mid < 0.60:
        return "large_adverse_soft_finish"
    if adverse >= 0.10:
        return "moderate_adverse"
    return "held_side"


def empty_acc() -> dict[str, Any]:
    return {
        "markets": 0,
        "active": 0,
        "fills": 0,
        "pnl": 0.0,
        "cost": 0.0,
        "wins": 0,
        "start_eq": None,
        "end_eq": None,
        "midwide_markets": 0,
        "midwide_active": 0,
        "midwide_pnl": 0.0,
    }


def add_market(acc: dict[str, Any], market: dict[str, Any]) -> None:
    if acc["start_eq"] is None:
        acc["start_eq"] = market["start_eq"]
    acc["end_eq"] = market["end_eq"]
    acc["markets"] += 1
    acc["active"] += 1 if market["fills"] else 0
    acc["fills"] += len(market["fills"])
    acc["pnl"] += market["pnl"]
    if market["final_range_bucket"] == "range_078_093_midwide":
        acc["midwide_markets"] += 1
        acc["midwide_active"] += 1 if market["fills"] else 0
        acc["midwide_pnl"] += market["pnl"]


def add_fill(acc: dict[str, Any], fill: dict[str, Any]) -> None:
    acc["fills"] += 1
    acc["pnl"] += fill["pnl"]
    acc["cost"] += fill["notional"]
    acc["wins"] += 1 if fill["won"] else 0


def fmt(value: float, decimals: int = 2) -> str:
    return f"{value:.{decimals}f}"


def money(value: float) -> str:
    return f"${value:,.2f}"


def pct(value: float, decimals: int = 2) -> str:
    return f"{value:.{decimals}f}%"


def q(values: list[float], quantile: float) -> float:
    if not values:
        return math.nan
    values = sorted(values)
    idx = min(len(values) - 1, max(0, round((len(values) - 1) * quantile)))
    return values[idx]


def summarize_acc(acc: dict[str, Any]) -> dict[str, float]:
    markets = int(acc["markets"])
    fills = int(acc["fills"])
    start = float(acc["start_eq"] or 0.0)
    end = float(acc["end_eq"] or start)
    return {
        "active_rate": acc["active"] / markets if markets else 0.0,
        "midwide_rate": acc["midwide_markets"] / markets if markets else 0.0,
        "midwide_active_rate": acc["midwide_active"] / acc["midwide_markets"]
        if acc["midwide_markets"]
        else 0.0,
        "return": end / start - 1.0 if start > 0.0 else 0.0,
        "win_rate": acc["wins"] / fills if fills else 0.0,
    }


def render_market_windows(lines: list[str], windows: dict[str, list[dict[str, Any]]]) -> None:
    lines.extend(
        [
            "## Window Regime Evolution",
            "",
            "| Window | Markets | Active | Active Rate | Fills | PnL | Return | Mid-Wide Markets | Mid-Wide Rate | Mid-Wide Active | Mid-Wide PnL |",
            "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for name, rows in windows.items():
        acc = empty_acc()
        for market in rows:
            add_market(acc, market)
        s = summarize_acc(acc)
        lines.append(
            f"| {name} | {acc['markets']} | {acc['active']} | {pct(s['active_rate'] * 100.0)} | "
            f"{acc['fills']} | {money(acc['pnl'])} | {pct(s['return'] * 100.0)} | "
            f"{acc['midwide_markets']} | {pct(s['midwide_rate'] * 100.0)} | "
            f"{acc['midwide_active']} | {money(acc['midwide_pnl'])} |"
        )
    lines.append("")


def render_weekly(lines: list[str], markets: list[dict[str, Any]]) -> None:
    weekly: dict[str, dict[str, Any]] = defaultdict(empty_acc)
    by_week_midwide_lane: dict[tuple[str, str], dict[str, Any]] = defaultdict(empty_acc)
    for market in markets:
        wk = week_key(market["ts"])
        add_market(weekly[wk], market)
        if market["final_range_bucket"] == "range_078_093_midwide":
            for fill in market["fills"]:
                add_fill(by_week_midwide_lane[(wk, fill["tag"])], fill)

    lines.extend(
        [
            "## Weekly Drift",
            "",
            "| Week | Markets | Active | Active Rate | PnL | Mid-Wide Markets | Mid-Wide Rate | Mid-Wide PnL |",
            "|---|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for wk, acc in sorted(weekly.items()):
        s = summarize_acc(acc)
        lines.append(
            f"| {wk} | {acc['markets']} | {acc['active']} | {pct(s['active_rate'] * 100.0)} | "
            f"{money(acc['pnl'])} | {acc['midwide_markets']} | "
            f"{pct(s['midwide_rate'] * 100.0)} | {money(acc['midwide_pnl'])} |"
        )
    lines.append("")

    lines.extend(
        [
            "### Weekly Mid-Wide Lane PnL",
            "",
            "| Week | Lane | Fills | PnL | Cost | Win Rate |",
            "|---|---|---:|---:|---:|---:|",
        ]
    )
    for (wk, lane), acc in sorted(by_week_midwide_lane.items()):
        if acc["fills"] == 0:
            continue
        s = summarize_acc(acc)
        lines.append(
            f"| {wk} | {lane} | {acc['fills']} | {money(acc['pnl'])} | "
            f"{money(acc['cost'])} | {pct(s['win_rate'] * 100.0)} |"
        )
    lines.append("")


def render_grouped_fills(
    lines: list[str],
    title: str,
    fills: list[dict[str, Any]],
    group_key: str,
    min_fills: int,
) -> None:
    grouped: dict[tuple[str, str], dict[str, Any]] = defaultdict(empty_acc)
    for fill in fills:
        add_fill(grouped[(fill["tag"], str(fill.get(group_key) or "unknown"))], fill)
    lines.extend(
        [
            f"## {title}",
            "",
            "| Lane | Bucket | Fills | PnL | Cost | Win Rate | PnL/Fill |",
            "|---|---|---:|---:|---:|---:|---:|",
        ]
    )
    rows = sorted(grouped.items(), key=lambda kv: (kv[0][0], kv[1]["pnl"]))
    for (lane, bucket), acc in rows:
        if acc["fills"] < min_fills:
            continue
        s = summarize_acc(acc)
        lines.append(
            f"| {lane} | {bucket} | {acc['fills']} | {money(acc['pnl'])} | "
            f"{money(acc['cost'])} | {pct(s['win_rate'] * 100.0)} | "
            f"{money(acc['pnl'] / acc['fills'])} |"
        )
    lines.append("")


def render_feature_drift(lines: list[str], early_fills: list[dict[str, Any]], late_fills: list[dict[str, Any]]) -> None:
    lines.extend(
        [
            "## Feature Drift",
            "",
            "| Lane | Feature | Early Median | Late Median | Delta | Early P25..P75 | Late P25..P75 |",
            "|---|---|---:|---:|---:|---:|---:|",
        ]
    )
    for lane in LANES:
        early_lane = [f for f in early_fills if f["tag"] == lane]
        late_lane = [f for f in late_fills if f["tag"] == lane]
        if len(early_lane) < 10 or len(late_lane) < 10:
            continue
        for feature in (
            "market_yes_range_so_far",
            "regime_whipsaw_score",
            "regime_path_efficiency",
            "regime_reversal_pressure",
            "regime_sign_flip_rate",
            "regime_realized_vol_180s_bps",
            "side_edge_vs_fill",
            "confidence_score",
        ):
            e = [float(f[feature]) for f in early_lane if f.get(feature) is not None]
            l = [float(f[feature]) for f in late_lane if f.get(feature) is not None]
            if len(e) < 10 or len(l) < 10:
                continue
            em = q(e, 0.5)
            lm = q(l, 0.5)
            lines.append(
                f"| {lane} | {feature} | {fmt(em, 4)} | {fmt(lm, 4)} | "
                f"{fmt(lm - em, 4)} | {fmt(q(e, 0.25), 4)}..{fmt(q(e, 0.75), 4)} | "
                f"{fmt(q(l, 0.25), 4)}..{fmt(q(l, 0.75), 4)} |"
            )
    lines.append("")


def rule_candidates() -> list[tuple[str, str, Any]]:
    return [
        ("late_confirm_range_ge_050", "br2_late_confirm", lambda f: (f.get("market_yes_range_so_far") or 0.0) >= 0.50),
        ("late_confirm_sign_flip_040_0457", "br2_late_confirm", lambda f: 0.40 <= (f.get("regime_sign_flip_rate") or 0.0) < 0.4572),
        ("late_confirm_reversal_024_034", "br2_late_confirm", lambda f: 0.24 <= (f.get("regime_reversal_pressure") or 0.0) < 0.34),
        ("late_confirm_low_conf_lt_081", "br2_late_confirm", lambda f: (f.get("confidence_score") or 0.0) < 0.81),
        ("late_fav_obs_040_050", "br2_late_favourite_load", lambda f: 0.40 <= (f.get("market_yes_range_so_far") or 0.0) < 0.50),
        ("late_fav_low_price_lt_076", "br2_late_favourite_load", lambda f: (f.get("price") or 0.0) < 0.76),
        ("late_fav_high_price_ge_079", "br2_late_favourite_load", lambda f: (f.get("price") or 0.0) >= 0.79),
        ("high_skew_final_midwide_posthoc", "br2_high_skew_load", lambda f: f.get("final_range_bucket") == "range_078_093_midwide"),
        ("late_confirm_final_midwide_posthoc", "br2_late_confirm", lambda f: f.get("final_range_bucket") == "range_078_093_midwide"),
        ("late_fav_final_midwide_posthoc", "br2_late_favourite_load", lambda f: f.get("final_range_bucket") == "range_078_093_midwide"),
    ]


def render_rule_diagnostics(lines: list[str], early_fills: list[dict[str, Any]], late_fills: list[dict[str, Any]]) -> None:
    lines.extend(
        [
            "## Candidate Guardrail Diagnostics",
            "",
            "Rows show what would happen if a rule removed those fills. Post-hoc rules are explicitly marked and are not deployable as-is.",
            "",
            "| Rule | Lane | Early Removed Fills | Early Removed PnL | Late Removed Fills | Late Removed PnL | Late Kept PnL | Late PnL If Removed |",
            "|---|---|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for name, lane, predicate in rule_candidates():
        early_lane = [f for f in early_fills if f["tag"] == lane]
        late_lane = [f for f in late_fills if f["tag"] == lane]
        early_removed = [f for f in early_lane if predicate(f)]
        late_removed = [f for f in late_lane if predicate(f)]
        late_kept = [f for f in late_lane if not predicate(f)]
        early_removed_pnl = sum(f["pnl"] for f in early_removed)
        late_removed_pnl = sum(f["pnl"] for f in late_removed)
        late_kept_pnl = sum(f["pnl"] for f in late_kept)
        lines.append(
            f"| `{name}` | {lane} | {len(early_removed)} | {money(early_removed_pnl)} | "
            f"{len(late_removed)} | {money(late_removed_pnl)} | {money(late_kept_pnl)} | "
            f"{money(late_kept_pnl)} |"
        )
    lines.append("")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl", help="local path, s3:// path, or '-'")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile")
    parser.add_argument("--recent-days", type=int, default=30)
    parser.add_argument("--min-fills", type=int, default=8)
    parser.add_argument("--source-label")
    parser.add_argument("--out-md", required=True)
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    markets: list[dict[str, Any]] = []
    f, proc = open_input(args.markets_jsonl)
    try:
        for row in iter_rows(f):
            strat = strategy_result(row, args.strategy)
            ts = close_ts(row)
            resolved_yes = yes_resolved(row, strat)
            fills = []
            final_range = float(row.get("volatility_range") or 0.0)
            for fill in strat.get("fills_detail") or []:
                pnl, won = fill_pnl(fill, resolved_yes)
                enriched = {
                    "ts": ts,
                    "date": day(ts),
                    "slug": row.get("slug"),
                    "tag": str(fill.get("tag") or "unknown"),
                    "side": str(fill.get("side") or "unknown"),
                    "price": float(fill.get("price") or 0.0),
                    "notional": float(fill.get("notional") or 0.0),
                    "shares": float(fill.get("shares") or 0.0),
                    "pnl": pnl,
                    "won": bool(won),
                    "final_range": final_range,
                    "final_range_bucket": final_range_bucket(final_range),
                }
                for feature in FEATURES:
                    value = fill.get(feature)
                    enriched[feature] = float(value) if value is not None else None
                enriched["observed_range_bucket"] = observed_range_bucket(
                    float(enriched.get("market_yes_range_so_far") or 0.0)
                )
                enriched["live_regime_label"] = live_regime_label(enriched)
                enriched["post_fill_bucket"] = post_fill_bucket(fill)
                fills.append(enriched)
            markets.append(
                {
                    "ts": ts,
                    "date": day(ts),
                    "slug": row.get("slug"),
                    "pnl": float(strat.get("pnl_usdc") or 0.0),
                    "start_eq": float(strat.get("start_equity_usdc") or 0.0),
                    "end_eq": float(strat.get("end_equity_usdc") or 0.0),
                    "final_range": final_range,
                    "final_range_bucket": final_range_bucket(final_range),
                    "fills": fills,
                }
            )
    finally:
        if proc is not None:
            assert proc.stderr is not None
            stderr = proc.stderr.read()
            rc = proc.wait()
            if rc != 0:
                raise RuntimeError(stderr.strip())
        elif f is not sys.stdin:
            f.close()

    markets.sort(key=lambda m: m["ts"])
    if not markets:
        raise RuntimeError("no markets found")

    min_dt = dt.datetime.fromtimestamp(markets[0]["ts"], tz=dt.timezone.utc)
    max_dt = dt.datetime.fromtimestamp(markets[-1]["ts"], tz=dt.timezone.utc)
    recent_start_ts = int((max_dt - dt.timedelta(days=args.recent_days) + dt.timedelta(minutes=5)).timestamp())
    early_end_ts = markets[0]["ts"] + args.recent_days * 86400 - 300
    n = len(markets)

    windows = {
        "first_third": markets[: n // 3],
        "middle_third": markets[n // 3 : 2 * n // 3],
        "last_third": markets[2 * n // 3 :],
        f"first_{args.recent_days}d": [m for m in markets if m["ts"] <= early_end_ts],
        f"last_{args.recent_days}d": [m for m in markets if m["ts"] >= recent_start_ts],
        "last_14d": [m for m in markets if m["ts"] >= int((max_dt - dt.timedelta(days=14) + dt.timedelta(minutes=5)).timestamp())],
        "last_7d": [m for m in markets if m["ts"] >= int((max_dt - dt.timedelta(days=7) + dt.timedelta(minutes=5)).timestamp())],
    }
    early_fills = [f for m in windows[f"first_{args.recent_days}d"] for f in m["fills"]]
    late_fills = [f for m in windows[f"last_{args.recent_days}d"] for f in m["fills"]]

    lines = [
        "# BTC5m Regime Evolution Diagnostics",
        "",
        f"Source: `{args.source_label or args.markets_jsonl}`",
        f"Range: `{min_dt.isoformat()}` to `{max_dt.isoformat()}`",
        "",
        "Core post-hoc regime: `range_078_093_midwide` means final resolved-market YES-mid range is at least `0.78` and below `0.93`.",
        "This is not directly tradable; it is the label we are trying to explain with live-safe features.",
        "",
    ]
    render_market_windows(lines, windows)
    render_weekly(lines, markets)
    render_grouped_fills(lines, "Last-Window Final Range Buckets", late_fills, "final_range_bucket", args.min_fills)
    render_grouped_fills(lines, "Last-Window Observed Range Buckets", late_fills, "observed_range_bucket", args.min_fills)
    render_grouped_fills(lines, "Last-Window Live Regime Labels", late_fills, "live_regime_label", args.min_fills)
    if any(f.get("post_fill_bucket") for f in late_fills):
        render_grouped_fills(lines, "Last-Window Post-Fill Path Buckets", late_fills, "post_fill_bucket", args.min_fills)
    render_feature_drift(lines, early_fills, late_fills)
    render_rule_diagnostics(lines, early_fills, late_fills)

    Path(args.out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(args.out_md).write_text("\n".join(lines) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
