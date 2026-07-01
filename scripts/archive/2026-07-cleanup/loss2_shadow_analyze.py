#!/usr/bin/env python3
"""Analyze loss2 signal on shadow-final + gate_ab + live log.

Shows: blocked vs taken after 2 consec losses, next-trade outcomes, session P&L delta.
"""
from __future__ import annotations

import argparse
import glob
import json
import re
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

TS_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
LIVE_ENTER_RE = re.compile(
    r"LIVE ENTER (UP|DOWN) (\S+).*touch=([\d.]+).*clip=(\d+)"
)


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def load_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def build_res_index(shadow_dir: Path) -> dict[tuple[str, str, int], dict]:
    entries, resolutions = [], []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for ev in load_jsonl(Path(fp)):
            if ev.get("type") == "would_enter":
                entries.append(ev)
            elif ev.get("type") == "resolution":
                resolutions.append(ev)
    by_ent: dict[tuple[str, str], list] = defaultdict(list)
    for e in entries:
        by_ent[(e["slug"], e["side"])].append(e)
    for ents in by_ent.values():
        ents.sort(key=lambda x: x["ts_utc"])
    by_res: dict[tuple[str, str], list] = defaultdict(list)
    for r in resolutions:
        by_res[(r["slug"], r["side"])].append(r)
    for ress in by_res.values():
        ress.sort(key=lambda x: x["ts_utc"])
    out: dict[tuple[str, str, int], dict] = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = res
    return out


def pnl_touch(clip: float, touch: float, won: bool) -> float:
    return clip / touch - clip if won else -clip


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--shadow-dir", default="/home/ubuntu/data/pm-alpha/shadow-final")
    ap.add_argument("--gate-ab", default="/home/ubuntu/data/pm-alpha/shadow-gate-ab/gate_ab.jsonl")
    ap.add_argument("--live-log", default="/home/ubuntu/data/pm-alpha/shadow_exec_tail.log")
    ap.add_argument("--since", default="2026-06-16T12:10:00Z")
    ap.add_argument("--clip-usd", type=float, default=25.0)
    args = ap.parse_args()

    cutoff = parse_ts(args.since)
    res_idx = build_res_index(Path(args.shadow_dir))

    # Chronological REF entries with resolutions
    entries = []
    for fp in sorted(glob.glob(str(Path(args.shadow_dir) / "shadow-*.jsonl"))):
        for ev in load_jsonl(Path(fp)):
            if ev.get("type") != "would_enter":
                continue
            if parse_ts(ev["ts_utc"]) < cutoff:
                continue
            key = (ev["slug"], ev["side"], int(ev.get("clip", 1)))
            res = res_idx.get(key)
            if not res:
                continue
            entries.append(
                {
                    "ts": ev["ts_utc"],
                    "slug": ev["slug"],
                    "side": ev["side"],
                    "clip": int(ev.get("clip", 1)),
                    "touch": float(ev.get("touch") or ev.get("limit") or 0.5),
                    "won": bool(res.get("won")),
                    "pnl50": float(res.get("ladder_settle_pnl_usd") or 0),
                }
            )
    entries.sort(key=lambda x: x["ts"])

    # Simulate loss2 gate on chronological stream
    consec = 0
    taken, blocked = [], []
    for e in entries:
        if consec >= 2:
            blocked.append({**e, "consec_before": consec})
        else:
            taken.append({**e, "consec_before": consec})
        if e["won"]:
            consec = 0
        else:
            consec += 1

    def summarize(rows: list[dict], label: str) -> None:
        n = len(rows)
        w = sum(1 for r in rows if r["won"])
        pnl = sum(r["pnl50"] * (args.clip_usd / 50) for r in rows)
        print(f"  {label:32s} n={n:4d}  {w}W/{n-w}L  gross=${pnl:+.1f}  $/tr=${pnl/n if n else 0:+.2f}")

    print(f"REF resolved since {args.since} (@ ${args.clip_usd:g} scaled from telemetry)")
    summarize(entries, "baseline (all)")
    summarize(taken, "loss2-gated TAKEN")
    summarize(blocked, "loss2-gated BLOCKED")

    # Trades immediately after 2nd loss (consec_before==2 in blocked = first blocked trade)
    first_blocked = [r for r in blocked if r["consec_before"] == 2]
    third_plus = [r for r in blocked if r["consec_before"] > 2]
    summarize(first_blocked, "  -> 1st blocked (post-loss2)")
    summarize(third_plus, "  -> deeper streak blocked")

    # Gate A/B file
    gate_rows = [
        r
        for r in load_jsonl(Path(args.gate_ab))
        if r.get("type") == "gate_eval" and parse_ts(r["ts_utc"]) >= cutoff
    ]
    if gate_rows:
        by_reason: dict[str, list] = defaultdict(list)
        for r in gate_rows:
            if not r.get("gated_allow"):
                by_reason[r.get("gate_reason", "?")].append(r)
        print(f"\nGate sidecar evals: {len(gate_rows)}")
        for reason, rows in sorted(by_reason.items()):
            keys = [(r["slug"], r["side"], int(r.get("clip", 1))) for r in rows]
            gross = wins = losses = 0
            for k in keys:
                res = res_idx.get(k)
                if not res:
                    continue
                p = float(res.get("ladder_settle_pnl_usd") or 0) * (args.clip_usd / 50)
                gross += p
                if res.get("won"):
                    wins += 1
                else:
                    losses += 1
            print(f"  blocked {reason}: n={len(rows)} resolved={wins+losses} ({wins}W/{losses}L) gross=${gross:+.1f}")

    # LIVE: entries after 2 consec resolved losses
    live_legs = []
    seen: set[tuple[str, int]] = set()
    with Path(args.live_log).open(encoding="utf-8") as f:
        for line in f:
            if "LIVE ENTER" not in line:
                continue
            ts_m = TS_RE.search(line)
            m = LIVE_ENTER_RE.search(line)
            if not ts_m or not m:
                continue
            if parse_ts(ts_m.group(1) + "Z") < cutoff:
                continue
            slug, clip = m.group(2), int(m.group(4))
            key = (slug, clip)
            if key in seen:
                continue
            seen.add(key)
            live_legs.append(
                {
                    "ts": ts_m.group(1),
                    "side": m.group(1).lower(),
                    "slug": slug,
                    "touch": float(m.group(3)),
                    "clip": clip,
                }
            )
    live_legs.sort(key=lambda x: x["ts"])

    consec = 0
    live_taken, live_post_loss2 = [], []
    for leg in live_legs:
        key = (leg["slug"], leg["side"], leg["clip"])
        res = res_idx.get(key)
        if not res:
            continue
        won = bool(res.get("won"))
        g = pnl_touch(args.clip_usd, leg["touch"], won)
        row = {**leg, "won": won, "gross": g, "consec_before": consec}
        live_taken.append(row)
        if consec >= 2:
            live_post_loss2.append(row)
        consec = 0 if won else consec + 1

    if live_taken:
        print(f"\nLIVE resolved since {args.since} (@ ${args.clip_usd:g})")
        n = len(live_taken)
        w = sum(1 for r in live_taken if r["won"])
        g = sum(r["gross"] for r in live_taken)
        print(f"  all resolved: n={n} {w}W/{n-w}L gross=${g:+.1f}")
        if live_post_loss2:
            n2 = len(live_post_loss2)
            w2 = sum(1 for r in live_post_loss2 if r["won"])
            g2 = sum(r["gross"] for r in live_post_loss2)
            print(f"  taken AFTER loss2 (ungated): n={n2} {w2}W/{n2-w2}L gross=${g2:+.1f}")
            print(f"  counterfactual save if loss2 blocked those: ${-g2:+.1f}")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())