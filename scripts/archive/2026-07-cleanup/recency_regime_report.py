#!/usr/bin/env python3
"""Analyze recency decay and regime drivers from walk-forward markets JSONL."""

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


FEATURES = [
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
    slug = str(row.get("slug") or "")
    return int(slug.rsplit("-", 1)[1]) + 300


def strategy_result(row: dict[str, Any], strategy: str) -> dict[str, Any]:
    return ((row.get("per_strategy") or {}).get(strategy)) or {}


def fill_won(row: dict[str, Any], side: str) -> bool | None:
    outcome = str(row.get("outcome_label") or "").lower()
    if side == "BuyYes":
        return outcome in ("yes", "up")
    if side == "BuyNo":
        return outcome in ("no", "down")
    return None


def fill_pnl(row: dict[str, Any], fill: dict[str, Any]) -> tuple[float, bool | None]:
    won = fill_won(row, str(fill.get("side") or ""))
    shares = float(fill.get("shares") or 0.0)
    notional = float(fill.get("notional") or 0.0)
    rebate = float(fill.get("rebate_usdc") or 0.0)
    return (shares if won else 0.0) - notional + rebate, won


def week_start(day: dt.date) -> dt.date:
    return day - dt.timedelta(days=day.weekday())


def fmt(value: float, decimals: int = 2) -> str:
    return f"{value:.{decimals}f}"


def pct(value: float, decimals: int = 2) -> str:
    return f"{value:.{decimals}f}%"


def empty_stat() -> dict[str, Any]:
    return {
        "markets": 0,
        "active": 0,
        "fills": 0,
        "pnl": 0.0,
        "start_eq": None,
        "end_eq": None,
        "peak": None,
        "maxdd": 0.0,
        "tags": defaultdict(lambda: {"fills": 0, "pnl": 0.0, "cost": 0.0, "wins": 0}),
    }


def update_stat(stat: dict[str, Any], market: dict[str, Any]) -> None:
    if stat["start_eq"] is None:
        stat["start_eq"] = market["start_eq"]
        stat["peak"] = market["start_eq"]
    stat["markets"] += 1
    stat["pnl"] += market["pnl"]
    stat["fills"] += len(market["fills"])
    if market["fills"]:
        stat["active"] += 1
    stat["end_eq"] = market["end_eq"]
    stat["peak"] = max(float(stat["peak"]), market["end_eq"])
    if stat["peak"] > 0.0:
        stat["maxdd"] = max(float(stat["maxdd"]), (float(stat["peak"]) - market["end_eq"]) / float(stat["peak"]))
    for fill in market["fills"]:
        tag = fill["tag"]
        tag_stat = stat["tags"][tag]
        tag_stat["fills"] += 1
        tag_stat["pnl"] += fill["pnl"]
        tag_stat["cost"] += fill["notional"]
        tag_stat["wins"] += 1 if fill["won"] else 0


def summarize_stat(stat: dict[str, Any]) -> dict[str, Any]:
    start = float(stat["start_eq"] or 0.0)
    end = float(stat["end_eq"] or start)
    ret = ((end / start) - 1.0) if start > 0.0 else 0.0
    days = stat["markets"] / 288.0 if stat["markets"] else 0.0
    daily = (end / start) ** (1.0 / days) - 1.0 if start > 0.0 and end > 0.0 and days > 0.0 else 0.0
    return {
        "markets": stat["markets"],
        "active": stat["active"],
        "fills": stat["fills"],
        "pnl": stat["pnl"],
        "start_eq": start,
        "end_eq": end,
        "return": ret,
        "daily": daily,
        "maxdd": stat["maxdd"],
        "active_rate": stat["active"] / stat["markets"] if stat["markets"] else 0.0,
        "tags": stat["tags"],
    }


def quantile(values: list[float], q: float) -> float:
    if not values:
        return math.nan
    ordered = sorted(values)
    idx = min(len(ordered) - 1, max(0, round((len(ordered) - 1) * q)))
    return ordered[idx]


def feature_bins(fills: list[dict[str, Any]], feature: str, min_fills: int) -> list[dict[str, Any]]:
    vals = [float(f[feature]) for f in fills if f.get(feature) is not None and math.isfinite(float(f[feature]))]
    if len(vals) < max(min_fills, 4):
        return []
    qs = [quantile(vals, q) for q in (0.25, 0.50, 0.75)]
    bounds = [(-math.inf, qs[0]), (qs[0], qs[1]), (qs[1], qs[2]), (qs[2], math.inf)]
    rows = []
    for lo, hi in bounds:
        bucket = [f for f in fills if f.get(feature) is not None and lo <= float(f[feature]) <= hi]
        if len(bucket) < min_fills:
            continue
        pnl = sum(float(f["pnl"]) for f in bucket)
        cost = sum(float(f["notional"]) for f in bucket)
        wins = sum(1 for f in bucket if f["won"])
        rows.append(
            {
                "feature": feature,
                "lo": lo,
                "hi": hi,
                "fills": len(bucket),
                "pnl": pnl,
                "pnl_per_fill": pnl / len(bucket),
                "cost": cost,
                "wins": wins,
                "win_rate": wins / len(bucket),
            }
        )
    return rows


def threshold_scans(fills: list[dict[str, Any]], feature: str, min_fills: int) -> list[dict[str, Any]]:
    vals = sorted({float(f[feature]) for f in fills if f.get(feature) is not None and math.isfinite(float(f[feature]))})
    if len(vals) < 8:
        return []
    candidates = [quantile(vals, q) for q in (0.2, 0.33, 0.5, 0.67, 0.8)]
    rows = []
    for threshold in candidates:
        for direction in ("<=", ">="):
            if direction == "<=":
                bucket = [f for f in fills if f.get(feature) is not None and float(f[feature]) <= threshold]
            else:
                bucket = [f for f in fills if f.get(feature) is not None and float(f[feature]) >= threshold]
            if len(bucket) < min_fills:
                continue
            pnl = sum(float(f["pnl"]) for f in bucket)
            wins = sum(1 for f in bucket if f["won"])
            rows.append(
                {
                    "rule": f"{feature} {direction} {threshold:.4f}",
                    "fills": len(bucket),
                    "pnl": pnl,
                    "pnl_per_fill": pnl / len(bucket),
                    "win_rate": wins / len(bucket),
                }
            )
    return rows


def regime_label(fill: dict[str, Any]) -> str:
    whipsaw = float(fill.get("regime_whipsaw_score") or 0.0)
    path_eff = float(fill.get("regime_path_efficiency") or 0.0)
    reversal = float(fill.get("regime_reversal_pressure") or 0.0)
    sign_flip = float(fill.get("regime_sign_flip_rate") or 0.0)
    realized_vol = float(fill.get("regime_realized_vol_180s_bps") or 0.0)
    observed_range = float(fill.get("market_yes_range_so_far") or 0.0)

    if observed_range >= 0.70 and reversal >= 0.45:
        return "extreme_range_reversal"
    if observed_range >= 0.50 and whipsaw >= 0.35 and path_eff <= 0.35:
        return "wide_range_chop"
    if whipsaw >= 0.45 and reversal >= 0.45:
        return "reversal_chop"
    if whipsaw >= 0.35 and path_eff <= 0.45:
        return "noisy_chop"
    if path_eff >= 0.55 and whipsaw < 0.35:
        return "smooth_directional"
    if realized_vol >= 3.0 and sign_flip >= 0.40:
        return "volatile_flip"
    if observed_range >= 0.50:
        return "wide_range"
    return "neutral"


def grouped_fill_stats(fills: list[dict[str, Any]], key: str) -> list[tuple[str, dict[str, Any]]]:
    grouped: dict[str, dict[str, Any]] = defaultdict(
        lambda: {"fills": 0, "pnl": 0.0, "cost": 0.0, "wins": 0}
    )
    for fill in fills:
        value = str(fill.get(key) or "unknown")
        row = grouped[value]
        row["fills"] += 1
        row["pnl"] += float(fill["pnl"])
        row["cost"] += float(fill["notional"])
        row["wins"] += 1 if fill["won"] else 0
    return sorted(grouped.items(), key=lambda kv: kv[1]["pnl"])


def render_stat_row(name: str, stat: dict[str, Any]) -> str:
    s = summarize_stat(stat)
    return (
        f"| {name} | {s['markets']} | {s['active']} | {s['fills']} | "
        f"${fmt(s['pnl'])} | ${fmt(s['start_eq'])} | ${fmt(s['end_eq'])} | "
        f"{pct(s['return'] * 100.0)} | {pct(s['daily'] * 100.0, 3)} | "
        f"{pct(s['maxdd'] * 100.0)} | {pct(s['active_rate'] * 100.0)} |"
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets_jsonl", help="local path, s3:// path, or '-'")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile")
    parser.add_argument("--recent-days", type=int, default=30)
    parser.add_argument("--min-fills", type=int, default=12)
    parser.add_argument("--out", help="write markdown report to this path")
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    f, proc = open_input(args.markets_jsonl)
    markets: list[dict[str, Any]] = []
    try:
        for row in iter_rows(f):
            strat = strategy_result(row, args.strategy)
            ts = close_ts(row)
            fills = []
            for fill in strat.get("fills_detail") or []:
                pnl, won = fill_pnl(row, fill)
                enriched = {
                    "ts": ts,
                    "date": dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date(),
                    "slug": row.get("slug"),
                    "tag": str(fill.get("tag") or "unknown"),
                    "side": str(fill.get("side") or "unknown"),
                    "price": float(fill.get("price") or 0.0),
                    "shares": float(fill.get("shares") or 0.0),
                    "notional": float(fill.get("notional") or 0.0),
                    "pnl": pnl,
                    "won": bool(won),
                    "volatility_range": float(row.get("volatility_range") or 0.0),
                    "volatility_band": str(row.get("volatility_band") or "unknown"),
                }
                for feature in FEATURES:
                    if feature in enriched:
                        continue
                    value = fill.get(feature)
                    enriched[feature] = float(value) if value is not None else None
                enriched["regime_label"] = regime_label(enriched)
                fills.append(enriched)
            markets.append(
                {
                    "ts": ts,
                    "date": dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date(),
                    "slug": row.get("slug"),
                    "pnl": float(strat.get("pnl_usdc") or 0.0),
                    "start_eq": float(strat.get("start_equity_usdc") or 0.0),
                    "end_eq": float(strat.get("end_equity_usdc") or 0.0),
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

    if not markets:
        raise RuntimeError("no markets found")

    min_dt = dt.datetime.fromtimestamp(markets[0]["ts"], tz=dt.timezone.utc)
    max_dt = dt.datetime.fromtimestamp(markets[-1]["ts"], tz=dt.timezone.utc)
    last_start = max_dt - dt.timedelta(days=args.recent_days) + dt.timedelta(minutes=5)
    last_start_ts = int(last_start.timestamp())
    first_end_ts = markets[0]["ts"] + args.recent_days * 86400 - 300
    n = len(markets)

    windows: dict[str, list[dict[str, Any]]] = {
        "first_third": markets[: n // 3],
        "middle_third": markets[n // 3 : 2 * n // 3],
        "last_third": markets[2 * n // 3 :],
        f"first_{args.recent_days}d": [m for m in markets if m["ts"] <= first_end_ts],
        f"last_{args.recent_days}d": [m for m in markets if m["ts"] >= last_start_ts],
        "last_14d": [m for m in markets if m["ts"] >= int((max_dt - dt.timedelta(days=14) + dt.timedelta(minutes=5)).timestamp())],
        "last_7d": [m for m in markets if m["ts"] >= int((max_dt - dt.timedelta(days=7) + dt.timedelta(minutes=5)).timestamp())],
    }
    stats = {}
    for name, rows in windows.items():
        stat = empty_stat()
        for market in rows:
            update_stat(stat, market)
        stats[name] = stat

    recent_fills = [fill for market in windows[f"last_{args.recent_days}d"] for fill in market["fills"]]
    early_fills = [fill for market in windows[f"first_{args.recent_days}d"] for fill in market["fills"]]

    lines = []
    lines.append("# BTC5m Recency And Regime Report")
    lines.append("")
    lines.append(f"Generated from `{args.markets_jsonl}`.")
    lines.append(f"Artifact range: `{min_dt.isoformat()}` to `{max_dt.isoformat()}`.")
    lines.append("")
    lines.append("## Window Summary")
    lines.append("")
    lines.append("| Window | Markets | Active | Fills | PnL | Start | End | Return | Daily | Max DD | Active Rate |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for name in windows:
        lines.append(render_stat_row(name, stats[name]))

    lines.append("")
    lines.append("## Fill Tag Drift")
    lines.append("")
    lines.append("| Window | Tag | Fills | PnL | Cost | Wins | Win Rate |")
    lines.append("|---|---|---:|---:|---:|---:|---:|")
    for name in (f"first_{args.recent_days}d", f"last_{args.recent_days}d", "last_14d", "last_7d"):
        for tag, tag_stat in sorted(stats[name]["tags"].items()):
            fills = int(tag_stat["fills"])
            wr = tag_stat["wins"] / fills if fills else 0.0
            lines.append(
                f"| {name} | {tag} | {fills} | ${fmt(tag_stat['pnl'])} | "
                f"${fmt(tag_stat['cost'])} | {tag_stat['wins']} | {pct(wr * 100.0)} |"
            )

    lines.append("")
    lines.append("## Recent Regime Bins")
    lines.append("")
    lines.append("Recent bins use fill-level PnL in the last window. They are diagnostic, not a promotion rule.")
    lines.append("")
    lines.append("### Deterministic Labels")
    lines.append("")
    lines.append("| Tag | Regime | Fills | PnL | Cost | Wins | Win Rate |")
    lines.append("|---|---|---:|---:|---:|---:|---:|")
    for tag in sorted({fill["tag"] for fill in recent_fills}):
        tag_fills = [fill for fill in recent_fills if fill["tag"] == tag]
        for label, row in grouped_fill_stats(tag_fills, "regime_label"):
            if row["fills"] < args.min_fills:
                continue
            wr = row["wins"] / row["fills"] if row["fills"] else 0.0
            lines.append(
                f"| {tag} | {label} | {row['fills']} | ${fmt(row['pnl'])} | "
                f"${fmt(row['cost'])} | {row['wins']} | {pct(wr * 100.0)} |"
            )

    lines.append("")
    lines.append("### Quantile Feature Bins")
    lines.append("")
    lines.append("| Tag | Feature | Range | Fills | PnL | PnL/Fill | Win Rate |")
    lines.append("|---|---|---:|---:|---:|---:|---:|")
    for tag in sorted({fill["tag"] for fill in recent_fills}):
        tag_fills = [fill for fill in recent_fills if fill["tag"] == tag]
        for feature in FEATURES + ["volatility_range"]:
            for row in feature_bins(tag_fills, feature, args.min_fills):
                lo = "-inf" if row["lo"] == -math.inf else fmt(row["lo"], 4)
                hi = "inf" if row["hi"] == math.inf else fmt(row["hi"], 4)
                lines.append(
                    f"| {tag} | {feature} | {lo}..{hi} | {row['fills']} | "
                    f"${fmt(row['pnl'])} | ${fmt(row['pnl_per_fill'])} | {pct(row['win_rate'] * 100.0)} |"
                )

    lines.append("")
    lines.append("## Best Recent Thresholds")
    lines.append("")
    lines.append("Top single-feature recent filters by PnL per fill with minimum fill count.")
    lines.append("")
    lines.append("| Tag | Rule | Fills | PnL | PnL/Fill | Win Rate |")
    lines.append("|---|---|---:|---:|---:|---:|")
    for tag in sorted({fill["tag"] for fill in recent_fills}):
        tag_fills = [fill for fill in recent_fills if fill["tag"] == tag]
        scans = []
        for feature in FEATURES + ["volatility_range"]:
            scans.extend(threshold_scans(tag_fills, feature, args.min_fills))
        scans.sort(key=lambda row: (row["pnl_per_fill"], row["pnl"]), reverse=True)
        for row in scans[:12]:
            lines.append(
                f"| {tag} | `{row['rule']}` | {row['fills']} | ${fmt(row['pnl'])} | "
                f"${fmt(row['pnl_per_fill'])} | {pct(row['win_rate'] * 100.0)} |"
            )

    output = "\n".join(lines) + "\n"
    if args.out:
        Path(args.out).write_text(output)
    else:
        print(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
