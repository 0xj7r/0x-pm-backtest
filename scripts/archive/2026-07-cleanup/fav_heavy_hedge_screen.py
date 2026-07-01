#!/usr/bin/env python3
"""Screen: when the book is favourite-heavy in chop, does buying the other side help?

Counterfactual on harness trade tapes: for each primary fade leg, simulate an
optional hedge clip on the book-favourite (opposite) side at entry. Uses
side_ask + mid_at_decision to estimate the opposite touch.

Usage:
  python3 scripts/fav_heavy_hedge_screen.py data/runs/strategy_validation/HOLDOUT_baseline.trades.jsonl
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from statistics import median


def load(path: Path) -> list[dict]:
    rows = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    return sorted(rows, key=lambda t: int(t.get("decision_ts_ns") or 0))


def yes_mid(t: dict) -> float:
    return float(t["mid_at_decision"])


def side_is_yes(t: dict) -> bool:
    return t["side"] == "Yes"


def our_ask(t: dict) -> float:
    return float(t.get("side_ask_at_decision") or t["avg_price"])


def our_mid(t: dict) -> float:
    m = yes_mid(t)
    return m if side_is_yes(t) else 1.0 - m


def fav_mid(t: dict) -> float:
    m = yes_mid(t)
    return max(m, 1.0 - m)


def fav_side_yes(t: dict) -> bool:
    return yes_mid(t) >= 0.5


def est_opp_ask(t: dict) -> float:
    """Estimate opposite-side touch ask from our side ask and mids."""
    m = yes_mid(t)
    ask = our_ask(t)
    spread = max(0.0, ask - our_mid(t))
    if side_is_yes(t):
        # favourite is likely NO when we bought YES as dog
        return min(0.99, max(0.01, (1.0 - m) + spread))
    return min(0.99, max(0.01, m + spread))


def est_pair_cost(t: dict) -> float:
    return our_ask(t) + est_opp_ask(t)


def primary_notional(t: dict) -> float:
    return float(t["shares"]) * float(t["avg_price"])


def hedge_pnl(
    t: dict,
    hedge_frac: float,
    *,
    fee_rate: float = 0.07,
) -> tuple[float, float]:
    """Return (combined_pnl, hedge_only_pnl) vs baseline primary pnl."""
    base = float(t["pnl"])
    if hedge_frac <= 0:
        return base, 0.0

    n_primary = primary_notional(t)
    n_hedge = n_primary * hedge_frac
    ask_p = our_ask(t)
    ask_h = est_opp_ask(t)
    if ask_p <= 0 or ask_h <= 0:
        return base, 0.0

    shares_p = n_primary / ask_p
    shares_h = n_hedge / ask_h
    fee_p = float(t.get("fee") or 0)
    fee_h = n_hedge * fee_rate * 0.5  # rough taker curve

    # Hedge is on the book-favourite (opposite touch). One side wins at resolution.
    if t["won"]:
        payout = shares_p
    else:
        payout = shares_h

    combined = payout - n_primary - n_hedge - fee_p - fee_h
    hedge_only = combined - base
    return combined, hedge_only


def is_chop(t: dict) -> bool:
    reg = str(t.get("regime") or t.get("regime_at_decision") or "")
    if "flip" in reg.lower() or "chop" in reg.lower():
        return True
    r30 = t.get("spot_ret_30s_bps")
    if r30 is not None and abs(float(r30)) < 3.0:
        return True
    return False


def summarize(trades: list[dict], label: str) -> dict:
    n = len(trades)
    pnl = sum(float(t["pnl"]) for t in trades)
    hit = sum(1 for t in trades if t["won"]) / n if n else 0.0
    print(
        f"  {label:42s}  n={n:5d}  NET=${pnl:10,.0f}  "
        f"hit={hit * 100:5.1f}%"
    )
    return {"n": n, "net": pnl}


def main() -> int:
    path = Path(sys.argv[1])
    trades = [t for t in load(path) if not t.get("is_completion")]
    print(f"# Favourite-heavy hedge screen — {path.name}\n")

    summarize(trades, "BASELINE (primary fade only)")

    for fav_thr in (0.60, 0.65, 0.70, 0.75):
        heavy = [t for t in trades if fav_mid(t) >= fav_thr]
        if not heavy:
            continue
        print(f"\n## Book favourite mid >= {fav_thr:.2f} (n={len(heavy)})")
        summarize(heavy, "  primary only (fav-heavy entries)")
        pairs = [est_pair_cost(t) for t in heavy]
        print(
            f"  est pair_cost (our+opp ask): med {median(pairs):.3f}  "
            f"p25 {sorted(pairs)[len(pairs)//4]:.3f}  "
            f"<1.00: {sum(1 for p in pairs if p < 1.0)/len(pairs):.0%}"
        )

        buying_dog = sum(1 for t in heavy if our_mid(t) < 0.5)
        print(f"  entries on underdog side (our mid < 0.5): {buying_dog}/{len(heavy)}")

        for frac in (0.25, 0.5, 1.0):
            deltas = []
            combined = 0.0
            for t in heavy:
                c, d = hedge_pnl(t, frac)
                combined += c
                deltas.append(d)
            base_net = sum(float(t["pnl"]) for t in heavy)
            improved = sum(1 for d in deltas if d > 0)
            print(
                f"  + hedge {frac:.0%} fav clip:  NET=${combined:+,.0f}  "
                f"Δ=${combined - base_net:+,.0f} vs primary  "
                f"helps {improved}/{len(heavy)} legs"
            )

        chop = [t for t in heavy if is_chop(t)]
        if chop:
            print(f"\n  chop subset (flip regime or |spot30|<3bps): n={len(chop)}")
            base_c = sum(float(t["pnl"]) for t in chop)
            for frac in (0.5, 1.0):
                comb = sum(hedge_pnl(t, frac)[0] for t in chop)
                print(
                    f"    hedge {frac:.0%}: NET=${comb:+,.0f}  Δ=${comb - base_c:+,.0f}"
                )

    # Loss clusters: fav-heavy losers only
    print("\n## Fav-heavy LOSERS (would hedge have helped?)")
    losers = [t for t in trades if not t["won"] and fav_mid(t) >= 0.65]
    if losers:
        base_l = sum(float(t["pnl"]) for t in losers)
        print(f"  n={len(losers)}  primary NET=${base_l:+,.0f}")
        for frac in (0.5, 1.0):
            comb = sum(hedge_pnl(t, frac)[0] for t in losers)
            # on losers, hedge wins when favourite wins
            fav_won_hedge = sum(
                1
                for t in losers
                if (fav_side_yes(t) and not side_is_yes(t) and not t["won"])
                or (not fav_side_yes(t) and side_is_yes(t) and not t["won"])
            )
            print(
                f"  hedge {frac:.0%}: NET=${comb:+,.0f}  Δ=${comb - base_l:+,.0f}  "
                f"(fav won {fav_won_hedge}/{len(losers)} → hedge pays)"
            )

    # Pair-complete instead of fade? (buy both when pair_cost < 1)
    print("\n## Pair-arb available at entry? (est pair_cost < 1.00)")
    arb = [t for t in trades if est_pair_cost(t) < 1.0]
    print(f"  legs with est pair_cost < 1: {len(arb)}/{len(trades)} ({100*len(arb)/len(trades):.1f}%)")
    if arb:
        summarize(arb, "  those primary legs")

    print("\n## Takeaway")
    print(
        "- Hedge buys the book favourite (expensive leg). It pays off when the "
        "fade loses AND the favourite wins — caps tail loss at (primary + hedge cost - hedge payout)."
    )
    print(
        "- When pair_cost > 1 (typical), full pairing is negative EV; partial hedge "
        "is insurance with premium ≈ hedge_cost - E[hedge payout]."
    )
    print(
        "- In chop + fav-heavy, check if Δ on losers exceeds drag on winners "
        "before wiring live."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())