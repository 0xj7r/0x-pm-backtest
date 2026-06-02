#!/usr/bin/env python3
"""Competitor-style crypto up/down replay harness.

This harness tests the strategy family suggested by profitable wallet activity:

1. passive two-sided accumulation when implied pair cost is attractive,
2. tight residual control, with optional spot-confirmed residual lean,
3. late favourite / discount-to-par taker sleeve,
4. explicit size, cadence, and pull controls,
5. clean PnL attribution by pair base, residual, rebate, and late sleeve.

Current local data coverage is BTC 5m YES-book cache. The code keeps asset and
horizon labels in outputs so the same report shape can be extended to ETH/SOL/XRP
and 15m/1h once those historical books are materialized locally.
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

REBATE_FRAC = S.REBATE_FRAC


@dataclass(frozen=True)
class PairRule:
    clip_shares: float
    max_pair_cost: float
    repair_delta_shares: float
    lean_delta_shares: float
    min_abs_delta_bps_for_lean: float
    stop_before_close_s: float
    max_spot_30s_abs_bps: float
    large_taker_pull_shares: float
    min_mid: float
    max_mid: float
    rebate_on: bool
    quote_mode: str = "passive_bid"

    @property
    def label(self) -> str:
        return (
            f"pair_clip{self.clip_shares:.0f}_cost{self.max_pair_cost:.3f}_"
            f"repair{self.repair_delta_shares:.0f}_lean{self.lean_delta_shares:.0f}_"
            f"d{self.min_abs_delta_bps_for_lean:.1f}_pull{self.stop_before_close_s:.0f}_"
            f"spot{self.max_spot_30s_abs_bps:.1f}_reb{int(self.rebate_on)}_"
            f"q{self.quote_mode}"
        )


@dataclass(frozen=True)
class LateRule:
    enabled: bool
    start_t_left: float
    end_t_left: float
    interval_s: float
    min_abs_delta_bps: float
    min_spot_30s_bps: float
    min_price: float
    max_price: float
    max_orders: int
    clip_usdc: float

    @property
    def label(self) -> str:
        if not self.enabled:
            return "late_off"
        return (
            f"late_start{self.start_t_left:.0f}_end{self.end_t_left:.0f}_"
            f"d{self.min_abs_delta_bps:.1f}_m30{self.min_spot_30s_bps:.1f}_"
            f"px{self.min_price:.2f}-{self.max_price:.2f}_n{self.max_orders}_clip{self.clip_usdc:.0f}"
        )


@dataclass(frozen=True)
class StrategyRule:
    pair: PairRule
    late: LateRule

    @property
    def label(self) -> str:
        return f"{self.pair.label}__{self.late.label}"


def spot_at(ts: np.ndarray, px: np.ndarray, when: float) -> float | None:
    idx = np.searchsorted(ts, when, side="right") - 1
    if idx < 0:
        return None
    value = float(px[idx])
    return value if math.isfinite(value) and value > 0.0 else None


def spot_return_bps(ts: np.ndarray, px: np.ndarray, when: float, lookback_s: float) -> float:
    now = spot_at(ts, px, when)
    prev = spot_at(ts, px, when - lookback_s)
    if now is None or prev is None:
        return 0.0
    return (now / prev - 1.0) * 10_000.0


def window_delta_bps(ts: np.ndarray, px: np.ndarray, close: int, t_left: float) -> float | None:
    start = spot_at(ts, px, close - S.WINDOW)
    now = spot_at(ts, px, close - t_left)
    if start is None or now is None:
        return None
    return (now / start - 1.0) * 10_000.0


def snap_idx(book: dict[str, Any], wall_time: float) -> int:
    idx = np.searchsorted(book["wall"], wall_time, side="right") - 1
    return max(0, min(idx, len(book["wall"]) - 1))


def allocate_pair_quotes(bid: float, ask: float, cap: float, mode: str, delta_bps: float) -> tuple[float, float, float] | None:
    """Return native YES/NO maker bids under a shared pair-cost cap.

    The local cache only has the YES book, so the native NO bid is mirrored as
    1 - YES ask. Improvement inside the spread spends slack while staying maker.
    """
    base_yes = bid
    base_no = 1.0 - ask
    base_pair_cost = base_yes + base_no
    slack = cap - base_pair_cost
    if slack < -1e-9:
        return None
    yes_capacity = max(0.0, ask - bid - 1e-6)
    no_capacity = max(0.0, ask - bid - 1e-6)

    yes_extra = 0.0
    no_extra = 0.0
    if mode == "shared_slack_even":
        yes_extra = min(yes_capacity, slack * 0.5)
        no_extra = min(no_capacity, slack - yes_extra)
        leftover = slack - yes_extra - no_extra
        if leftover > 0.0:
            add_yes = min(yes_capacity - yes_extra, leftover)
            yes_extra += add_yes
            leftover -= add_yes
            no_extra += min(no_capacity - no_extra, leftover)
    elif mode == "shared_slack_leader":
        yes_weight = 0.75 if delta_bps > 0.0 else 0.25 if delta_bps < 0.0 else 0.5
        yes_extra = min(yes_capacity, slack * yes_weight)
        no_extra = min(no_capacity, slack - yes_extra)
        leftover = slack - yes_extra - no_extra
        if leftover > 0.0:
            if yes_weight >= 0.5:
                add_yes = min(yes_capacity - yes_extra, leftover)
                yes_extra += add_yes
                leftover -= add_yes
                no_extra += min(no_capacity - no_extra, leftover)
            else:
                add_no = min(no_capacity - no_extra, leftover)
                no_extra += add_no
                leftover -= add_no
                yes_extra += min(yes_capacity - yes_extra, leftover)
    elif mode != "passive_bid":
        raise ValueError(f"unknown quote_mode: {mode}")

    yes_px = base_yes + yes_extra
    no_px = base_no + no_extra
    pair_cost = yes_px + no_px
    if pair_cost > cap + 1e-9:
        return None
    return yes_px, no_px, pair_cost


def simulate_pair_base(
    book: dict[str, Any],
    close: int,
    trades: dict[str, Any],
    bin_day: tuple[np.ndarray, np.ndarray] | None,
    yes_wins: bool,
    rule: PairRule,
) -> dict[str, Any]:
    if bin_day is None:
        return empty_pair_result()
    spot_ts, spot_px = bin_day
    yes_long = 0.0
    no_long = 0.0
    yes_cost = 0.0
    no_cost = 0.0
    rebate = 0.0
    fills = 0
    market_live = False
    late_pull_count = 0
    toxic_pull_count = 0
    repair_pull_count = 0
    pair_cost_samples: list[float] = []

    for k in range(len(trades["t"])):
        t_left = float(trades["t"][k])
        wall = float(trades["wall"][k])
        side = str(trades["side"][k]).lower()
        trade_price = float(trades["p"][k])
        trade_size = float(trades["sz"][k])
        if t_left <= rule.stop_before_close_s:
            late_pull_count += 1
            continue

        idx = snap_idx(book, wall)
        bid = float(book["bid"][idx])
        ask = float(book["ask"][idx])
        mid = float(book["mid"][idx])
        bid_sz = max(float(book["bsz"][idx]), 0.0)
        ask_sz = max(float(book["asz"][idx]), 0.0)
        if not (0.0 < bid <= ask < 1.0 and rule.min_mid <= mid <= rule.max_mid):
            continue
        delta_bps = window_delta_bps(spot_ts, spot_px, close, t_left) or 0.0
        quotes = allocate_pair_quotes(bid, ask, rule.max_pair_cost, rule.quote_mode, delta_bps)
        if quotes is None:
            continue
        yes_quote, no_quote, pair_cost_now = quotes
        no_yes_limit = 1.0 - no_quote
        pair_cost_samples.append(pair_cost_now)
        market_live = True

        ret30 = spot_return_bps(spot_ts, spot_px, close + wall, 30.0)
        if abs(ret30) > rule.max_spot_30s_abs_bps:
            toxic_pull_count += 1
            continue
        if trade_size >= rule.large_taker_pull_shares:
            toxic_pull_count += 1
            continue

        delta = yes_long - no_long
        lean_yes = delta_bps >= rule.min_abs_delta_bps_for_lean
        lean_no = delta_bps <= -rule.min_abs_delta_bps_for_lean
        max_delta = rule.repair_delta_shares + (rule.lean_delta_shares if lean_yes else 0.0)
        min_delta = -rule.repair_delta_shares - (rule.lean_delta_shares if lean_no else 0.0)

        bid_live = True
        ask_live = True
        if delta >= max_delta:
            bid_live = False
            repair_pull_count += 1
        if delta <= min_delta:
            ask_live = False
            repair_pull_count += 1

        if bid_live and side == "sell" and trade_price <= yes_quote + 1e-9:
            ahead = bid_sz if abs(yes_quote - bid) <= 1e-9 else 0.0
            frac = rule.clip_shares / (rule.clip_shares + ahead) if rule.clip_shares + ahead > 0 else 0.0
            qty = min(rule.clip_shares, trade_size * frac, max(0.0, max_delta - delta))
            if qty > 1e-9:
                yes_long += qty
                yes_cost += qty * yes_quote
                rebate += qty * yes_quote * REBATE_FRAC
                fills += 1

        delta = yes_long - no_long
        if ask_live and side == "buy" and trade_price >= no_yes_limit - 1e-9:
            ahead = ask_sz if abs(no_yes_limit - ask) <= 1e-9 else 0.0
            frac = rule.clip_shares / (rule.clip_shares + ahead) if rule.clip_shares + ahead > 0 else 0.0
            qty = min(rule.clip_shares, trade_size * frac, max(0.0, delta - min_delta))
            if qty > 1e-9:
                no_long += qty
                no_cost += qty * no_quote
                rebate += qty * no_quote * REBATE_FRAC
                fills += 1

    paired = min(yes_long, no_long)
    yes_avg = yes_cost / yes_long if yes_long > 0 else 0.0
    no_avg = no_cost / no_long if no_long > 0 else 0.0
    pair_cost = yes_avg + no_avg if paired > 0 else 0.0
    pnl_pairs = paired * (1.0 - pair_cost)

    res_yes = yes_long - paired
    res_no = no_long - paired
    pnl_residual = 0.0
    if res_yes > 0:
        pnl_residual += res_yes * ((1.0 if yes_wins else 0.0) - yes_avg)
    if res_no > 0:
        pnl_residual += res_no * ((0.0 if yes_wins else 1.0) - no_avg)
    rebate_realized = rebate if rule.rebate_on else 0.0
    total_leg = yes_long + no_long
    return {
        "pair_live": market_live,
        "pair_fills": fills,
        "yes_long": yes_long,
        "no_long": no_long,
        "paired": paired,
        "residual_shares": res_yes + res_no,
        "residual_frac": (res_yes + res_no) / total_leg if total_leg else 0.0,
        "pair_cost": pair_cost,
        "pair_cost_sample_min": min(pair_cost_samples) if pair_cost_samples else 0.0,
        "pair_cost_sample_mean": float(np.mean(pair_cost_samples)) if pair_cost_samples else 0.0,
        "pnl_pairs": pnl_pairs,
        "pnl_residual": pnl_residual,
        "rebate": rebate_realized,
        "pair_net": pnl_pairs + pnl_residual + rebate_realized,
        "pair_notional": yes_cost + no_cost,
        "late_pull_count": late_pull_count,
        "toxic_pull_count": toxic_pull_count,
        "repair_pull_count": repair_pull_count,
    }


def empty_pair_result() -> dict[str, Any]:
    return {
        "pair_live": False,
        "pair_fills": 0,
        "yes_long": 0.0,
        "no_long": 0.0,
        "paired": 0.0,
        "residual_shares": 0.0,
        "residual_frac": 0.0,
        "pair_cost": 0.0,
        "pair_cost_sample_min": 0.0,
        "pair_cost_sample_mean": 0.0,
        "pnl_pairs": 0.0,
        "pnl_residual": 0.0,
        "rebate": 0.0,
        "pair_net": 0.0,
        "pair_notional": 0.0,
        "late_pull_count": 0,
        "toxic_pull_count": 0,
        "repair_pull_count": 0,
    }


def snapshot_at_tleft(book: dict[str, Any], t_left: float) -> dict[str, float] | None:
    t = book["t"]
    idx = int(np.argmin(np.abs(t - t_left)))
    if abs(float(t[idx]) - t_left) > max(2.0, min(8.0, t_left * 0.20)):
        return None
    bid = float(book["bid"][idx])
    ask = float(book["ask"][idx])
    mid = float(book["mid"][idx])
    if not (0.0 < bid <= ask < 1.0):
        return None
    return {"yes_bid": bid, "yes_ask": ask, "yes_mid": mid, "no_ask": max(0.0, 1.0 - bid)}


def order_pnl(shares: float, price: float, buy_yes: bool, yes_wins: bool) -> float:
    return shares * ((1.0 if buy_yes == yes_wins else 0.0) - price)


def simulate_late_sleeve(
    book: dict[str, Any],
    close: int,
    bin_day: tuple[np.ndarray, np.ndarray] | None,
    yes_wins: bool,
    rule: LateRule,
) -> dict[str, Any]:
    if not rule.enabled or bin_day is None:
        return empty_late_result()
    spot_ts, spot_px = bin_day
    fills = 0
    spend = 0.0
    pnl = 0.0
    prices = []
    for t_left in np.arange(rule.start_t_left, rule.end_t_left - 1e-9, -rule.interval_s):
        if fills >= rule.max_orders:
            break
        snap = snapshot_at_tleft(book, float(t_left))
        if snap is None:
            continue
        delta = window_delta_bps(spot_ts, spot_px, close, float(t_left))
        if delta is None or abs(delta) < rule.min_abs_delta_bps:
            continue
        buy_yes = delta >= 0.0
        if buy_yes != (snap["yes_mid"] >= 0.5):
            continue
        ret30 = spot_return_bps(spot_ts, spot_px, close - float(t_left), 30.0)
        if buy_yes and ret30 < rule.min_spot_30s_bps:
            continue
        if (not buy_yes) and ret30 > -rule.min_spot_30s_bps:
            continue
        price = snap["yes_ask"] if buy_yes else snap["no_ask"]
        if price < rule.min_price or price > rule.max_price:
            continue
        shares = rule.clip_usdc / price
        pnl += order_pnl(shares, price, buy_yes, yes_wins)
        spend += rule.clip_usdc
        fills += 1
        prices.append(price)
    return {
        "late_fills": fills,
        "late_spend": spend,
        "late_pnl": pnl,
        "late_roi": pnl / spend if spend else 0.0,
        "late_avg_price": float(np.mean(prices)) if prices else 0.0,
    }


def empty_late_result() -> dict[str, Any]:
    return {"late_fills": 0, "late_spend": 0.0, "late_pnl": 0.0, "late_roi": 0.0, "late_avg_price": 0.0}


def simulate_market(parsed_market: tuple[Any, int, Any, Any, str], rule: StrategyRule) -> dict[str, Any] | None:
    book, close, trades, bin_day, date = parsed_market
    yes_wins, _, _ = S.binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None
    pair = simulate_pair_base(book, close, trades, bin_day, bool(yes_wins), rule.pair)
    late = simulate_late_sleeve(book, close, bin_day, bool(yes_wins), rule.late)
    if pair["pair_fills"] == 0 and late["late_fills"] == 0:
        return None
    pnl = pair["pair_net"] + late["late_pnl"]
    spend = pair["pair_notional"] + late["late_spend"]
    return {
        "date": date,
        "close": close,
        "asset": "BTC",
        "horizon": "5m",
        "yes_wins": bool(yes_wins),
        "pnl": pnl,
        "spend": spend,
        "roi": pnl / spend if spend else 0.0,
        **pair,
        **late,
    }


def simulate_pair_market(parsed_market: tuple[Any, int, Any, Any, str], rule: PairRule) -> dict[str, Any] | None:
    book, close, trades, bin_day, date = parsed_market
    yes_wins, _, _ = S.binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None
    pair = simulate_pair_base(book, close, trades, bin_day, bool(yes_wins), rule)
    if pair["pair_fills"] == 0:
        return None
    return {
        "date": date,
        "close": close,
        "asset": "BTC",
        "horizon": "5m",
        "yes_wins": bool(yes_wins),
        **pair,
    }


def simulate_late_market(parsed_market: tuple[Any, int, Any, Any, str], rule: LateRule) -> dict[str, Any] | None:
    book, close, _trades, bin_day, date = parsed_market
    yes_wins, _, _ = S.binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None
    late = simulate_late_sleeve(book, close, bin_day, bool(yes_wins), rule)
    if late["late_fills"] == 0:
        return None
    return {
        "date": date,
        "close": close,
        "asset": "BTC",
        "horizon": "5m",
        "yes_wins": bool(yes_wins),
        **late,
    }


def combine_cached_rows(
    pair_rows: dict[int, dict[str, Any]],
    late_rows: dict[int, dict[str, Any]],
) -> list[dict[str, Any]]:
    rows = []
    for close in sorted(set(pair_rows) | set(late_rows)):
        p = pair_rows.get(close)
        l = late_rows.get(close)
        template = p or l
        if template is None:
            continue
        pair = empty_pair_result() if p is None else {k: p[k] for k in empty_pair_result().keys()}
        late = empty_late_result() if l is None else {k: l[k] for k in empty_late_result().keys()}
        pnl = pair["pair_net"] + late["late_pnl"]
        spend = pair["pair_notional"] + late["late_spend"]
        rows.append(
            {
                "date": template["date"],
                "close": close,
                "asset": template["asset"],
                "horizon": template["horizon"],
                "yes_wins": template["yes_wins"],
                "pnl": pnl,
                "spend": spend,
                "roi": pnl / spend if spend else 0.0,
                **pair,
                **late,
            }
        )
    return rows


def summarize(rows: list[dict[str, Any]], rule: StrategyRule, split: str) -> dict[str, Any]:
    out = {
        "label": rule.label,
        "split": split,
        "pair_label": rule.pair.label,
        "late_label": rule.late.label,
        **{f"pair_cfg_{k}": v for k, v in asdict(rule.pair).items()},
        **{f"late_cfg_{k}": v for k, v in asdict(rule.late).items()},
    }
    if not rows:
        out.update({"markets": 0, "fills": 0, "pnl": 0.0, "spend": 0.0, "roi": 0.0})
        return out
    pnl = np.array([r["pnl"] for r in rows], dtype=np.float64)
    ordered = sorted(rows, key=lambda r: r["close"])
    equity = np.cumsum([r["pnl"] for r in ordered])
    peak = np.maximum.accumulate(np.maximum(equity, 0.0))
    dd = peak - equity
    spend = float(sum(r["spend"] for r in rows))
    out.update(
        {
            "markets": len(rows),
            "fills": int(sum(r["pair_fills"] + r["late_fills"] for r in rows)),
            "pair_fills": int(sum(r["pair_fills"] for r in rows)),
            "late_fills": int(sum(r["late_fills"] for r in rows)),
            "pnl": float(pnl.sum()),
            "spend": spend,
            "roi": float(pnl.sum() / spend) if spend else 0.0,
            "hit_rate": float((pnl > 0.0).mean()),
            "worst_market": float(pnl.min()),
            "p10_market": float(np.percentile(pnl, 10)),
            "max_drawdown": float(dd.max()) if len(dd) else 0.0,
            "pnl_pairs": float(sum(r["pnl_pairs"] for r in rows)),
            "pnl_residual": float(sum(r["pnl_residual"] for r in rows)),
            "rebate": float(sum(r["rebate"] for r in rows)),
            "late_pnl": float(sum(r["late_pnl"] for r in rows)),
            "pair_notional": float(sum(r["pair_notional"] for r in rows)),
            "late_spend": float(sum(r["late_spend"] for r in rows)),
            "avg_residual_frac": float(np.mean([r["residual_frac"] for r in rows])),
            "p95_residual_frac": float(np.percentile([r["residual_frac"] for r in rows], 95)),
            "avg_pair_cost": float(np.mean([r["pair_cost"] for r in rows if r["paired"] > 0])) if any(r["paired"] > 0 for r in rows) else 0.0,
            "avg_late_price": float(np.mean([r["late_avg_price"] for r in rows if r["late_fills"] > 0])) if any(r["late_fills"] > 0 for r in rows) else 0.0,
        }
    )
    return out


def evaluate(parsed: list[tuple[Any, int, Any, Any, str]], rule: StrategyRule) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    rows = [r for market in parsed if (r := simulate_market(market, rule)) is not None]
    early = [r for r in rows if r["date"] <= "2026-05-13"]
    late = [r for r in rows if r["date"] >= "2026-05-14"]
    return summarize(rows, rule, "all"), summarize(early, rule, "early"), summarize(late, rule, "late")


def evaluate_from_cached(
    pair_rows: dict[int, dict[str, Any]],
    late_rows: dict[int, dict[str, Any]],
    rule: StrategyRule,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, Any]]:
    rows = combine_cached_rows(pair_rows, late_rows)
    early = [r for r in rows if r["date"] <= "2026-05-13"]
    late = [r for r in rows if r["date"] >= "2026-05-14"]
    return summarize(rows, rule, "all"), summarize(early, rule, "early"), summarize(late, rule, "late")


def candidate_pair_rules() -> list[PairRule]:
    rules = []
    for clip in (5.0, 10.0, 20.0, 40.0):
        for max_pair_cost in (0.970, 0.980, 0.990, 0.995):
            for repair in (2.0, 5.0, 10.0, 20.0):
                for lean in (0.0, 5.0, 15.0):
                    for stop in (30.0, 45.0, 60.0):
                        rules.append(
                            PairRule(
                                clip_shares=clip,
                                max_pair_cost=max_pair_cost,
                                repair_delta_shares=repair,
                                lean_delta_shares=lean,
                                min_abs_delta_bps_for_lean=5.0,
                                stop_before_close_s=stop,
                                max_spot_30s_abs_bps=8.0,
                                large_taker_pull_shares=150.0,
                                min_mid=0.20,
                                max_mid=0.80,
                                rebate_on=False,
                            )
                        )
    # Check the sensitivity to rebate on a smaller core subset.
    for clip in (5.0, 10.0, 20.0):
        for max_pair_cost in (0.970, 0.980, 0.990):
            for repair in (2.0, 5.0, 10.0):
                rules.append(
                    PairRule(
                        clip_shares=clip,
                        max_pair_cost=max_pair_cost,
                        repair_delta_shares=repair,
                        lean_delta_shares=0.0,
                        min_abs_delta_bps_for_lean=5.0,
                        stop_before_close_s=45.0,
                        max_spot_30s_abs_bps=8.0,
                        large_taker_pull_shares=150.0,
                        min_mid=0.20,
                        max_mid=0.80,
                        rebate_on=True,
                    )
                )
    return rules


def candidate_late_rules(*, pair_only: bool = False) -> list[LateRule]:
    off = LateRule(False, 0.0, 0.0, 10.0, 0.0, 0.0, 0.0, 1.0, 0, 0.0)
    if pair_only:
        return [off]
    rules = [off]
    for start in (60.0, 30.0):
        for min_delta in (2.0, 5.0, 10.0):
            for min_price, max_price in ((0.70, 0.92), (0.80, 0.97), (0.90, 0.99)):
                for max_orders in (1, 3):
                    rules.append(
                        LateRule(
                            enabled=True,
                            start_t_left=start,
                            end_t_left=5.0,
                            interval_s=10.0,
                            min_abs_delta_bps=min_delta,
                            min_spot_30s_bps=1.0,
                            min_price=min_price,
                            max_price=max_price,
                            max_orders=max_orders,
                            clip_usdc=10.0,
                        )
                    )
    return rules


def candidate_strategy_rules(pair_limit: int | None = None, *, pair_only: bool = False) -> list[StrategyRule]:
    pairs = candidate_pair_rules()
    if pair_limit is not None:
        pairs = pairs[:pair_limit]
    lates = candidate_late_rules(pair_only=pair_only)
    return [StrategyRule(pair=p, late=l) for p in pairs for l in lates]


def focused_rules(*, pair_only: bool = False) -> list[StrategyRule]:
    pairs = []
    for clip, repair, lean_values in (
        (5.0, 2.0, (0.0, 5.0, 15.0)),
        (10.0, 4.0, (0.0, 10.0, 30.0)),
        (20.0, 8.0, (0.0, 20.0, 60.0)),
        (40.0, 16.0, (0.0, 40.0, 120.0)),
        (80.0, 32.0, (0.0, 80.0, 240.0)),
    ):
        for lean in lean_values:
            for quote_mode in ("passive_bid", "shared_slack_even", "shared_slack_leader"):
                pairs.append(
                    PairRule(
                        clip_shares=clip,
                        max_pair_cost=0.970,
                        repair_delta_shares=repair,
                        lean_delta_shares=lean,
                        min_abs_delta_bps_for_lean=5.0,
                        stop_before_close_s=30.0,
                        max_spot_30s_abs_bps=8.0,
                        large_taker_pull_shares=150.0,
                        min_mid=0.20,
                        max_mid=0.80,
                        rebate_on=False,
                        quote_mode=quote_mode,
                    )
                )
                pairs.append(
                    PairRule(
                        clip_shares=clip,
                        max_pair_cost=0.970,
                        repair_delta_shares=repair,
                        lean_delta_shares=lean,
                        min_abs_delta_bps_for_lean=5.0,
                        stop_before_close_s=45.0,
                        max_spot_30s_abs_bps=8.0,
                        large_taker_pull_shares=150.0,
                        min_mid=0.20,
                        max_mid=0.80,
                        rebate_on=False,
                        quote_mode=quote_mode,
                    )
                )
    lates = candidate_late_rules(pair_only=pair_only)
    return [StrategyRule(pair=p, late=l) for p in pairs for l in lates]


def robustness_score(all_row: dict[str, Any], early_row: dict[str, Any], late_row: dict[str, Any]) -> float:
    if all_row.get("markets", 0) < 30:
        return -1e9
    if early_row.get("markets", 0) < 10 or late_row.get("markets", 0) < 10:
        return -1e8 + all_row.get("pnl", 0.0)
    if early_row.get("pnl", 0.0) <= 0.0 or late_row.get("pnl", 0.0) <= 0.0:
        return -1e6 + all_row.get("pnl", 0.0)
    attribution_penalty = 0.0
    if all_row.get("pnl_pairs", 0.0) <= 0.0 and all_row.get("late_pnl", 0.0) <= 0.0:
        attribution_penalty += 10.0
    return (
        min(early_row["roi"], late_row["roi"]) * math.sqrt(max(all_row["fills"], 1))
        - 0.001 * all_row["max_drawdown"]
        - attribution_penalty
    )


def flatten(all_row: dict[str, Any], early_row: dict[str, Any], late_row: dict[str, Any]) -> dict[str, Any]:
    row = {
        "label": all_row["label"],
        "pair_label": all_row["pair_label"],
        "late_label": all_row["late_label"],
        "robustness_score": robustness_score(all_row, early_row, late_row),
    }
    for prefix, src in (("all", all_row), ("early", early_row), ("late", late_row)):
        for key, value in src.items():
            if key in {"label", "split", "pair_label", "late_label"} or key.startswith("pair_cfg_") or key.startswith("late_cfg_"):
                continue
            row[f"{prefix}_{key}"] = value
    for key, value in all_row.items():
        if key.startswith("pair_cfg_") or key.startswith("late_cfg_"):
            row[key] = value
    return row


def write_csv(path: Path, rows: list[dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if not rows:
        path.write_text("")
        return
    keys = list(rows[0].keys())
    with path.open("w", newline="") as fh:
        writer = csv.DictWriter(fh, fieldnames=keys)
        writer.writeheader()
        writer.writerows(rows)


def write_markdown(path: Path, rows: list[dict[str, Any]], parsed_markets: int, candidate_count: int) -> None:
    lines = [
        "# Competitor-Style Replay",
        "",
        f"Dataset: local BTC 5m cache, parsed markets={parsed_markets}, candidate strategies={candidate_count}.",
        "",
        "This tests passive two-sided pair accumulation plus optional late favourite taker sleeve. NO prices are mirrored from the YES book; this is a BTC 5m screen, not yet the multi-asset production proof.",
        "",
        "## Top Robust Cells",
        "",
        "| Rank | Pair | Late | All PnL | ROI | Early PnL | Late PnL | Pair PnL | Residual | Rebate | Late Sleeve | Fills | Avg Resid | Worst | DD |",
        "|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for idx, row in enumerate(rows[:30], 1):
        lines.append(
            f"| {idx} | `{row['pair_label']}` | `{row['late_label']}` | "
            f"{row['all_pnl']:.2f} | {100*row['all_roi']:.2f}% | "
            f"{row['early_pnl']:.2f} | {row['late_pnl']:.2f} | "
            f"{row['all_pnl_pairs']:.2f} | {row['all_pnl_residual']:.2f} | "
            f"{row['all_rebate']:.2f} | {row['all_late_pnl']:.2f} | "
            f"{row['all_fills']} | {100*row['all_avg_residual_frac']:.2f}% | "
            f"{row['all_worst_market']:.2f} | {row['all_max_drawdown']:.2f} |"
        )
    lines.append("")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--limit", type=int, default=0, help="Stride-sample book files for quick runs.")
    parser.add_argument("--pair-limit", type=int, help="Limit pair-rule count for debugging.")
    parser.add_argument("--pair-only", action="store_true", help="Only evaluate pair-base rules; skip late-sleeve combinations.")
    parser.add_argument("--focused", action="store_true", help="Evaluate a focused set around the best pair-base cells plus late sleeves.")
    parser.add_argument("--out-csv", type=Path, default=Path("data/runs/competitor_style/sweep.csv"))
    parser.add_argument("--out-md", type=Path, default=Path("docs/competitor_style_replay_2026-06-01.md"))
    args = parser.parse_args()

    book_files = sorted(glob.glob(f"{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet"))
    if args.limit:
        step = max(1, len(book_files) // args.limit)
        book_files = book_files[::step][: args.limit]
    parsed = S.load_all_markets(book_files, {})
    rules = focused_rules(pair_only=args.pair_only) if args.focused else candidate_strategy_rules(args.pair_limit, pair_only=args.pair_only)

    rows = []
    if args.focused:
        pair_rules = {rule.pair.label: rule.pair for rule in rules}
        late_rules = {rule.late.label: rule.late for rule in rules}
        pair_cache: dict[str, dict[int, dict[str, Any]]] = {}
        late_cache: dict[str, dict[int, dict[str, Any]]] = {}
        for idx, (label, pair_rule) in enumerate(pair_rules.items(), 1):
            market_rows = {}
            for market in parsed:
                row = simulate_pair_market(market, pair_rule)
                if row is not None:
                    market_rows[row["close"]] = row
            pair_cache[label] = market_rows
            print(f"cached pair {idx}/{len(pair_rules)} {label} markets={len(market_rows)}", flush=True)
        for idx, (label, late_rule) in enumerate(late_rules.items(), 1):
            market_rows = {}
            for market in parsed:
                row = simulate_late_market(market, late_rule)
                if row is not None:
                    market_rows[row["close"]] = row
            late_cache[label] = market_rows
            print(f"cached late {idx}/{len(late_rules)} {label} markets={len(market_rows)}", flush=True)
        for idx, rule in enumerate(rules, 1):
            all_row, early_row, late_row = evaluate_from_cached(
                pair_cache[rule.pair.label],
                late_cache[rule.late.label],
                rule,
            )
            rows.append(flatten(all_row, early_row, late_row))
            if idx % 250 == 0:
                print(f"combined {idx}/{len(rules)}", flush=True)
    else:
        for idx, rule in enumerate(rules, 1):
            all_row, early_row, late_row = evaluate(parsed, rule)
            rows.append(flatten(all_row, early_row, late_row))
            if idx % 250 == 0:
                print(f"evaluated {idx}/{len(rules)}", flush=True)
    rows.sort(key=lambda r: (r["robustness_score"], r["all_pnl"]), reverse=True)

    write_csv(args.out_csv, rows)
    write_markdown(args.out_md, rows, len(parsed), len(rules))
    manifest_path = args.out_csv.with_suffix(".json")
    manifest_path.write_text(
        json.dumps(
            {
                "parsed_markets": len(parsed),
                "candidate_strategies": len(rules),
                "out_csv": str(args.out_csv),
                "out_md": str(args.out_md),
                "top": rows[:10],
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    print(f"wrote {args.out_csv}")
    print(f"wrote {args.out_md}")
    print(f"wrote {manifest_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
