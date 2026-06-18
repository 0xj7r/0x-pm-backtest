#!/usr/bin/env python3
"""Score strategy hunt matrix: fee-net NET, Sharpe, adoption verdict."""
from __future__ import annotations

import json
import re
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "data/runs/strategy_hunt"
REPORT = ROOT / "docs/research/strategy-hunt/scorecard.md"

BANKROLL = 1000.0


def daily_stats(trades: list[dict]) -> tuple[float, float, float]:
    by_day: dict[str, float] = defaultdict(float)
    for t in trades:
        ts = t.get("decision_ts_ns") or t.get("entry_ts_ns") or 0
        if not ts:
            continue
        day = datetime.fromtimestamp(ts / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")
        by_day[day] += float(t.get("pnl_net", t.get("pnl", 0)))
    if not by_day:
        return 0.0, 0.0, 0.0
    vals = list(by_day.values())
    mean = sum(vals) / len(vals)
    worst = min(vals)
    if len(vals) < 2:
        return mean, 0.0, worst
    var = sum((v - mean) ** 2 for v in vals) / (len(vals) - 1)
    sharpe = mean / (var ** 0.5) * (365 ** 0.5) if var > 0 else 0.0
    return mean, sharpe, worst


def load_trades(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(l) for l in path.read_text().splitlines() if l.strip()]


def score_alpha(path: Path) -> dict:
    trades = load_trades(path.with_suffix(".trades.jsonl"))
    if trades:
        net = sum(float(t.get("pnl_net", t.get("pnl", 0))) for t in trades)
    else:
        # Fall back to harness JSON aggregate
        data = json.loads(path.read_text())
        net = 0.0
        for cell in data.get("sweep", [{}])[0].get("report", {}).get("cells", {}).values():
            net += float(cell.get("total_pnl", 0))
    hit = sum(1 for t in trades if float(t.get("pnl_net", t.get("pnl", 0))) > 0) / max(len(trades), 1)
    dmean, sharpe, worst = daily_stats(trades)
    days = len({datetime.fromtimestamp((t.get("decision_ts_ns") or 0) / 1e9, tz=timezone.utc).strftime("%Y-%m-%d") for t in trades if t.get("decision_ts_ns")})
    return {
        "tag": path.stem,
        "net": net,
        "n": len(trades),
        "hit": hit,
        "sharpe": sharpe,
        "worst_day": worst,
        "days": days,
        "e_per_day": len(trades) / max(days, 1),
    }


def score_wf(path: Path) -> dict:
    data = json.loads(path.read_text())
    net = float(data.get("total_pnl_usd", data.get("net_pnl_usd", 0)))
    return {
        "tag": path.parent.name,
        "net": net,
        "n": data.get("markets_evaluated", 0),
        "hit": 0.0,
        "sharpe": 0.0,
        "worst_day": 0.0,
        "days": 0,
        "e_per_day": 0.0,
        "final_equity": data.get("final_equity_usd", BANKROLL + net),
    }


def parse_tag(tag: str) -> tuple[str, str, str]:
    # VERIFY_F1_fade_btc5m -> VERIFY, F1, btc5m
    m = re.match(r"^(TUNE|VERIFY|HOLDOUT)_([FW][0-9]+)_(?:.+_)?([a-z0-9]+)$", tag)
    if m:
        return m.group(1), m.group(2), m.group(3)
    return "", "", tag


def verdict(tune: dict, verify: dict) -> str:
    if not tune or not verify:
        return "INCOMPLETE"
    if tune["net"] <= 0 or verify["net"] <= 0:
        return "REJECT"
    if verify["sharpe"] < 1.0 and verify["n"] > 50:
        return "REJECT"
    if verify["worst_day"] < -0.05 * BANKROLL:
        return "REJECT"
    if verify["hit"] < 0.50 and verify["n"] > 50 and "aligned" in verify["tag"].lower():
        return "REJECT"
    if verify["net"] < 0.9 * tune["net"] * (verify["days"] / max(tune["days"], 1)):
        return "INCONCLUSIVE"
    return "VIABLE"


def main() -> None:
    rows: list[dict] = []
    for p in sorted(OUT.glob("*.json")):
        try:
            rows.append(score_alpha(p))
        except Exception as e:
            print(f"skip {p}: {e}", file=sys.stderr)
    for p in sorted(OUT.glob("*/summary.json")):
        try:
            rows.append(score_wf(p))
        except Exception as e:
            print(f"skip {p}: {e}", file=sys.stderr)

    by_key: dict[tuple[str, str], dict[str, dict]] = defaultdict(dict)
    for r in rows:
        win, fam, mkt = parse_tag(r["tag"])
        if win:
            by_key[(fam, mkt)][win] = r

    lines = [
        "# Strategy Hunt Scorecard\n\n",
        f"Bankroll: ${BANKROLL:.0f}, clip: $25, results: `{OUT}`\n\n",
        "| family | market | TUNE NET | VERIFY NET | VERIFY Sharpe | VERIFY worst day | verdict |\n",
        "|---|---|---:|---:|---:|---:|---|\n",
    ]

    viable = []
    for (fam, mkt), wins in sorted(by_key.items()):
        t, v = wins.get("TUNE", {}), wins.get("VERIFY", {})
        vdict = verdict(t, v)
        lines.append(
            f"| {fam} | {mkt} | {t.get('net', 0):.0f} | {v.get('net', 0):.0f} | "
            f"{v.get('sharpe', 0):.1f} | {v.get('worst_day', 0):.0f} | {vdict} |\n"
        )
        if vdict == "VIABLE":
            viable.append((fam, mkt, v.get("net", 0)))

    if viable:
        lines.append("\n## Holdout candidates\n\n")
        for fam, mkt, net in sorted(viable, key=lambda x: -x[2]):
            lines.append(f"- `{fam}` on `{mkt}` (VERIFY NET ${net:.0f})\n")

    REPORT.parent.mkdir(parents=True, exist_ok=True)
    REPORT.write_text("".join(lines))
    print("".join(lines))
    print(f"Wrote {REPORT}")


if __name__ == "__main__":
    main()