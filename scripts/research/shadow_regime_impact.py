#!/usr/bin/env python3
"""Robust regime × market-condition impact analysis for shadow-final fade.

Reconstructs entry-time regime from Binance 1m klines (pm_alpha v1 thresholds),
joins daily spot regime, and reports P&L with bootstrap confidence intervals,
cross-tabs, and gate counterfactuals.

Usage:
  python3 scripts/research/shadow_regime_impact.py \\
    --shadow-dir ~/data/pm-alpha/shadow-final \\
    --since 2026-06-14 \\
    --out ~/data/runs/shadow_regime_impact/report.md
"""
from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from shadow_analysis_lib import (
    SPOT_RET_COLS,
    bootstrap_ci,
    bucket_from_rows,
    cross_tab,
    enrich_rows,
    filter_period,
    fmt_money,
    gate_counterfactual,
    group_by,
    load_jsonl_events,
    pair_entries,
)


def write_bucket_table(
    lines: list[str],
    title: str,
    groups: dict,
    baseline_mean: float,
    min_n: int = 5,
) -> None:
    lines.append(f"### {title}")
    lines.append("")
    lines.append("| Bucket | n | Hit | Mean P&L | Total P&L | 95% CI mean | vs baseline |")
    lines.append("|--------|---|-----|----------|-----------|-------------|-------------|")
    for key in sorted(groups, key=lambda k: groups[k].pnl):
        g = groups[key]
        if g.n < min_n:
            continue
        mean, lo, hi = bootstrap_ci(g.pnls or [])
        delta = mean - baseline_mean
        lines.append(
            f"| {key} | {g.n} | {g.hit:.0%} | {fmt_money(mean)} | {fmt_money(g.pnl)} | "
            f"[{fmt_money(lo)}, {fmt_money(hi)}] | {fmt_money(delta)} |"
        )
    lines.append("")


def write_crosstab(
    lines: list[str],
    title: str,
    tab: dict,
    min_n: int = 8,
) -> None:
    lines.append(f"### {title}")
    lines.append("")
    lines.append("| A | B | n | Hit | Total P&L | Mean P&L |")
    lines.append("|---|---|---|-----|-----------|----------|")
    for (a, b), g in sorted(tab.items(), key=lambda x: x[1].pnl):
        if g.n < min_n:
            continue
        lines.append(
            f"| {a} | {b} | {g.n} | {g.hit:.0%} | {fmt_money(g.pnl)} | {fmt_money(g.mean_pnl)} |"
        )
    lines.append("")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--day-regime", default=None)
    ap.add_argument("--since", default="2026-06-14")
    ap.add_argument("--until", default=None)
    ap.add_argument("--out", default="data/runs/shadow_regime_impact/report.md")
    ap.add_argument("--json", default=None, help="optional machine-readable summary")
    args = ap.parse_args()

    shadow_dir = Path(args.shadow_dir)
    print("Loading shadow JSONL...", file=sys.stderr)
    entries, resolutions = load_jsonl_events(shadow_dir)
    rows = pair_entries(entries, resolutions)
    rows = filter_period(rows, args.since, args.until)
    print(f"Enriching {len(rows)} paired entries (klines + regime)...", file=sys.stderr)
    enrich_rows(rows, Path(args.day_regime) if args.day_regime else None)

    base = bucket_from_rows(rows)
    base_mean, _, _ = bootstrap_ci([r["pnl_usd"] for r in rows])

    lines: list[str] = []
    w = lines.append
    w("# Shadow regime impact analysis (robust)")
    w("")
    w(f"Shadow dir: `{shadow_dir}` | period: {args.since} → {args.until or 'latest'}")
    w(f"Trades: **{base.n}** | Hit: **{base.hit:.1%}** | Net P&L: **{fmt_money(base.pnl)}**")
    w(f"Bootstrap mean P&L/trade: **{fmt_money(base_mean)}** (95% CI via 2000 resamples)")
    w("")

    # Logged vs reconstructed regime agreement
    both = [r for r in rows if r.get("regime_logged") and r.get("regime_reconstructed") != "unknown"]
    agree = sum(1 for r in both if r["regime_logged"] == r["regime_reconstructed"])
    w("## Regime telemetry quality")
    w("")
    w(f"- Entries with logged `regime`: {sum(1 for r in rows if r.get('regime_logged'))}/{len(rows)}")
    w(f"- Reconstructed from Binance 1m @ entry: {sum(1 for r in rows if r.get('regime_reconstructed') != 'unknown')}/{len(rows)}")
    if both:
        w(f"- Logged vs reconstructed agreement: {agree}/{len(both)} ({agree/len(both):.0%})")
    w("")
    w("**Caution:** 1m-klines reconstruction ≠ live tick `pm_alpha::regime` (~50% agreement).")
    w("Section 1b uses **logged** regime where available (authoritative for gate decisions).")
    w("")

    logged_rows = [r for r in rows if r.get("regime_logged")]
    if logged_rows:
        w("## 1b. Logged entry regime (live classifier, authoritative)")
        w("")
        lm, _, _ = bootstrap_ci([r["pnl_usd"] for r in logged_rows])
        w(f"n={len(logged_rows)} | P&L={fmt_money(sum(r['pnl_usd'] for r in logged_rows))} | mean={fmt_money(lm)}")
        w("")
        write_bucket_table(lines, "Logged regime (all)", group_by(logged_rows, "regime_logged"), lm, min_n=3)
        draw_log = [r for r in logged_rows if r["period"] == "drawdown"]
        if draw_log:
            dm, _, _ = bootstrap_ci([r["pnl_usd"] for r in draw_log])
            w(f"#### Logged regime: drawdown only (n={len(draw_log)})")
            w("")
            write_bucket_table(lines, "drawdown logged", group_by(draw_log, "regime_logged"), dm, min_n=3)

    w("## 1. Entry-time regime → fade P&L (reconstructed, full n)")
    w("")
    w("Regime = pm_alpha v1 (`calm_low_vol`, `clean_directional`, `expanded_high_flip`, `expanded_mixed`).")
    w("Reconstructed from 30m Binance 1m path @ each entry timestamp.")
    w("")
    by_regime = group_by(rows, "regime_reconstructed")
    write_bucket_table(lines, "All periods", by_regime, base_mean)

    for period in ("good_tape", "drawdown"):
        sub = [r for r in rows if r["period"] == period]
        if not sub:
            continue
        pm, _, _ = bootstrap_ci([r["pnl_usd"] for r in sub])
        w(f"#### Period: {period} (n={len(sub)})")
        w("")
        write_bucket_table(lines, f"{period} by entry regime", group_by(sub, "regime_reconstructed"), pm, min_n=3)

    w("## 2. Daily spot regime → fade P&L")
    w("")
    w("Full UTC-day BTC character from 1m klines (`day_regime`).")
    w("")
    write_bucket_table(lines, "By day_regime", group_by(rows, "day_regime"), base_mean)

    w("## 3. Regime interactions (strategy impact)")
    w("")
    write_crosstab(lines, "Entry regime × ask band", cross_tab(rows, "regime_reconstructed", "ask_band"))
    write_crosstab(lines, "Entry regime × spot against 120s", cross_tab(rows, "regime_reconstructed", "spot_against_120s"))
    write_crosstab(lines, "Day regime × entry regime", cross_tab(rows, "day_regime", "regime_reconstructed"))
    write_crosstab(lines, "Period × entry regime", cross_tab(rows, "period", "regime_reconstructed"))

    w("## 4. Path stats at entry (continuous → quartile buckets)")
    w("")
    for feat, label in [
        ("path_efficiency", "Path efficiency (30m)"),
        ("sign_flip_rate", "Sign flip rate (30m)"),
        ("vol_180s_bps", "Realized vol 180s (bps)"),
        ("day_rv_bps", "Daily RV (bps)"),
    ]:
        vals = sorted(r[feat] for r in rows if r.get(feat) is not None)
        if len(vals) < 20:
            continue
        qs = [vals[int(len(vals) * p)] for p in (0.25, 0.5, 0.75)]
        buckets: dict[str, list] = defaultdict(list)
        for r in rows:
            v = r.get(feat)
            if v is None:
                continue
            if v <= qs[0]:
                b = "Q1_low"
            elif v <= qs[1]:
                b = "Q2"
            elif v <= qs[2]:
                b = "Q3"
            else:
                b = "Q4_high"
            buckets[b].append(r)
        w(f"### {label}")
        w("")
        w("| Quartile | n | Hit | Total P&L | Mean P&L |")
        w("|----------|---|-----|-----------|----------|")
        for b in ("Q1_low", "Q2", "Q3", "Q4_high"):
            g = bucket_from_rows(buckets.get(b, []))
            if g.n < 5:
                continue
            w(f"| {b} | {g.n} | {g.hit:.0%} | {fmt_money(g.pnl)} | {fmt_money(g.mean_pnl)} |")
        w("")

    w("## 5. Gate counterfactuals (bootstrap Δ mean P&L)")
    w("")
    gates = [
        ("baseline (all)", lambda r: True),
        ("skip calm_low_vol", lambda r: r["regime_reconstructed"] != "calm_low_vol"),
        ("skip expanded_mixed", lambda r: r["regime_reconstructed"] != "expanded_mixed"),
        ("skip expanded_high_flip", lambda r: r["regime_reconstructed"] != "expanded_high_flip"),
        ("prod (skip calm + mixed)", lambda r: r["regime_reconstructed"] not in ("calm_low_vol", "expanded_mixed")),
        ("skip spot against 120s", lambda r: not r.get("spot_against_120s")),
        ("prod + skip high_flip", lambda r: r["regime_reconstructed"] not in ("calm_low_vol", "expanded_mixed", "expanded_high_flip")),
        ("prod + skip spot against", lambda r: r["regime_reconstructed"] not in ("calm_low_vol", "expanded_mixed") and not r.get("spot_against_120s")),
    ]
    w("| Gate | kept n | Hit | Total P&L | Mean P&L | 95% CI | blocked P&L |")
    w("|------|--------|-----|-----------|----------|--------|-------------|")
    summary_gates = []
    for label, pred in gates:
        _, kept, blocked = gate_counterfactual(rows, pred, label)
        mean, lo, hi = bootstrap_ci(kept.pnls or [])
        summary_gates.append({
            "gate": label,
            "n": kept.n,
            "hit": kept.hit,
            "total_pnl": kept.pnl,
            "mean_pnl": mean,
            "ci_lo": lo,
            "ci_hi": hi,
            "blocked_pnl": blocked.pnl,
            "blocked_n": blocked.n,
        })
        w(
            f"| {label} | {kept.n} | {kept.hit:.0%} | {fmt_money(kept.pnl)} | "
            f"{fmt_money(mean)} | [{fmt_money(lo)}, {fmt_money(hi)}] | "
            f"{fmt_money(blocked.pnl)} ({blocked.n} tr) |"
        )
    w("")

    w("## 6. Strategy interpretation")
    w("")
    w("**What hurts fade (robust patterns from this sample):**")
    w("")
    # Auto-pick worst regimes with n>=10
    worst = sorted(
        [(k, g) for k, g in by_regime.items() if g.n >= 10],
        key=lambda x: x[1].mean_pnl,
    )[:3]
    for k, g in worst:
        w(f"- `{k}`: {g.n} trades, {fmt_money(g.pnl)} total, {g.hit:.0%} hit")
    w("")
    w("**What daily conditions correlate with drawdown:**")
    draw = [r for r in rows if r["period"] == "drawdown"]
    good = [r for r in rows if r["period"] == "good_tape"]
    if draw and good:
        d_rv = sum(r.get("day_rv_bps") or 0 for r in draw) / len(draw)
        g_rv = sum(r.get("day_rv_bps") or 0 for r in good) / len(good)
        w(f"- Drawdown days avg RV: **{d_rv:.0f} bps** vs good tape **{g_rv:.0f} bps**")
    w("")
    w("**Complement routing signal:**")
    w("- On `clean_directional` entry regime days, fade underperforms → BR2 `late_favourite` satellite candidate.")
    w("- On `expanded_high_flip` + spot against 120s, fade is most toxic → hard gate or size-down.")
    w("- Daily `chop_whipsaw` label alone is insufficient (Jun 14–16 also chop but profitable).")
    w("")

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {out_path}", file=sys.stderr)

    if args.json:
        jp = Path(args.json)
        jp.parent.mkdir(parents=True, exist_ok=True)
        jp.write_text(
            json.dumps(
                {
                    "n": base.n,
                    "hit": base.hit,
                    "total_pnl": base.pnl,
                    "mean_pnl": base_mean,
                    "by_regime": {k: {"n": g.n, "hit": g.hit, "pnl": g.pnl} for k, g in by_regime.items()},
                    "gates": summary_gates,
                },
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        print(f"wrote {jp}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())