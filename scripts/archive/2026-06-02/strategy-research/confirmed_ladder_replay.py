#!/usr/bin/env python3
"""Replay simple confirmed-winner ladder taker strategies on cached BTC 5m books.

This is a deliberately simple competitor-style hypothesis test:

* choose the book favourite during the final part of the market,
* require Binance window-delta to confirm the same side,
* keep buying up the ladder at a fixed cadence and clip,
* optionally buy a tiny cheap opposite tail as crash insurance,
* hold everything to resolution.

The local cache is BTC 5m and YES-token only. NO prices are mirrored from the
YES book (`NO ask = 1 - YES bid`), so this is a directional taker screen rather
than a production execution proof.
"""

from __future__ import annotations

import argparse
import csv
import glob
import json
import math
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

import numpy as np

import mm_paired_sim as S


@dataclass(frozen=True)
class LadderRule:
    start_t_left: float
    end_t_left: float
    interval_s: float
    min_abs_delta_bps: float
    min_price: float
    max_price: float
    max_orders: int
    clip_usdc: float
    min_spot_30s_bps: float
    tail_frac: float
    tail_max_price: float

    @property
    def label(self) -> str:
        return (
            f"start{self.start_t_left:.0f}_end{self.end_t_left:.0f}_"
            f"int{self.interval_s:.0f}_d{self.min_abs_delta_bps:.1f}_"
            f"px{self.min_price:.2f}-{self.max_price:.2f}_"
            f"n{self.max_orders}_clip{self.clip_usdc:.0f}_"
            f"m30{self.min_spot_30s_bps:.1f}_tail{self.tail_frac:.2f}"
        )


def spot_at(ts: np.ndarray, px: np.ndarray, when: float) -> float | None:
    idx = np.searchsorted(ts, when, side="right") - 1
    if idx < 0:
        return None
    value = float(px[idx])
    return value if math.isfinite(value) and value > 0.0 else None


def spot_return_bps(ts: np.ndarray, px: np.ndarray, when: float, lookback_s: float) -> float | None:
    now = spot_at(ts, px, when)
    prev = spot_at(ts, px, when - lookback_s)
    if now is None or prev is None:
        return None
    return (now / prev - 1.0) * 10_000.0


def snapshot_at(book: dict[str, Any], t_left: float) -> dict[str, float] | None:
    t = book["t"]
    idx = int(np.argmin(np.abs(t - t_left)))
    if abs(float(t[idx]) - t_left) > max(2.0, min(8.0, t_left * 0.20)):
        return None
    bid = float(book["bid"][idx])
    ask = float(book["ask"][idx])
    mid = float(book["mid"][idx])
    if not (0.0 < bid <= ask < 1.0 and 0.0 < mid < 1.0):
        return None
    return {
        "yes_bid": bid,
        "yes_ask": ask,
        "yes_mid": mid,
        "no_ask": max(0.0, 1.0 - bid),
    }


def one_order_pnl(shares: float, price: float, buy_yes: bool, yes_wins: bool) -> float:
    payoff = 1.0 if buy_yes == yes_wins else 0.0
    return shares * (payoff - price)


def simulate_market(
    book: dict[str, Any],
    close: int,
    bin_day: tuple[np.ndarray, np.ndarray] | None,
    date: str,
    rule: LadderRule,
) -> dict[str, Any] | None:
    yes_wins, spot_ts, spot_px = S.binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None
    start_px = spot_at(spot_ts, spot_px, close - S.WINDOW)
    if start_px is None:
        return None

    orders: list[dict[str, Any]] = []
    pnl = 0.0
    spend = 0.0
    t_values = np.arange(rule.start_t_left, rule.end_t_left - 1e-9, -rule.interval_s)
    for t_left in t_values:
        if len([o for o in orders if not o["tail"]]) >= rule.max_orders:
            break
        snap = snapshot_at(book, float(t_left))
        if snap is None:
            continue
        now_px = spot_at(spot_ts, spot_px, close - float(t_left))
        if now_px is None:
            continue
        delta_bps = (now_px / start_px - 1.0) * 10_000.0
        if abs(delta_bps) < rule.min_abs_delta_bps:
            continue
        spot_buy_yes = delta_bps >= 0.0
        fav_buy_yes = snap["yes_mid"] >= 0.5
        if fav_buy_yes != spot_buy_yes:
            continue
        ret30 = spot_return_bps(spot_ts, spot_px, close - float(t_left), 30.0)
        if ret30 is None:
            continue
        if spot_buy_yes and ret30 < rule.min_spot_30s_bps:
            continue
        if (not spot_buy_yes) and ret30 > -rule.min_spot_30s_bps:
            continue

        price = snap["yes_ask"] if fav_buy_yes else snap["no_ask"]
        if price < rule.min_price or price > rule.max_price:
            continue
        shares = rule.clip_usdc / price
        order_pnl = one_order_pnl(shares, price, fav_buy_yes, bool(yes_wins))
        pnl += order_pnl
        spend += rule.clip_usdc
        orders.append(
            {
                "t_left": float(t_left),
                "buy_yes": fav_buy_yes,
                "price": price,
                "shares": shares,
                "pnl": order_pnl,
                "delta_bps": delta_bps,
                "ret30_bps": ret30,
                "tail": False,
            }
        )

        if rule.tail_frac > 0.0:
            tail_buy_yes = not fav_buy_yes
            tail_price = snap["no_ask"] if fav_buy_yes else snap["yes_ask"]
            if 0.0 < tail_price <= rule.tail_max_price:
                tail_usdc = rule.clip_usdc * rule.tail_frac
                tail_shares = tail_usdc / tail_price
                tail_pnl = one_order_pnl(tail_shares, tail_price, tail_buy_yes, bool(yes_wins))
                pnl += tail_pnl
                spend += tail_usdc
                orders.append(
                    {
                        "t_left": float(t_left),
                        "buy_yes": tail_buy_yes,
                        "price": tail_price,
                        "shares": tail_shares,
                        "pnl": tail_pnl,
                        "delta_bps": delta_bps,
                        "ret30_bps": ret30,
                        "tail": True,
                    }
                )

    if not orders:
        return None
    return {
        "date": date,
        "close": close,
        "yes_wins": bool(yes_wins),
        "orders": len(orders),
        "primary_orders": len([o for o in orders if not o["tail"]]),
        "tail_orders": len([o for o in orders if o["tail"]]),
        "spend": spend,
        "pnl": pnl,
        "roi": pnl / spend if spend else 0.0,
        "worst_order": min(o["pnl"] for o in orders),
        "avg_price": float(np.mean([o["price"] for o in orders if not o["tail"]])),
        "orders_detail": orders,
    }


def summarize(rows: list[dict[str, Any]], rule: LadderRule, split_label: str) -> dict[str, Any]:
    if not rows:
        out = asdict(rule)
        out.update({"label": rule.label, "split": split_label, "markets": 0, "fills": 0, "pnl": 0.0, "spend": 0.0, "roi": 0.0})
        return out
    pnl = np.array([r["pnl"] for r in rows], dtype=np.float64)
    spend = float(sum(r["spend"] for r in rows))
    ordered = sorted(rows, key=lambda r: r["close"])
    equity = np.cumsum([r["pnl"] for r in ordered])
    peak = np.maximum.accumulate(np.maximum(equity, 0.0))
    dd = peak - equity
    out = asdict(rule)
    out.update(
        {
            "label": rule.label,
            "split": split_label,
            "markets": len(rows),
            "fills": int(sum(r["orders"] for r in rows)),
            "primary_fills": int(sum(r["primary_orders"] for r in rows)),
            "tail_fills": int(sum(r["tail_orders"] for r in rows)),
            "pnl": float(pnl.sum()),
            "spend": spend,
            "roi": float(pnl.sum() / spend) if spend else 0.0,
            "hit_rate": float((pnl > 0.0).mean()),
            "avg_market_pnl": float(pnl.mean()),
            "p10_market_pnl": float(np.percentile(pnl, 10)),
            "worst_market": float(pnl.min()),
            "max_drawdown": float(dd.max()) if len(dd) else 0.0,
            "avg_price": float(np.mean([r["avg_price"] for r in rows])),
        }
    )
    return out


def evaluate_rule(parsed: list[tuple[Any, int, Any, Any, str]], rule: LadderRule) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    rows = []
    for book, close, _trades, bin_day, date in parsed:
        r = simulate_market(book, close, bin_day, date, rule)
        if r is not None:
            rows.append(r)
    early = [r for r in rows if r["date"] <= "2026-05-13"]
    late = [r for r in rows if r["date"] >= "2026-05-14"]
    return summarize(rows, rule, "all"), summarize(early, rule, "early"), summarize(late, rule, "late")


def candidate_rules(clip_usdc: float) -> list[LadderRule]:
    rules = []
    for start in (180.0, 120.0, 90.0, 60.0, 30.0):
        for end in (5.0, 15.0, 30.0):
            if end >= start:
                continue
            for min_delta in (1.0, 2.0, 5.0, 10.0, 20.0):
                for min_price, max_price in ((0.55, 0.90), (0.60, 0.95), (0.70, 0.97), (0.80, 0.98), (0.90, 0.99)):
                    for max_orders in (1, 3, 6, 12):
                        for ret30 in (0.0, 1.0, 2.0, 5.0):
                            rules.append(
                                LadderRule(
                                    start_t_left=start,
                                    end_t_left=end,
                                    interval_s=10.0,
                                    min_abs_delta_bps=min_delta,
                                    min_price=min_price,
                                    max_price=max_price,
                                    max_orders=max_orders,
                                    clip_usdc=clip_usdc,
                                    min_spot_30s_bps=ret30,
                                    tail_frac=0.0,
                                    tail_max_price=0.08,
                                )
                            )
    # A small tail overlay only on higher-cert favourite profiles.
    for start in (60.0, 30.0):
        for min_delta in (5.0, 10.0, 20.0):
            for max_orders in (1, 3, 6):
                for tail_frac in (0.05, 0.10, 0.20):
                    rules.append(
                        LadderRule(
                            start_t_left=start,
                            end_t_left=5.0,
                            interval_s=10.0,
                            min_abs_delta_bps=min_delta,
                            min_price=0.80,
                            max_price=0.98,
                            max_orders=max_orders,
                            clip_usdc=clip_usdc,
                            min_spot_30s_bps=1.0,
                            tail_frac=tail_frac,
                            tail_max_price=0.08,
                        )
                    )
    return rules


def robustness_score(all_row: dict[str, Any], early_row: dict[str, Any], late_row: dict[str, Any]) -> float:
    if early_row["markets"] < 20 or late_row["markets"] < 20:
        return -1e9
    if early_row["pnl"] <= 0.0 or late_row["pnl"] <= 0.0:
        return -1e6 + all_row["pnl"]
    return min(early_row["roi"], late_row["roi"]) * math.sqrt(all_row["fills"]) - 0.001 * all_row["max_drawdown"]


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        path.write_text("")
        return
    fieldnames = list(rows[0].keys())
    with path.open("w", newline="") as fh:
        writer = csv.DictWriter(fh, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def write_markdown(path: Path, ranked: list[dict[str, Any]], n_markets: int, n_rules: int) -> None:
    lines = [
        "# Confirmed Favourite Ladder Replay",
        "",
        f"Dataset: local cached BTC 5m books, parsed markets={n_markets}, candidate rules={n_rules}.",
        "",
        "Gross taker PnL from buying the book favourite only when Binance window delta and 30s spot momentum confirm the same side. NO asks are mirrored from YES bids, so this remains a screen.",
        "",
        "## Top Robust Rules",
        "",
        "| Rank | Rule | All PnL | All ROI | Early PnL | Late PnL | Fills | Markets | Worst | Max DD | Avg px |",
        "|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for i, row in enumerate(ranked[:25], 1):
        lines.append(
            f"| {i} | `{row['label']}` | {row['all_pnl']:.2f} | {100*row['all_roi']:.2f}% | "
            f"{row['early_pnl']:.2f} | {row['late_pnl']:.2f} | {row['all_fills']} | "
            f"{row['all_markets']} | {row['all_worst_market']:.2f} | {row['all_max_drawdown']:.2f} | "
            f"{row['all_avg_price']:.3f} |"
        )
    lines.append("")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--limit", type=int, default=0)
    parser.add_argument("--clip-usdc", type=float, default=10.0)
    parser.add_argument("--out-csv", type=Path, default=Path("data/runs/confirmed_ladder/sweep.csv"))
    parser.add_argument("--out-md", type=Path, default=Path("docs/confirmed_ladder_replay_2026-06-01.md"))
    args = parser.parse_args()

    book_files = sorted(glob.glob(f"{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet"))
    if args.limit:
        step = max(1, len(book_files) // args.limit)
        book_files = book_files[::step][: args.limit]
    parsed = S.load_all_markets(book_files, {})
    rules = candidate_rules(args.clip_usdc)

    rows = []
    for idx, rule in enumerate(rules, 1):
        all_row, early_row, late_row = evaluate_rule(parsed, rule)
        flat = {
            "label": rule.label,
            "robustness_score": robustness_score(all_row, early_row, late_row),
        }
        for prefix, row in (("all", all_row), ("early", early_row), ("late", late_row)):
            for key, value in row.items():
                if key in {"label", "split"}:
                    continue
                flat[f"{prefix}_{key}"] = value
        rows.append(flat)
        if idx % 250 == 0:
            print(f"evaluated {idx}/{len(rules)} rules", flush=True)

    rows.sort(key=lambda r: (r["robustness_score"], r["all_pnl"]), reverse=True)
    write_csv(args.out_csv, rows)
    write_markdown(args.out_md, rows, len(parsed), len(rules))
    manifest = {
        "parsed_markets": len(parsed),
        "candidate_rules": len(rules),
        "clip_usdc": args.clip_usdc,
        "out_csv": str(args.out_csv),
        "out_md": str(args.out_md),
        "top": rows[:10],
    }
    manifest_path = args.out_csv.with_suffix(".json")
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    print(f"wrote {args.out_csv}")
    print(f"wrote {args.out_md}")
    print(f"wrote {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
