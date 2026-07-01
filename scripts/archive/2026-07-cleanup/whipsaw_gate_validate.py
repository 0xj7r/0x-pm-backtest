#!/usr/bin/env python3
"""Counterfactual whipsaw / chop gate validation on harness trade tapes.

Replays TradeRecord JSONL with decision-layer gates (no belief change).
Stateful gates (loss streaks, opposite-side whipsaw) need chronological order.

Usage:
  python3 scripts/whipsaw_gate_validate.py data/runs/open-entry/baseline.trades.jsonl
  python3 scripts/whipsaw_gate_validate.py data/runs/p-improvement/perp_w90.trades.jsonl
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Callable


def load(path: Path) -> list[dict]:
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def p_side(t: dict) -> float:
    p = float(t["p_exo"])
    return p if t["side"] == "Yes" else 1.0 - p


def side_label(t: dict) -> str:
    return "up" if t["side"] == "Yes" else "down"


def side_aligned_30s(t: dict) -> bool | None:
    if "side_aligned_30s" in t and t["side_aligned_30s"] is not None:
        return bool(t["side_aligned_30s"])
    r = t.get("spot_ret_30s_bps")
    if r is None:
        return None
    r = float(r)
    return r > 0 if t["side"] == "Yes" else r < 0


def secs_from_open(t: dict) -> float | None:
    open_ns = t.get("open_ts_ns")
    ts = t.get("decision_ts_ns")
    if not open_ns or not ts:
        return None
    return (ts - open_ns) / 1e9


def model_book_gap(t: dict) -> float:
    ask = float(t.get("side_ask_at_decision") or t.get("avg_price") or 0)
    return p_side(t) - ask


def summarize(trades: list[dict], label: str) -> dict:
    n = len(trades)
    pnl = sum(float(t["pnl"]) for t in trades)
    hit = sum(1 for t in trades if float(t["pnl"]) > 0) / n if n else 0.0
    print(
        f"{label:42s}  n={n:5d}  NET=${pnl:10,.0f}  "
        f"hit={hit * 100:5.1f}%  $/tr={pnl / n if n else 0:6.2f}"
    )
    return {"n": n, "net": pnl, "hit": hit}


@dataclass
class SessionState:
    last_ts_ns: int = 0
    last_side: str | None = None
    last_won: bool | None = None
    consec_losses: int = 0

    def observe(self, t: dict) -> None:
        self.last_ts_ns = int(t.get("decision_ts_ns") or 0)
        self.last_side = side_label(t)
        won = float(t["pnl"]) > 0
        self.last_won = won
        self.consec_losses = 0 if won else self.consec_losses + 1


def filter_chronological(
    trades: list[dict],
    keep_fn: Callable[[dict, SessionState], bool],
) -> list[dict]:
    """Gate on session state but advance state on every baseline trade.

    State (loss streaks, last side) reflects what would have happened had we
    kept trading; only PnL from kept trades counts toward NET.
    """
    ordered = sorted(trades, key=lambda t: int(t.get("decision_ts_ns") or 0))
    kept: list[dict] = []
    state = SessionState()
    for t in ordered:
        if keep_fn(t, state):
            kept.append(t)
        state.observe(t)
    return kept


def gate_regime_flip(t: dict, _s: SessionState) -> bool:
    return t.get("regime") != "expanded_high_flip"


def gate_model_book_gap_085_065(t: dict, _s: SessionState) -> bool:
    ask = float(t.get("side_ask_at_decision") or t.get("avg_price") or 0)
    ps = p_side(t)
    if ps > 0.85 and ask < 0.65:
        return False
    return True


def gate_open_fav_gap(t: dict, _s: SessionState) -> bool:
    """Skip :00 favourites where model >> book (today's UP p~1 touch~0.56)."""
    s = secs_from_open(t)
    if s is None or s > 5:
        return True
    ask = float(t.get("side_ask_at_decision") or t.get("avg_price") or 0)
    if p_side(t) > 0.90 and ask < 0.60:
        return False
    return True


def gate_after_2_losses(t: dict, s: SessionState) -> bool:
    return s.consec_losses < 2


def gate_after_3_losses(t: dict, s: SessionState) -> bool:
    return s.consec_losses < 3


def gate_opposite_side_after_loss(t: dict, s: SessionState) -> bool:
    """Skip if prior trade lost on the other side within 30m (whipsaw flip)."""
    if s.last_side is None or s.last_won is not False:
        return True
    cur = side_label(t)
    if cur == s.last_side:
        return True
    ts = int(t.get("decision_ts_ns") or 0)
    if ts - s.last_ts_ns > 30 * 60 * 1_000_000_000:
        return True
    return False


def gate_same_side_after_loss(t: dict, s: SessionState) -> bool:
    if s.last_side is None or s.last_won is not False:
        return True
    if side_label(t) != s.last_side:
        return True
    ts = int(t.get("decision_ts_ns") or 0)
    if ts - s.last_ts_ns > 20 * 60 * 1_000_000_000:
        return True
    return False


def gate_flip_misalign_after_win(t: dict, s: SessionState) -> bool:
    """Skip opp-side entry after a win when 30s spot disagrees with side."""
    if s.last_side is None or s.last_won is not True:
        return True
    if side_label(t) == s.last_side:
        return True
    aligned = side_aligned_30s(t)
    if aligned is False:
        return False
    return True


def gate_loss2_or_flip_misalign(t: dict, s: SessionState) -> bool:
    return gate_after_2_losses(t, s) and gate_flip_misalign_after_win(t, s)


def combine(*fns: Callable[[dict, SessionState], bool]) -> Callable[[dict, SessionState], bool]:
    def merged(t: dict, s: SessionState) -> bool:
        return all(fn(t, s) for fn in fns)

    return merged


def pnl_by_regime(trades: list[dict]) -> None:
    print("\n## Baseline by regime (open-time)")
    by: dict[str, list[dict]] = defaultdict(list)
    for t in trades:
        by[str(t.get("regime") or "unknown")].append(t)
    for k in sorted(by):
        summarize(by[k], k)


def pnl_by_gap_bucket(trades: list[dict]) -> None:
    print("\n## Baseline by model-book gap (p_side - ask)")
    buckets = [
        ("gap<0.10", lambda t: model_book_gap(t) < 0.10),
        ("0.10-0.25", lambda t: 0.10 <= model_book_gap(t) < 0.25),
        ("0.25-0.40", lambda t: 0.25 <= model_book_gap(t) < 0.40),
        ("gap>=0.40", lambda t: model_book_gap(t) >= 0.40),
    ]
    for name, pred in buckets:
        sub = [t for t in trades if pred(t)]
        if sub:
            summarize(sub, name)


def main() -> None:
    path = Path(sys.argv[1])
    trades = load(path)
    print(f"# Whipsaw gate validation — {path}\n")
    summarize(trades, "BASELINE (all trades)")
    pnl_by_regime(trades)
    pnl_by_gap_bucket(trades)

    print("\n## Counterfactual gates (chronological / stateful where noted)")
    gates: list[tuple[str, Callable[[dict, SessionState], bool], bool]] = [
        ("no expanded_high_flip", gate_regime_flip, True),
        ("p_side>0.85 & ask<0.65 skip", gate_model_book_gap_085_065, False),
        (":00 p>0.90 & ask<0.60 skip", gate_open_fav_gap, False),
        ("pause after 2 consec losses", gate_after_2_losses, True),
        ("skip flip+misalign after win", gate_flip_misalign_after_win, True),
        ("loss2 OR skip flip+misalign", gate_loss2_or_flip_misalign, True),
        ("pause after 3 consec losses", gate_after_3_losses, True),
        ("skip opp-side after loss (30m)", gate_opposite_side_after_loss, True),
        ("skip same-side after loss (20m)", gate_same_side_after_loss, True),
        (
            "combo: flip regime + open fav gap",
            combine(gate_regime_flip, gate_open_fav_gap),
            True,
        ),
        (
            "combo: flip + opp-side-after-loss",
            combine(gate_regime_flip, gate_opposite_side_after_loss),
            True,
        ),
        (
            "combo: open fav + 2-loss pause",
            combine(gate_open_fav_gap, gate_after_2_losses),
            True,
        ),
        (
            "combo: all whipsaw heuristics",
            combine(
                gate_regime_flip,
                gate_open_fav_gap,
                gate_opposite_side_after_loss,
                gate_after_2_losses,
            ),
            True,
        ),
    ]

    print(f"{'gate':42s}  {'n':>5s}  {'NET':>11s}  {'hit%':>6s}  {'$/tr':>6s}  {'Δ$':>9s}")
    base_net = sum(float(t["pnl"]) for t in trades)
    for name, pred, stateful in gates:
        if stateful:
            sub = filter_chronological(trades, pred)
        else:
            sub = [t for t in trades if pred(t, SessionState())]
        n = len(sub)
        pnl = sum(float(t["pnl"]) for t in sub)
        hit = sum(1 for t in sub if float(t["pnl"]) > 0) / n * 100 if n else 0
        per = pnl / n if n else 0
        delta = pnl - base_net
        print(
            f"{name:42s}  {n:5d}  ${pnl:10,.0f}  {hit:5.1f}%  {per:6.2f}  {delta:+9,.0f}"
        )

    print("\n## Notes")
    print("- Stateful gates process trades in decision_ts order (session simulation).")
    print("- spot_ret_60s not on TradeRecord yet; use open-entry / harness extension for v2.")
    print("- Promote gates only if VERIFY NET improves AND worst-day does not blow up.")


if __name__ == "__main__":
    main()