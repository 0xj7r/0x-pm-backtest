#!/usr/bin/env python3
"""Screen replay-safe low-vol continuation/fade rules from decision logs.

This is not an engine backtest. It is a fast hypothesis filter that takes at
most one synthetic taker entry per market for each rule, using only decision-row
features available at that event. Rules that survive here still need engine
implementation and full walk-forward validation.
"""

from __future__ import annotations

import argparse
import json
import math
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Iterable


@dataclass(frozen=True)
class MarketLabel:
    slug: str
    close_ts: int
    yes_resolved: bool


@dataclass(frozen=True)
class Rule:
    name: str
    direction: str
    predicate: Callable[[dict[str, Any]], bool]


@dataclass(frozen=True)
class RegimeCell:
    name: str
    predicate: Callable[[dict[str, Any]], bool]


def parse_close_ts(slug: str, fallback: int) -> int:
    try:
        return int(slug.rsplit("-", 1)[1]) + 300
    except Exception:
        return fallback


def load_manifest(path: Path, skip: int, max_markets: int) -> dict[int, MarketLabel]:
    rows = []
    with path.open() as fh:
        for line in fh:
            if not line.strip():
                continue
            row = json.loads(line)
            close_ts = int(row.get("close_ts") or parse_close_ts(str(row.get("slug")), 0))
            rows.append((close_ts, row))
    rows.sort(key=lambda item: item[0])
    if skip:
        rows = rows[skip:]
    if max_markets:
        rows = rows[:max_markets]

    out: dict[int, MarketLabel] = {}
    for idx, (_, row) in enumerate(rows, 1):
        outcome = str(row.get("outcome"))
        if outcome.lower() in {"up", "yes"}:
            yes_resolved = True
        elif outcome.lower() in {"down", "no"}:
            yes_resolved = False
        else:
            raise ValueError(f"bad outcome label for {row.get('slug')}: {outcome!r}")
        out[idx] = MarketLabel(
            slug=str(row.get("slug")),
            close_ts=int(row.get("close_ts") or parse_close_ts(str(row.get("slug")), 0)),
            yes_resolved=yes_resolved,
        )
    return out


def iter_decisions(path: Path) -> Iterable[dict[str, Any]]:
    with path.open() as fh:
        for line in fh:
            if line.strip():
                yield json.loads(line)


def f(row: dict[str, Any], key: str) -> float:
    value = row.get(key)
    if value is None:
        return 0.0
    try:
        value_f = float(value)
    except (TypeError, ValueError):
        return 0.0
    if not math.isfinite(value_f):
        return 0.0
    return value_f


def side_price(row: dict[str, Any], side_is_yes: bool) -> float:
    yes_ask = f(row, "yes_ask")
    yes_bid = f(row, "yes_bid")
    return yes_ask if side_is_yes else max(0.0, 1.0 - yes_bid)


def enriched_rows(decision_log: Path, labels: dict[int, MarketLabel]) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for row in iter_decisions(decision_log):
        market_id = int(row.get("market_id") or 0)
        label = labels.get(market_id)
        if label is None or not row.get("has_model_output"):
            continue
        ts_s = int(row.get("ts_ns") or 0) / 1_000_000_000.0
        seconds_to_close = max(0.0, label.close_ts - ts_s)
        model_side_yes = bool(row.get("side_is_yes"))
        long_price = side_price(row, model_side_yes)
        fade_side_yes = not model_side_yes
        fade_price = side_price(row, fade_side_yes)
        calibrated_p = f(row, "calibrated_p")
        item = dict(row)
        item["_market_id"] = market_id
        item["_slug"] = label.slug
        item["_yes_resolved"] = label.yes_resolved
        item["_seconds_to_close"] = seconds_to_close
        item["_long_side_yes"] = model_side_yes
        item["_long_price"] = long_price
        item["_long_edge"] = calibrated_p - long_price
        item["_fade_side_yes"] = fade_side_yes
        item["_fade_price"] = fade_price
        item["_fade_edge"] = (1.0 - calibrated_p) - fade_price
        rows.append(item)
    rows.sort(key=lambda row: (row["_market_id"], int(row.get("event_idx") or 0)))
    return rows


def pnl_one_share(price: float, side_yes: bool, yes_resolved: bool) -> float:
    won = side_yes == yes_resolved
    return (1.0 - price) if won else -price


def summarize_rule(rows: list[dict[str, Any]], rule: Rule) -> dict[str, Any]:
    selected = []
    seen_market: set[int] = set()
    for row in rows:
        market_id = int(row["_market_id"])
        if market_id in seen_market:
            continue
        if not rule.predicate(row):
            continue
        seen_market.add(market_id)
        if rule.direction == "long":
            price = row["_long_price"]
            side_yes = row["_long_side_yes"]
            edge = row["_long_edge"]
        elif rule.direction == "fade":
            price = row["_fade_price"]
            side_yes = row["_fade_side_yes"]
            edge = row["_fade_edge"]
        else:
            raise ValueError(f"unknown direction {rule.direction}")
        pnl = pnl_one_share(price, side_yes, bool(row["_yes_resolved"]))
        selected.append((row, price, edge, pnl))

    total = sum(item[3] for item in selected)
    wins = sum(1 for item in selected if item[3] > 0.0)
    n = len(selected)
    prices = [item[1] for item in selected]
    edges = [item[2] for item in selected]
    return {
        "name": rule.name,
        "direction": rule.direction,
        "trades": n,
        "pnl_per_share": total,
        "pnl_per_100": total * 100.0,
        "hit_rate": wins / n if n else 0.0,
        "avg_price": sum(prices) / n if n else 0.0,
        "avg_edge": sum(edges) / n if n else 0.0,
        "worst": min((item[3] for item in selected), default=0.0),
        "best": max((item[3] for item in selected), default=0.0),
    }


def summarize_regime_cell(rows: list[dict[str, Any]], cell: RegimeCell) -> dict[str, Any]:
    selected = []
    seen_market: set[int] = set()
    for row in rows:
        market_id = int(row["_market_id"])
        if market_id in seen_market or not cell.predicate(row):
            continue
        seen_market.add(market_id)
        long_pnl = pnl_one_share(
            row["_long_price"],
            bool(row["_long_side_yes"]),
            bool(row["_yes_resolved"]),
        )
        fade_pnl = pnl_one_share(
            row["_fade_price"],
            bool(row["_fade_side_yes"]),
            bool(row["_yes_resolved"]),
        )
        selected.append((row, long_pnl, fade_pnl))

    n = len(selected)
    long_total = sum(item[1] for item in selected)
    fade_total = sum(item[2] for item in selected)
    long_wins = sum(1 for item in selected if item[1] > 0.0)
    fade_wins = sum(1 for item in selected if item[2] > 0.0)
    prices = [item[0]["_long_price"] for item in selected]
    risks = [f(item[0], "risk_score") for item in selected]
    ranges = [f(item[0], "feature_observed_yes_range_so_far") for item in selected]
    whipsaws = [f(item[0], "feature_whipsaw") for item in selected]
    return {
        "name": cell.name,
        "trades": n,
        "long_pnl_per_share": long_total,
        "long_pnl_per_100": long_total * 100.0,
        "long_hit_rate": long_wins / n if n else 0.0,
        "fade_pnl_per_share": fade_total,
        "fade_pnl_per_100": fade_total * 100.0,
        "fade_hit_rate": fade_wins / n if n else 0.0,
        "avg_long_price": sum(prices) / n if n else 0.0,
        "avg_risk": sum(risks) / n if n else 0.0,
        "avg_range": sum(ranges) / n if n else 0.0,
        "avg_whipsaw": sum(whipsaws) / n if n else 0.0,
    }


def rules() -> list[Rule]:
    base_window = lambda r: 20.0 <= r["_seconds_to_close"] <= 160.0
    has_price = lambda r: 0.02 <= r["_long_price"] <= 0.98 and 0.02 <= r["_fade_price"] <= 0.98
    emitted = lambda r: int(r.get("orders_requested") or 0) > 0
    model = lambda r: bool(r.get("has_model_output")) and f(r, "calibrated_p") >= 0.55

    return [
        Rule("engine_emitted_long", "long", lambda r: emitted(r) and has_price(r)),
        Rule(
            "long_high_price_low_risk",
            "long",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_long_price"] >= 0.78
            and f(r, "risk_score") <= 0.52,
        ),
        Rule(
            "long_edge_low_risk",
            "long",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_long_edge"] >= 0.02
            and r["_long_price"] >= 0.72
            and f(r, "risk_score") <= 0.56,
        ),
        Rule(
            "long_high_price_low_risk_no_low_range",
            "long",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_long_price"] >= 0.78
            and f(r, "risk_score") <= 0.52
            and f(r, "feature_observed_yes_range_so_far") >= 0.45,
        ),
        Rule(
            "long_higher_vol_only",
            "long",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and f(r, "feature_volatility_regime") >= 1.0,
        ),
        Rule(
            "long_hi_price_hi_range",
            "long",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_long_price"] >= 0.82
            and f(r, "feature_observed_yes_range_so_far") >= 0.45,
        ),
        Rule(
            "late_may_continuation_cell",
            "long",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_long_price"] >= 0.82
            and f(r, "risk_score") <= 0.56
            and f(r, "feature_observed_yes_range_so_far") >= 0.45,
        ),
        Rule(
            "fade_cheap_low_range_low_vol",
            "fade",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_long_price"] <= 0.82
            and f(r, "feature_observed_yes_range_so_far") <= 0.45
            and f(r, "feature_volatility_regime") <= 1.0,
        ),
        Rule(
            "fade_cheap_low_vol_mid_risk",
            "fade",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_long_price"] <= 0.82
            and f(r, "feature_volatility_regime") <= 1.0
            and f(r, "risk_score") >= 0.52,
        ),
        Rule(
            "fade_positive_edge",
            "fade",
            lambda r: base_window(r)
            and has_price(r)
            and model(r)
            and r["_fade_edge"] >= 0.02,
        ),
        Rule(
            "fade_late_cheap",
            "fade",
            lambda r: 20.0 <= r["_seconds_to_close"] <= 80.0
            and has_price(r)
            and model(r)
            and r["_long_price"] <= 0.78,
        ),
    ]


def regime_cells() -> list[RegimeCell]:
    base_window = lambda r: 20.0 <= r["_seconds_to_close"] <= 160.0
    has_price = lambda r: 0.02 <= r["_long_price"] <= 0.98 and 0.02 <= r["_fade_price"] <= 0.98
    model = lambda r: bool(r.get("has_model_output")) and f(r, "calibrated_p") >= 0.55
    base = lambda r: base_window(r) and has_price(r) and model(r)

    return [
        RegimeCell(
            "high_cert_low_risk_continuation",
            lambda r: base(r) and r["_long_price"] >= 0.82 and f(r, "risk_score") <= 0.52,
        ),
        RegimeCell(
            "expanded_range_continuation",
            lambda r: base(r)
            and r["_long_price"] >= 0.78
            and f(r, "feature_observed_yes_range_so_far") >= 0.45
            and f(r, "risk_score") <= 0.56,
        ),
        RegimeCell(
            "cheap_low_range_favourite",
            lambda r: base(r)
            and r["_long_price"] <= 0.82
            and f(r, "feature_observed_yes_range_so_far") <= 0.45,
        ),
        RegimeCell(
            "low_vol_low_range_drift",
            lambda r: base(r)
            and f(r, "feature_volatility_regime") <= 1.0
            and f(r, "feature_observed_yes_range_so_far") <= 0.45
            and f(r, "feature_whipsaw") <= 0.55,
        ),
        RegimeCell(
            "low_vol_low_range_whipsaw",
            lambda r: base(r)
            and f(r, "feature_volatility_regime") <= 1.0
            and f(r, "feature_observed_yes_range_so_far") <= 0.45
            and f(r, "feature_whipsaw") > 0.55,
        ),
        RegimeCell(
            "late_cheap_favourite_toxic",
            lambda r: 20.0 <= r["_seconds_to_close"] <= 80.0
            and has_price(r)
            and model(r)
            and r["_long_price"] <= 0.78,
        ),
    ]


def markdown_table(
    results_by_slice: dict[str, list[dict[str, Any]]],
    cells_by_slice: dict[str, list[dict[str, Any]]],
) -> str:
    rule_names = [r.name for r in rules()]
    lines = [
        "# Low-Vol Microstructure Strategy Screen",
        "",
        "Synthetic screen: one first eligible entry per market per rule, one share, taker at visible ask proxy. This is a hypothesis filter, not final P&L.",
        "",
        "| Rule | Slice | Direction | Trades | PnL/share | PnL/100sh | Hit | Avg Price | Avg Edge | Worst |",
        "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for rule_name in rule_names:
        for slice_name, results in results_by_slice.items():
            row = next(item for item in results if item["name"] == rule_name)
            lines.append(
                f"| {rule_name} | {slice_name} | {row['direction']} | {row['trades']} | "
                f"{row['pnl_per_share']:.2f} | {row['pnl_per_100']:.2f} | "
                f"{100.0 * row['hit_rate']:.1f}% | {row['avg_price']:.3f} | "
                f"{row['avg_edge']:.3f} | {row['worst']:.3f} |"
            )
    lines.append("")
    lines.extend(
        [
            "## Regime Cells",
            "",
            "Each cell takes the first eligible row per market and scores both model-side continuation and the opposite-side fade at visible ask proxy.",
            "",
            "| Cell | Slice | Trades | Long PnL/share | Long Hit | Fade PnL/share | Fade Hit | Avg Price | Avg Risk | Avg Range | Avg Whipsaw |",
            "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
        ]
    )
    for cell in regime_cells():
        for slice_name, results in cells_by_slice.items():
            row = next(item for item in results if item["name"] == cell.name)
            lines.append(
                f"| {cell.name} | {slice_name} | {row['trades']} | "
                f"{row['long_pnl_per_share']:.2f} | {100.0 * row['long_hit_rate']:.1f}% | "
                f"{row['fade_pnl_per_share']:.2f} | {100.0 * row['fade_hit_rate']:.1f}% | "
                f"{row['avg_long_price']:.3f} | {row['avg_risk']:.3f} | "
                f"{row['avg_range']:.3f} | {row['avg_whipsaw']:.3f} |"
            )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=Path("data/runs/volgate/markets-may-labeled.jsonl"))
    parser.add_argument("--slice", action="append", nargs=4, metavar=("NAME", "DECISION_LOG", "SKIP", "MAX_MARKETS"), required=True)
    parser.add_argument("--out-md", type=Path, required=True)
    args = parser.parse_args()

    results_by_slice: dict[str, list[dict[str, Any]]] = {}
    cells_by_slice: dict[str, list[dict[str, Any]]] = {}
    for name, decision_log, skip_s, max_s in args.slice:
        labels = load_manifest(args.manifest, int(skip_s), int(max_s))
        rows = enriched_rows(Path(decision_log), labels)
        results_by_slice[name] = [summarize_rule(rows, rule) for rule in rules()]
        cells_by_slice[name] = [summarize_regime_cell(rows, cell) for cell in regime_cells()]

    args.out_md.parent.mkdir(parents=True, exist_ok=True)
    args.out_md.write_text(markdown_table(results_by_slice, cells_by_slice))
    print(f"wrote {args.out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
