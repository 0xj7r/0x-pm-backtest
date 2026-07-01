#!/usr/bin/env python3
"""Discovery screen on shadow-final labeled entries (05-quant-signals protocol).

Ranks exo/dir/flow/spot features by loser separation (SMD, AUC for P(loss)).
Works on historical JSONL even before live flow telemetry — uses logged spot_ret
and dir/exo vectors; enriched flow fields scored when present.

Usage:
  python3 scripts/research/shadow_flow_discovery.py \\
    --shadow-dir ~/data/pm-alpha/shadow-final \\
    --since 2026-06-14 \\
    --focus-period drawdown \\
    --out ~/data/runs/shadow_flow_discovery/report.md
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from shadow_analysis_lib import (
    DIR_NAMES,
    EXO_NAMES,
    FLOW_COLS,
    SPOT_RET_COLS,
    auc_binary,
    enrich_rows,
    filter_period,
    fmt_money,
    group_by,
    load_jsonl_events,
    pair_entries,
    smd,
)


def feature_cols(rows: list[dict]) -> list[str]:
    cols = list(SPOT_RET_COLS)
    cols += [f"exo_{n}" for n in EXO_NAMES]
    cols += [f"dir_{n}" for n in DIR_NAMES]
    cols += FLOW_COLS
    cols += ["path_efficiency", "sign_flip_rate", "vol_180s_bps", "day_rv_bps", "day_trend_eff"]
    return [c for c in cols if any(r.get(c) is not None for r in rows)]


def rank_features(rows: list[dict], cols: list[str]) -> list[dict]:
    y_loss = [0 if r["won"] else 1 for r in rows]
    ranked = []
    for col in cols:
        scores = []
        mask = []
        for r in rows:
            v = r.get(col)
            if v is None:
                continue
            scores.append(float(v))
            mask.append(0 if r["won"] else 1)
        if len(scores) < 20:
            continue
        wins = [s for s, y in zip(scores, mask) if y == 0]
        losses = [s for s, y in zip(scores, mask) if y == 1]
        if len(wins) < 5 or len(losses) < 5:
            continue
        ranked.append({
            "feature": col,
            "smd_loss_vs_win": smd(losses, wins),
            "auc_loss": auc_binary(mask, scores),
            "mean_win": sum(wins) / len(wins),
            "mean_loss": sum(losses) / len(losses),
            "n": len(scores),
        })
    ranked.sort(key=lambda x: abs(x["smd_loss_vs_win"]), reverse=True)
    return ranked


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--day-regime", default=None)
    ap.add_argument("--since", default="2026-06-14")
    ap.add_argument("--until", default=None)
    ap.add_argument(
        "--focus-period",
        choices=("all", "good_tape", "drawdown", "regime_known"),
        default="all",
    )
    ap.add_argument("--out", default="data/runs/shadow_flow_discovery/report.md")
    args = ap.parse_args()

    entries, resolutions = load_jsonl_events(Path(args.shadow_dir))
    rows = pair_entries(entries, resolutions)
    rows = filter_period(rows, args.since, args.until)
    enrich_rows(rows, Path(args.day_regime) if args.day_regime else None)

    if args.focus_period == "good_tape":
        rows = [r for r in rows if r["period"] == "good_tape"]
    elif args.focus_period == "drawdown":
        rows = [r for r in rows if r["period"] == "drawdown"]
    elif args.focus_period == "regime_known":
        rows = [r for r in rows if r.get("regime_logged")]

    if not rows:
        print("No rows in focus window", file=sys.stderr)
        return 1

    cols = feature_cols(rows)
    ranked = rank_features(rows, cols)
    n_loss = sum(1 for r in rows if not r["won"])
    n_win = len(rows) - n_loss
    total_pnl = sum(r["pnl_usd"] for r in rows)
    n_flow = sum(1 for r in rows if r.get("binance_adverse_vol_30s") is not None)

    by_regime = group_by(rows, "regime_reconstructed")
    regime_feats = rank_features(rows, ["path_efficiency", "sign_flip_rate", "vol_180s_bps"])

    lines = [
        "# Shadow flow & feature discovery",
        "",
        f"Focus: **{args.focus_period}** | n={len(rows)} (W={n_win} L={n_loss}) | P&L={fmt_money(total_pnl)}",
        f"Enriched flow telemetry present: **{n_flow}/{len(rows)}**",
        "",
        "Label: **loss** (1 = loser, 0 = winner). Positive SMD = higher on losers.",
        "AUC: ability to rank losses above wins (0.5 = random, >0.6 = discovery candidate).",
        "",
        "## Top features (|SMD| rank)",
        "",
        "| Feature | SMD (loss vs win) | AUC loss | mean(win) | mean(loss) | n |",
        "|---------|-------------------|----------|-----------|------------|---|",
    ]
    for r in ranked[:25]:
        lines.append(
            f"| {r['feature']} | {r['smd_loss_vs_win']:+.2f} | {r['auc_loss']:.3f} | "
            f"{r['mean_win']:.3f} | {r['mean_loss']:.3f} | {r['n']} |"
        )
    lines.extend(["", "## Regime path stats", ""])
    lines.append("| Feature | SMD | AUC loss |")
    lines.append("|---------|-----|----------|")
    for r in regime_feats:
        lines.append(f"| {r['feature']} | {r['smd_loss_vs_win']:+.2f} | {r['auc_loss']:.3f} |")

    lines.extend(["", "## Fade P&L by reconstructed entry regime", ""])
    lines.append("| Regime | n | Hit | P&L |")
    lines.append("|--------|---|-----|-----|")
    for k, g in sorted(by_regime.items(), key=lambda x: x[1].pnl):
        if g.n < 3:
            continue
        lines.append(f"| {k} | {g.n} | {g.hit:.0%} | {fmt_money(g.pnl)} |")

    lines.extend([
        "",
        "## Discovery gates (|SMD| > 0.15 or AUC > 0.55)",
        "",
    ])
    candidates = [r for r in ranked if abs(r["smd_loss_vs_win"]) >= 0.15 or r["auc_loss"] >= 0.55]
    if candidates:
        for r in candidates[:10]:
            direction = "block when HIGH" if r["smd_loss_vs_win"] > 0 else "block when LOW"
            lines.append(f"- **{r['feature']}**: SMD={r['smd_loss_vs_win']:+.2f}, AUC={r['auc_loss']:.3f} → {direction}")
    else:
        lines.append("- No features cleared discovery bar in this window (need more enriched-flow samples).")

    lines.extend([
        "",
        "## Next steps",
        "",
        "- Promote features with AUC ≥ 0.58 into `decide_entry` gate sweep (shadow JSONL replay).",
        "- Re-run with `--focus-period drawdown` after 48h enriched telemetry.",
        "- Pair with `shadow_regime_impact.py` for regime × feature interactions.",
        "",
    ])

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {out} features_ranked={len(ranked)}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())