#!/usr/bin/env python3
"""Score p-improvement sweep variants vs shadow_baseline on VERIFY.

Reads *_summary.json under data/runs/p-improvement/ (or OUT dir arg).
Baseline: data/runs/analysis/shadow_baseline_summary.json, else perp_w75 in OUT.

Promotion rules (PASS / FAIL / INVESTIGATE):
  PASS        — NET >= baseline + 2%, hit within 2pp, worst-day not >15% worse
  FAIL        — NET < baseline - 5% OR (negative worst-day delta AND NET below base)
  INVESTIGATE — marginal / mixed / stress harness / fee-proxy / tail sleeves
"""
from __future__ import annotations

import json
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_OUT = ROOT / "data/runs/p-improvement"
BASELINE_PATH = ROOT / "data/runs/analysis/shadow_baseline_summary.json"

STRESS = frozenset({"stress_depth25", "stress_skip_touch", "stress_both"})
FEE_PROXY = frozenset({"edge_014", "edge_016"})
TAIL = frozenset({"tail_010", "tail_010_standalone"})


def load_summary(path: Path) -> dict:
    return json.loads(path.read_text())


def worst_day_from_trades(trades_path: Path) -> tuple[float, str]:
    by_day: dict[str, float] = defaultdict(float)
    for line in trades_path.read_text().splitlines():
        if not line.strip():
            continue
        t = json.loads(line)
        ts = t.get("decision_ts_ns") or t.get("entry_ts_ns") or 0
        if not ts:
            continue
        day = datetime.fromtimestamp(ts / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")
        by_day[day] += float(t.get("pnl_net", t.get("pnl", 0)))
    if not by_day:
        return 0.0, ""
    worst_day, worst_pnl = min(by_day.items(), key=lambda kv: kv[1])
    return worst_pnl, worst_day


def enrich(row: dict, out_dir: Path) -> dict:
    variant = row["variant"]
    trades = out_dir / f"{variant}.trades.jsonl"
    worst, worst_date = worst_day_from_trades(trades) if trades.exists() else (0.0, "")
    row = dict(row)
    row["worst_day"] = round(worst, 2)
    row["worst_day_date"] = worst_date
    return row


def load_baseline(out_dir: Path) -> dict:
    if BASELINE_PATH.is_file():
        base = enrich(load_summary(BASELINE_PATH), ROOT / "data/runs/analysis")
        base["variant"] = "shadow_baseline"
        return base
    fallback = out_dir / "perp_w75_summary.json"
    if fallback.is_file():
        return enrich(load_summary(fallback), out_dir)
    return {
        "variant": "shadow_baseline",
        "NET": 26025.73,
        "trades": 4952,
        "hit": 0.5681,
        "worst_day": 0.0,
        "worst_day_date": "",
    }


def promote(variant: str, row: dict, base: dict) -> str:
    if variant in ("shadow_baseline", "perp_w75"):
        return "BASELINE"

    net = float(row.get("NET", 0))
    base_net = float(base.get("NET", 0))
    hit = float(row.get("hit", 0))
    base_hit = float(base.get("hit", 0))
    worst = float(row.get("worst_day", 0))
    base_worst = float(base.get("worst_day", 0))

    net_delta_pct = (net - base_net) / abs(base_net) if base_net else 0.0
    hit_delta_pp = (hit - base_hit) * 100.0
    worst_delta = worst - base_worst

    if variant in STRESS:
        if net >= 0.80 * base_net:
            return "INVESTIGATE"
        if net < 0.50 * base_net:
            return "FAIL"
        return "INVESTIGATE"

    if variant in FEE_PROXY | TAIL:
        if net >= base_net * 1.02 and hit_delta_pp >= -2.0:
            return "INVESTIGATE"
        if net < base_net * 0.90:
            return "FAIL"
        return "INVESTIGATE"

    # Model / belief improvements
    if net >= base_net * 1.02 and hit_delta_pp >= -2.0:
        if base_worst < 0 and worst_delta < base_worst * 0.15:
            return "INVESTIGATE"
        if base_worst >= 0 and worst_delta < -500:
            return "INVESTIGATE"
        return "PASS"

    if net < base_net * 0.95 and (worst_delta < 0 or net_delta_pct < -0.10):
        return "FAIL"

    if net < base_net * 0.98 and hit_delta_pp < -3.0:
        return "FAIL"

    return "INVESTIGATE"


def family(variant: str) -> str:
    if variant.startswith("p_cal"):
        return "A_calibrator"
    if variant.startswith("perp_w"):
        return "B_perp_weight"
    if variant.startswith("vol_") or variant == "sigma_floor_only":
        return "C_vol"
    if variant.startswith("edge_"):
        return "D_fee_proxy"
    if variant.startswith("stress_"):
        return "E_stress"
    if variant.startswith("tail_"):
        return "F_tail"
    if variant == "strikes_official":
        return "G_strikes"
    return "other"


def main() -> None:
    out_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_OUT
    if not out_dir.is_dir():
        print(f"OUT dir missing: {out_dir}", file=sys.stderr)
        sys.exit(1)

    base = load_baseline(out_dir)
    rows: list[dict] = []
    for path in sorted(out_dir.glob("*_summary.json")):
        try:
            rows.append(enrich(load_summary(path), out_dir))
        except Exception as exc:
            print(f"skip {path.name}: {exc}", file=sys.stderr)

    if not rows:
        print(f"No summaries in {out_dir}")
        sys.exit(0)

    rows.sort(key=lambda r: (-float(r.get("NET", 0)), r.get("variant", "")))

    base_net = float(base.get("NET", 0))
    print(f"# P-improvement promotion — {out_dir}")
    print(f"# Baseline: {base.get('variant')} NET=${base_net:,.2f} "
          f"trades={base.get('trades')} hit={float(base.get('hit', 0))*100:.1f}% "
          f"worst_day=${float(base.get('worst_day', 0)):,.2f}")
    print()
    hdr = (
        f"{'family':<14} {'variant':<22} {'verdict':<12} "
        f"{'NET':>12} {'ΔNET%':>8} {'trades':>7} {'hit%':>6} "
        f"{'worst_day':>11} {'Δworst':>9}"
    )
    print(hdr)
    print("-" * len(hdr))

    counts = defaultdict(int)
    for r in rows:
        variant = r.get("variant", "?")
        verdict = promote(variant, r, base)
        counts[verdict] += 1
        net = float(r.get("NET", 0))
        delta_pct = (net - base_net) / abs(base_net) * 100 if base_net else 0.0
        worst = float(r.get("worst_day", 0))
        base_worst = float(base.get("worst_day", 0))
        print(
            f"{family(variant):<14} {variant:<22} {verdict:<12} "
            f"${net:>10,.2f} {delta_pct:>+7.1f}% {int(r.get('trades', 0)):>7} "
            f"{float(r.get('hit', 0))*100:>5.1f}% "
            f"${worst:>9,.2f} {worst - base_worst:>+8,.2f}"
        )

    print()
    print(
        f"PASS={counts['PASS']}  FAIL={counts['FAIL']}  "
        f"INVESTIGATE={counts['INVESTIGATE']}  BASELINE={counts['BASELINE']}"
    )


if __name__ == "__main__":
    main()