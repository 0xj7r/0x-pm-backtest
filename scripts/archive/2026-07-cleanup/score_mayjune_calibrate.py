#!/usr/bin/env python3
"""Score May/June calibration runs — adoption verdict vs legacy_prod."""
from __future__ import annotations

import json
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "data/runs/mayjune_calibrate"
TSV = OUT / "summary.tsv"
REPORT = ROOT / "docs/research/strategy-hunt/mayjune_calibrate_scorecard.md"


def daily_stats(trades: list[dict]) -> tuple[float, float]:
    by_day: dict[str, float] = defaultdict(float)
    for t in trades:
        ts = t.get("decision_ts_ns") or 0
        if not ts:
            continue
        day = datetime.fromtimestamp(ts / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")
        by_day[day] += float(t.get("pnl_net", t.get("pnl", 0)))
    if not by_day:
        return 0.0, 0.0
    vals = list(by_day.values())
    return sum(vals) / len(vals), min(vals)


def load_row(path: Path) -> dict:
    trades = []
    tp = path.with_suffix(".trades.jsonl")
    if tp.is_file():
        trades = [json.loads(l) for l in tp.read_text().splitlines() if l.strip()]
    if trades:
        net = sum(float(t.get("pnl_net", t.get("pnl", 0))) for t in trades)
        n = len(trades)
        hit = sum(1 for t in trades if float(t.get("pnl_net", t.get("pnl", 0))) > 0) / n
    else:
        r = json.loads(path.read_text())
        agg = (r.get("sweep") or [r])[0]["report"]["aggregate"]
        net = float(agg.get("total_pnl", 0))
        n = int(agg.get("n_trades", 0))
        hit = float(agg.get("hit_rate", 0))
    dmean, worst = daily_stats(trades)
    return {"net": net, "n": n, "hit": hit, "worst_day": worst, "mean_day": dmean}


def main() -> int:
    if not TSV.is_file():
        print(f"missing {TSV} — run ./scripts/mayjune_strategy_calibrate.sh first", file=sys.stderr)
        return 1

    rows: list[tuple[str, str, dict]] = []
    for line in TSV.read_text().splitlines()[1:]:
        if not line.strip():
            continue
        win, var, n, net, hit, per = line.split("\t")
        rows.append((win, var, {"n": int(n), "net": float(net), "hit": float(hit) / 100, "per": float(per)}))

    # Enrich from trade tapes where available
    for win, var, _ in list(rows):
        p = OUT / f"{win}_{var}.json"
        if p.is_file():
            extra = load_row(p)
            for i, (w, v, d) in enumerate(rows):
                if w == win and v == var:
                    rows[i] = (w, v, {**d, **extra})

    lines = [
        "# May/June strategy calibration scorecard",
        "",
        "Tuned **only** on May 7 – Jun 17 tape. Do not compare promotion to Feb–Apr VERIFY.",
        "",
        "| window | variant | trades | NET | hit% | $/tr | worst day |",
        "|--------|---------|--------|-----|------|------|-----------|",
    ]
    for win in ("VAL_MAY", "VAL_JUN", "FULL"):
        sub = [(w, v, d) for w, v, d in rows if w == win]
        if not sub:
            continue
        lines.append(f"\n### {win}\n")
        base_net = next((d["net"] for _, v, d in sub if v == "legacy_prod"), None)
        for _, var, d in sorted(sub, key=lambda x: -x[2]["net"]):
            delta = ""
            if base_net is not None and var != "legacy_prod":
                delta = f" ({d['net'] - base_net:+.0f} vs legacy)"
            wd = d.get("worst_day", 0)
            lines.append(
                f"| {win} | {var} | {d['n']} | ${d['net']:+,.0f} | {d['hit']*100:.1f}% | "
                f"${d.get('per', d['net']/max(d['n'],1)):+.2f} | ${wd:+,.0f} |{delta}"
            )

    # Walk-forward compare
    lines.append("\n### Walk-forward $1K (May 7 – Jun 17)\n")
    for strat in ("exo_fade", "mayjune_fade"):
        sp = OUT / f"wf_{strat}" / "summary.json"
        if sp.is_file():
            s = json.loads(sp.read_text())
            net = float(s.get("total_pnl_usd", s.get("net_pnl_usd", 0)))
            lines.append(f"- **{strat}**: NET ${net:+,.0f}")

    lines.append("\n## Promotion rule (May/June only)\n")
    lines.append("- Beat `legacy_prod` on **both** VAL_MAY and VAL_JUN NET")
    lines.append("- Worst day not worse than legacy on FULL window")
    lines.append("- If `mayjune_cal` beats `mayjune` on JUN val → deploy calibrator; else gates-only\n")

    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text("\n".join(lines) + "\n")
    print(REPORT.read_text())
    return 0


if __name__ == "__main__":
    raise SystemExit(main())