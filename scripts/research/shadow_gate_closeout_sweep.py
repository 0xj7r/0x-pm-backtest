#!/usr/bin/env python3
"""Full prod gate closeout sweep: regime + spot-misalign horizon + min_entry_ask.

Replays shadow-final `would_enter` rows against `resolution` ladder PnL.
Uses logged `regime` (decision-time classifier) — same field `decide_entry` gates on.

Usage:
  python3 scripts/research/shadow_gate_closeout_sweep.py \\
    --shadow-dir ~/data/pm-alpha/shadow-final \\
    --since 2026-06-14
"""

from __future__ import annotations

import argparse
import itertools
import json
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

from shadow_analysis_lib import load_jsonl_events, pair_entries, parse_ts

SPOT_RET_COL = {
    30: "spot_ret_30s_bps",
    60: "spot_ret_60s_bps",
    120: "spot_ret_120s_bps",
}

PROD_MIN_ASK = 0.45
REGIME_FLAGS = ("skip_calm", "skip_expanded_mixed", "skip_expanded_high_flip")
MOM_HORIZONS = (0, 30, 60, 120)


@dataclass(frozen=True)
class GatePackage:
    skip_calm: bool = False
    skip_expanded_mixed: bool = False
    skip_expanded_high_flip: bool = False
    skip_spot_misalign_s: int = 0
    min_entry_ask: float = PROD_MIN_ASK

    def label(self) -> str:
        parts: list[str] = []
        if self.skip_calm:
            parts.append("skip_calm")
        if self.skip_expanded_mixed:
            parts.append("skip_exp_mixed")
        if self.skip_expanded_high_flip:
            parts.append("skip_exp_flip")
        if self.skip_spot_misalign_s:
            parts.append(f"mom{self.skip_spot_misalign_s}")
        if self.min_entry_ask > 0:
            parts.append(f"min_ask={self.min_entry_ask:.2f}")
        return "+".join(parts) if parts else "baseline"

    def is_prod_current(self) -> bool:
        return (
            self.skip_calm
            and self.skip_expanded_mixed
            and self.skip_expanded_high_flip
            and self.skip_spot_misalign_s == 120
            and abs(self.min_entry_ask - PROD_MIN_ASK) < 1e-9
        )


def spot_agrees(side: str, ret_bps: float | None) -> bool:
    if ret_bps is None:
        return True
    if side == "up":
        return ret_bps > 0.0
    return ret_bps < 0.0


def regime_allows(regime: str | None, pkg: GatePackage) -> bool:
    if regime is None:
        return True
    if pkg.skip_calm and regime == "calm_low_vol":
        return False
    if pkg.skip_expanded_mixed and regime == "expanded_mixed":
        return False
    if pkg.skip_expanded_high_flip and regime == "expanded_high_flip":
        return False
    return True


def entry_passes(row: dict, pkg: GatePackage) -> bool:
    touch = float(row.get("touch_price") or 0)
    if pkg.min_entry_ask > 0 and touch < pkg.min_entry_ask:
        return False
    regime = row.get("regime_logged") or row.get("regime")
    if not regime_allows(regime, pkg):
        return False
    if pkg.skip_spot_misalign_s > 0:
        col = SPOT_RET_COL.get(pkg.skip_spot_misalign_s)
        if col:
            ret = row.get(col)
            if ret is not None and not spot_agrees(row["side"], float(ret)):
                return False
    return True


def score_package(rows: list[dict], pkg: GatePackage) -> dict:
    gross = 0.0
    wins = losses = 0
    by_day: dict[str, float] = defaultdict(float)
    by_period: dict[str, float] = defaultdict(float)
    blocked = 0

    for r in rows:
        if not entry_passes(r, pkg):
            blocked += 1
            continue
        pnl = float(r["pnl_usd"])
        gross += pnl
        if r["won"]:
            wins += 1
        else:
            losses += 1
        day = r["utc_day"]
        by_day[day] += pnl
        period = "good" if day <= "2026-06-16" else ("drawdown" if day <= "2026-06-19" else "post")
        by_period[period] += pnl

    n = wins + losses
    return {
        "gross": gross,
        "wins": wins,
        "losses": losses,
        "trades": n,
        "hit": (wins / n * 100) if n else 0.0,
        "blocked": blocked,
        "by_day": dict(by_day),
        "by_period": dict(by_period),
    }


def packages() -> list[GatePackage]:
    """Prod-fixed min_ask; sweep regime subsets × mom horizon."""
    out: list[GatePackage] = []
    for r in range(len(REGIME_FLAGS) + 1):
        for regime_bits in itertools.combinations(REGIME_FLAGS, r):
            kw = {f: True for f in regime_bits}
            for mom in MOM_HORIZONS:
                out.append(
                    GatePackage(
                        skip_spot_misalign_s=mom,
                        min_entry_ask=PROD_MIN_ASK,
                        **kw,
                    )
                )
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--since", default="2026-06-14")
    ap.add_argument("--top", type=int, default=20)
    ap.add_argument("--out-json", default=None, help="write full results JSON")
    args = ap.parse_args()

    cutoff = datetime.strptime(args.since, "%Y-%m-%d").replace(tzinfo=timezone.utc)
    shadow_dir = Path(args.shadow_dir)

    entries, resolutions = load_jsonl_events(shadow_dir)
    entries = [e for e in entries if parse_ts(e["ts_utc"]) >= cutoff]
    rows = pair_entries(entries, resolutions)
    labeled = sum(1 for r in rows if r.get("regime_logged"))
    logged_rows = [r for r in rows if r.get("regime_logged")]

    print(f"# Gate closeout sweep — {shadow_dir}")
    print(f"since {args.since}")
    print(f"paired: {len(rows)}  regime-labeled: {labeled}")
    print()

    baseline = GatePackage(min_entry_ask=0.0)
    base_all = score_package(rows, baseline)
    base_logged = score_package(logged_rows, baseline)
    print(f"baseline (no gates): all=${base_all['gross']:,.0f}  logged=${base_logged['gross']:,.0f}")
    print()

    results: list[tuple[GatePackage, dict, dict]] = []
    for pkg in packages():
        s_all = score_package(rows, pkg)
        s_log = score_package(logged_rows, pkg)
        results.append((pkg, s_all, s_log))

    # Rank: logged-regime PnL primary, drawdown period secondary, trade count tertiary
    def rank_key(item: tuple) -> tuple:
        pkg, s_all, s_log = item
        dd = s_all["by_period"].get("drawdown", 0.0)
        return (s_log["gross"], dd, s_all["gross"], -s_all["blocked"])

    results.sort(key=rank_key, reverse=True)

    prod = next((p for p in packages() if GatePackage(
        skip_calm=True, skip_expanded_mixed=True, skip_spot_misalign_s=30, min_entry_ask=PROD_MIN_ASK
    ).label() == p.label()), None)
    prod_score = None
    for pkg, s_all, s_log in results:
        if pkg.is_prod_current():
            prod_score = (pkg, s_all, s_log)
            break

    print(f"{'combo':<52} {'all_pnl':>9} {'log_pnl':>9} {'dd_pnl':>9} {'trades':>7} {'hit':>6}")
    for pkg, s_all, s_log in results[: args.top]:
        dd = s_all["by_period"].get("drawdown", 0.0)
        mark = " *PROD*" if pkg.is_prod_current() else ""
        print(
            f"{pkg.label():<52} ${s_all['gross']:>7,.0f} ${s_log['gross']:>7,.0f} "
            f"${dd:>7,.0f} {s_all['trades']:>7} {s_all['hit']:>5.1f}%{mark}"
        )

    winner_pkg, winner_all, winner_log = results[0]
    print()
    print("## Winner (logged-regime primary)")
    print(f"  {winner_pkg.label()}")
    print(f"  all: ${winner_all['gross']:,.0f} ({winner_all['trades']} trades, {winner_all['hit']:.1f}% hit)")
    print(f"  logged: ${winner_log['gross']:,.0f}")
    print(f"  drawdown window: ${winner_all['by_period'].get('drawdown', 0):,.0f}")
    if prod_score:
        _, prod_all, prod_log = prod_score
        print()
        print("## Current prod")
        print(f"  delta all: ${winner_all['gross'] - prod_all['gross']:+,.0f}")
        print(f"  delta logged: ${winner_log['gross'] - prod_log['gross']:+,.0f}")
        print(f"  delta drawdown: ${winner_all['by_period'].get('drawdown', 0) - prod_all['by_period'].get('drawdown', 0):+,.0f}")

    if args.out_json:
        out_path = Path(args.out_json)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "since": args.since,
            "n_rows": len(rows),
            "n_labeled": labeled,
            "winner": {
                "label": winner_pkg.label(),
                "skip_calm": winner_pkg.skip_calm,
                "skip_expanded_mixed": winner_pkg.skip_expanded_mixed,
                "skip_expanded_high_flip": winner_pkg.skip_expanded_high_flip,
                "skip_spot_misalign_s": winner_pkg.skip_spot_misalign_s,
                "min_entry_ask": winner_pkg.min_entry_ask,
                "all": winner_all,
                "logged": winner_log,
            },
            "top": [
                {
                    "label": p.label(),
                    "skip_calm": p.skip_calm,
                    "skip_expanded_mixed": p.skip_expanded_mixed,
                    "skip_expanded_high_flip": p.skip_expanded_high_flip,
                    "skip_spot_misalign_s": p.skip_spot_misalign_s,
                    "all_pnl": s_all["gross"],
                    "logged_pnl": s_log["gross"],
                    "drawdown_pnl": s_all["by_period"].get("drawdown", 0),
                    "trades": s_all["trades"],
                }
                for p, s_all, s_log in results[: args.top]
            ],
        }
        out_path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        print(f"\nwrote {out_path}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())