#!/usr/bin/env python3
"""Replay shadow-final JSONL with regime gate combos; score per-day PnL.

Uses `regime` logged on each `would_enter` (decision-time 30m spot classifier).
Joins entries to `resolution` rows for ladder_settle_pnl_usd @ clip.

Usage:
  python3 scripts/research/score_regime_gate_sweep.py \\
    --shadow-dir ~/data/pm-alpha/shadow-final \\
    --since 2026-06-14
"""

from __future__ import annotations

import argparse
import glob
import itertools
import json
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def load_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def load_shadow_events(shadow_dir: Path) -> tuple[list[dict], list[dict]]:
    entries, resolutions = [], []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for ev in load_jsonl(Path(fp)):
            typ = ev.get("type")
            if typ == "would_enter":
                entries.append(ev)
            elif typ == "resolution":
                resolutions.append(ev)
    return entries, resolutions


def index_resolutions(
    entries: list[dict], resolutions: list[dict]
) -> dict[tuple[str, str, int], dict]:
    by_ss_ent: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for e in entries:
        by_ss_ent[(e["slug"], e["side"])].append(e)
    for ents in by_ss_ent.values():
        ents.sort(key=lambda x: x["ts_utc"])

    by_ss_res: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for r in resolutions:
        by_ss_res[(r["slug"], r["side"])].append(r)
    for ress in by_ss_res.values():
        ress.sort(key=lambda x: x["ts_utc"])

    out: dict[tuple[str, str, int], dict] = {}
    for (slug, side), ents in by_ss_ent.items():
        for ent, res in zip(ents, by_ss_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = res
    return out


@dataclass(frozen=True)
class GateCombo:
    skip_calm: bool = False
    only_calm: bool = False
    skip_expanded_mixed: bool = False
    skip_expanded_high_flip: bool = False

    def label(self) -> str:
        parts = []
        if self.skip_calm:
            parts.append("skip_calm")
        if self.only_calm:
            parts.append("only_calm")
        if self.skip_expanded_mixed:
            parts.append("skip_exp_mixed")
        if self.skip_expanded_high_flip:
            parts.append("skip_exp_flip")
        return "+".join(parts) if parts else "baseline"

    def allows(self, regime: str | None) -> bool:
        if regime is None:
            return True
        if self.skip_calm and regime == "calm_low_vol":
            return False
        if self.only_calm and regime != "calm_low_vol":
            return False
        if self.skip_expanded_mixed and regime == "expanded_mixed":
            return False
        if self.skip_expanded_high_flip and regime == "expanded_high_flip":
            return False
        return True


def combos() -> list[GateCombo]:
    flags = ("skip_calm", "only_calm", "skip_expanded_mixed", "skip_expanded_high_flip")
    out: list[GateCombo] = [GateCombo()]
    for r in range(1, len(flags) + 1):
        for bits in itertools.combinations(flags, r):
            if "skip_calm" in bits and "only_calm" in bits:
                continue
            kw = {f: True for f in bits}
            out.append(GateCombo(**kw))
    return out


def score_combo(
    entries: list[dict],
    res_idx: dict[tuple[str, str, int], dict],
    combo: GateCombo,
) -> tuple[float, int, int, dict[str, float], dict[str, int]]:
    gross = 0.0
    wins = losses = 0
    by_day: dict[str, float] = defaultdict(float)
    by_regime: dict[str, int] = defaultdict(int)

    for ent in entries:
        regime = ent.get("regime")
        if not combo.allows(regime):
            continue
        key = (ent["slug"], ent["side"], int(ent.get("clip", 1)))
        res = res_idx.get(key)
        if not res:
            continue
        pnl = float(res.get("ladder_settle_pnl_usd") or 0)
        gross += pnl
        if res.get("won"):
            wins += 1
        else:
            losses += 1
        day = parse_ts(ent["ts_utc"]).strftime("%Y-%m-%d")
        by_day[day] += pnl
        by_regime[regime or "unknown"] += 1

    return gross, wins, losses, dict(by_day), dict(by_regime)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--since", default=None, help="YYYY-MM-DD UTC cutoff")
    ap.add_argument("--top", type=int, default=15, help="show top N combos by total PnL")
    args = ap.parse_args()

    cutoff = None
    if args.since:
        cutoff = datetime.strptime(args.since, "%Y-%m-%d").replace(tzinfo=timezone.utc)

    entries, resolutions = load_shadow_events(Path(args.shadow_dir))
    if cutoff:
        entries = [e for e in entries if parse_ts(e["ts_utc"]) >= cutoff]
        # keep resolutions for joined entries only
    res_idx = index_resolutions(entries, resolutions)

    labeled = sum(1 for e in entries if e.get("regime"))
    print(f"# Regime gate sweep: {args.shadow_dir}")
    if cutoff:
        print(f"since {args.since}")
    print(f"would_enter: {len(entries)}  labeled: {labeled}  resolved: {len(res_idx)}")
    print()

    baseline_gross, _, _, baseline_days, _ = score_combo(entries, res_idx, GateCombo())
    print(f"baseline total: ${baseline_gross:,.0f}")
    for day in sorted(baseline_days):
        print(f"  {day}: ${baseline_days[day]:+,.0f}")
    print()

    rows: list[tuple[float, GateCombo, int, int, dict[str, float]]] = []
    for combo in combos():
        if combo.label() == "baseline":
            continue
        gross, w, l, by_day, _ = score_combo(entries, res_idx, combo)
        rows.append((gross, combo, w, l, by_day))

    rows.sort(key=lambda x: x[0], reverse=True)
    print(f"top {args.top} combos (sorted by total PnL):")
    print(f"{'combo':<40} {'pnl':>10} {'delta':>10} {'trades':>7} {'hit':>6}")
    for gross, combo, w, l, _ in rows[: args.top]:
        n = w + l
        hit = (w / n * 100) if n else 0.0
        delta = gross - baseline_gross
        print(f"{combo.label():<40} ${gross:>8,.0f} ${delta:>+8,.0f} {n:>7} {hit:>5.1f}%")

    print()
    print("per-day for top 3:")
    for gross, combo, w, l, by_day in rows[:3]:
        print(f"\n## {combo.label()}  total=${gross:,.0f}  trades={w+l}")
        for day in sorted(by_day):
            base = baseline_days.get(day, 0.0)
            print(f"  {day}: ${by_day[day]:+,.0f}  (baseline ${base:+,.0f}, delta ${by_day[day]-base:+,.0f})")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())