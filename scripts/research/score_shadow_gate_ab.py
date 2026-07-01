#!/usr/bin/env python3
"""Score shadow A/B: baseline REF would_enter vs gated_allow from gate_eval JSONL.

Joins gate_eval with shadow-final resolutions (slug+side+clip) for P&L @ $50 REF
telemetry (scale ×0.5 for $25 live).

Usage:
  python3 scripts/research/score_shadow_gate_ab.py \\
    --gate-ab ~/data/pm-alpha/shadow-gate-ab/gate_ab.jsonl \\
    --shadow-dir ~/data/pm-alpha/shadow-final
"""

from __future__ import annotations

import argparse
import glob
import json
from collections import defaultdict
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


def pnl_for(keys: list[tuple], res_idx: dict) -> tuple[float, int, int]:
    gross = 0.0
    wins = losses = 0
    for key in keys:
        res = res_idx.get(key)
        if not res:
            continue
        pnl = float(res.get("ladder_settle_pnl_usd") or 0)
        gross += pnl
        if res.get("won"):
            wins += 1
        else:
            losses += 1
    return gross, wins, losses


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--gate-ab", required=True)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--since", default=None, help="ISO UTC cutoff e.g. 2026-06-17T00:00:00Z")
    ap.add_argument("--clip-usd", type=float, default=50.0)
    args = ap.parse_args()

    cutoff = parse_ts(args.since) if args.since else None
    gate_rows = load_jsonl(Path(args.gate_ab))
    gate_rows = [r for r in gate_rows if r.get("type") == "gate_eval"]
    if cutoff:
        gate_rows = [r for r in gate_rows if parse_ts(r["ts_utc"]) >= cutoff]

    entries, resolutions = load_shadow_events(Path(args.shadow_dir))
    res_idx = index_resolutions(entries, resolutions)

    baseline_keys = [
        (r["slug"], r["side"], int(r.get("clip", 1))) for r in gate_rows
    ]
    gated_keys = [
        (r["slug"], r["side"], int(r.get("clip", 1)))
        for r in gate_rows
        if r.get("gated_allow")
    ]
    blocked = [r for r in gate_rows if not r.get("gated_allow")]

    b_gross, b_w, b_l = pnl_for(baseline_keys, res_idx)
    g_gross, g_w, g_l = pnl_for(gated_keys, res_idx)

    print(f"# Shadow gate A/B — {args.gate_ab}")
    if cutoff:
        print(f"since {args.since}")
    print()
    print(f"gate_eval rows:     {len(gate_rows)}")
    print(f"blocked:            {len(blocked)}")
    by_reason: dict[str, int] = defaultdict(int)
    for r in blocked:
        by_reason[str(r.get("block_reason") or "unknown")] += 1
    for reason, n in sorted(by_reason.items()):
        print(f"  block {reason}: {n}")
    print()
    n_b = b_w + b_l
    n_g = g_w + g_l
    print(
        f"BASELINE (all REF):  resolved={n_b:4d}  "
        f"W/L={b_w}/{b_l}  NET=${b_gross:+,.2f}  @ ${args.clip_usd:.0f}/clip"
    )
    print(
        f"GATED arm:           resolved={n_g:4d}  "
        f"W/L={g_w}/{g_l}  NET=${g_gross:+,.2f}  Δ=${g_gross - b_gross:+,.2f}"
    )
    if args.clip_usd == 50.0:
        print(f"GATED @ $25 live:   NET=${g_gross * 0.5:+,.2f}  Δ=${(g_gross - b_gross) * 0.5:+,.2f}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())