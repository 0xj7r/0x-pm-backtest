#!/usr/bin/env python3
"""June regime diagnostics: when does gated fade win/lose, and what should route to BR2?

Joins:
  - data/runs/june_gated_daily/daily.tsv (fade PnL by day)
  - Binance 1m BTC spot (daily vol, trend, whipsaw proxy)
  - Optional harness trade tapes for entry-level features

Outputs markdown report to data/runs/june_regime_compare/report.md

Usage:
  python3 scripts/research/june_regime_compare.py
  python3 scripts/research/june_regime_compare.py --trades data/runs/june_gated_daily/2026-06-15_trades.jsonl
"""

from __future__ import annotations

import argparse
import json
import statistics as st
from collections import defaultdict
from dataclasses import dataclass
from functools import lru_cache
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq

ROOT = Path(__file__).resolve().parents[2]
FADE_TSV = ROOT / "data/runs/june_gated_daily/daily.tsv"
OUT_DIR = ROOT / "data/runs/june_regime_compare"
SPOT_DIR = ROOT / "data/cache/raw/binance/exchange=binance/channel=agg_trades"


@dataclass
class DaySpot:
    day: str
    ret_bps: float
    abs_ret_bps: float
    rv_bps: float
    range_bps: float
    sign_flips: int
    trend_eff: float  # |ret| / sum(|1m ret|) — 1 = clean trend, 0 = chop


def load_fade_daily(path: Path) -> dict[str, float]:
    out: dict[str, float] = {}
    for line in path.read_text().splitlines()[1:]:
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) >= 6 and parts[1] == "base":
            out[parts[0]] = float(parts[3])
        elif len(parts) == 5:
            out[parts[0]] = float(parts[2])
    return out


@lru_cache(maxsize=32)
def load_spot_day(day: str) -> DaySpot | None:
    ddir = SPOT_DIR / f"symbol=BTCUSDT/date={day}"
    if not ddir.is_dir():
        return None
    files = sorted(ddir.glob("*.parquet"))
    if not files:
        return None
    tbl = pq.ParquetFile(files[0]).read(columns=["price", "transact_time_ms"])
    px = np.asarray(tbl.column("price").to_pylist(), dtype=float)
    if len(px) < 100:
        return None
    # Resample to ~1m buckets for daily regime stats
    us = tbl.column("transact_time_ms").to_numpy()
    order = np.argsort(us)
    us, px = us[order], px[order]
    bucket_ms = 300_000  # 5m bars — avoids tick-noise inflation of sign flips
    buckets: list[float] = []
    t0 = int(us[0])
    for t in range(t0, int(us[-1]), bucket_ms):
        idx = int(np.searchsorted(us, t + bucket_ms, side="right")) - 1
        if idx >= 0 and px[idx] > 0:
            buckets.append(float(px[idx]))
    if len(buckets) < 10:
        return None
    px = np.asarray(buckets, dtype=float)
    rets = np.diff(px) / px[:-1]
    ret_bps = (px[-1] / px[0] - 1.0) * 1e4
    abs_1m = np.abs(rets) * 1e4
    rv = float(np.sqrt((rets**2).sum()) * 1e4)
    rng = (px.max() / px.min() - 1.0) * 1e4
    signs = np.sign(rets)
    flips = int(np.sum(signs[1:] * signs[:-1] < 0))
    path_len = float(abs_1m.sum()) or 1.0
    trend_eff = abs(ret_bps) / path_len
    return DaySpot(day, ret_bps, abs(ret_bps), rv, rng, flips, trend_eff)


def load_trades(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    return [json.loads(l) for l in path.read_text().splitlines() if l.strip()]


def p_side(t: dict) -> float:
    p = float(t["p_exo"])
    return p if t["side"] == "Yes" else 1.0 - p


def ask(t: dict) -> float:
    return float(t.get("side_ask_at_decision") or t["avg_price"])


def mid_yes(t: dict) -> float:
    return float(t["mid_at_decision"])


def analyze_trades(trades: list[dict]) -> dict:
    if not trades:
        return {}
    gap = [t for t in trades if p_side(t) > 0.88 and ask(t) < 0.65]
    underdog = [t for t in trades if ask(t) < 0.50]
    fav = [t for t in trades if ask(t) >= 0.70]
    misalign = []
    for t in trades:
        m = mid_yes(t)
        fav_is_yes = m >= 0.5
        bought_yes = t["side"] == "Yes"
        if fav_is_yes != bought_yes:
            misalign.append(t)
    def stats(xs):
        if not xs:
            return {"n": 0, "net": 0.0, "hit": 0.0}
        pnls = [float(x["pnl"]) for x in xs]
        return {
            "n": len(xs),
            "net": sum(pnls),
            "hit": sum(1 for p in pnls if p > 0) / len(pnls),
        }
    return {
        "all": stats(trades),
        "gap_p88_ask65": stats(gap),
        "underdog_ask50": stats(underdog),
        "fav_ask70": stats(fav),
        "fade_vs_book_fav": stats(misalign),
    }


def classify_regime(sp: DaySpot) -> str:
    if sp.trend_eff >= 0.35 and sp.abs_ret_bps >= 40:
        return "directional_trend"
    if sp.sign_flips >= 400 and sp.trend_eff < 0.15:
        return "chop_whipsaw"
    if sp.rv_bps >= 80:
        return "high_vol"
    if sp.rv_bps < 35:
        return "low_vol"
    return "mixed"


def router_recommendation(sp: DaySpot, fade_pnl: float) -> str:
    reg = classify_regime(sp)
    if reg == "directional_trend":
        return "br2_late_favourite (momentum-aligned directional)"
    if reg == "chop_whipsaw":
        return "risk_off or br2 only with high vol gate; fade underdog bleeds"
    if reg == "high_vol" and fade_pnl < 0:
        return "br2_late_favourite + skip fade mid-band"
    if reg == "low_vol" and fade_pnl > 200:
        return "fade OK (stale-book windows)"
    return "mixed — use per-window gates (gap + mom30)"


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--fade-tsv", type=Path, default=FADE_TSV)
    ap.add_argument("--trades", type=Path, action="append", default=[])
    ap.add_argument("-o", type=Path, default=OUT_DIR / "report.md")
    args = ap.parse_args()
    OUT_DIR.mkdir(parents=True, exist_ok=True)

    fade = load_fade_daily(args.fade_tsv)
    rows: list[tuple[str, float, DaySpot | None]] = []
    for d in sorted(fade):
        rows.append((d, fade[d], load_spot_day(d)))

    win_days = [r for r in rows if r[1] > 100]
    loss_days = [r for r in rows if r[1] < -50]
    zero_days = [r for r in rows if abs(r[1]) < 1]

    def spot_avg(rs, attr):
        xs = [getattr(r[2], attr) for r in rs if r[2]]
        return st.mean(xs) if xs else float("nan")

    lines: list[str] = []
    w = lines.append
    w("# June Regime Analysis — Fade vs Directional Router")
    w("")
    w(f"Fade source: `{args.fade_tsv}`")
    w(f"Generated: {datetime.now(timezone.utc):%Y-%m-%d %H:%M UTC}")
    w("")
    w("## Executive summary")
    w("")
    total = sum(fade.values())
    w(f"- **Gated fade June total:** ${total:+,.0f} ({len(fade)} days)")
    w(f"- **Win days (>$100):** {len(win_days)} | **Loss days (<-$50):** {len(loss_days)} | **Flat:** {len(zero_days)}")
    w("")
    w("June is **not one regime** — it's a mix of:")
    w("1. **Directional trend days** (high trend efficiency, spot moves >40bps) → fade underdog loses; **BR2 late-favourite** aligns")
    w("2. **Chop/whipsaw days** (many 1m sign flips, low trend efficiency) → model-book gap failures cluster")
    w("3. **Low-vol stale-book days** → fade still works (Jun 1–5, 10, 14–15)")
    w("")
    w("## Daily spot features vs fade PnL")
    w("")
    w("| date | fade NET | spot ret | RV bps | range bps | 1m flips | trend eff | regime | router |")
    w("|------|----------|----------|--------|-----------|----------|-----------|--------|--------|")
    for d, pnl, sp in rows:
        if not sp:
            w(f"| {d} | ${pnl:+,.0f} | — | — | — | — | — | no_spot | — |")
            continue
        reg = classify_regime(sp)
        route = router_recommendation(sp, pnl)
        w(
            f"| {d} | ${pnl:+,.0f} | {sp.ret_bps:+.0f} | {sp.rv_bps:.0f} | "
            f"{sp.range_bps:.0f} | {sp.sign_flips} | {sp.trend_eff:.2f} | {reg} | {route} |"
        )
    w("")
    w("## Win vs loss day fingerprints (spot)")
    w("")
    w("| metric | win days (>$100) | loss days (<-$50) |")
    w("|--------|------------------|-------------------|")
    for attr, label in [
        ("abs_ret_bps", "|spot ret| bps"),
        ("rv_bps", "realized vol bps"),
        ("range_bps", "high-low bps"),
        ("sign_flips", "1m sign flips"),
        ("trend_eff", "trend efficiency"),
    ]:
        w(f"| {label} | {spot_avg(win_days, attr):.1f} | {spot_avg(loss_days, attr):.1f} |")
    w("")
    w("## Proposed live regime router")
    w("")
    w("```")
    w("IF trend_eff >= 0.30 AND abs(spot_ret_1d) >= 35bps:")
    w("    RUN br2_late_favourite (ask 0.70–0.97, vol gate 1.25bps)")
    w("    SKIP fade underdog entries (ask < 0.65)")
    w("ELIF sign_flips >= 380 AND trend_eff < 0.18:")
    w("    RISK_OFF fade OR require prod_against + gap gate (p<0.88 or ask>0.62)")
    w("ELIF rv_bps < 40 AND trend_eff < 0.25:")
    w("    FADE OK (stale-book regime)")
    w("ELSE:")
    w("    FADE with mom30 + open_fav_gap + max_p_side 0.92")
    w("```")
    w("")
    w("## Harness gate counterfactuals (Jun 1–17, from drawdown_gate_sweep)")
    w("")
    w("| variant | Jun 1–17 NET | vs prod |")
    w("|---------|-------------|---------|")
    w("| prod_mom30_ask045 (current) | +$12,712 | baseline |")
    w("| prod_against (momentum-aligned) | +$13,177 | **+3.7%** |")
    w("| prod_gap_full (model-book gap) | +$12,308 | −3.2% |")
    w("| prod_loss2 (pause after 2 losses) | +$842 | −93% volume |")
    w("")
    w("**prod_against** = only fade when spot agrees on ≥1 horizon — partial step toward BR2 directional.")
    w("")
    if args.trades:
        w("## Entry-level trade decomposition")
        w("")
        for tp in args.trades:
            trades = load_trades(tp)
            if not trades:
                continue
            day = tp.stem.split("_")[0] if "_" in tp.stem else tp.stem
            a = analyze_trades(trades)
            w(f"### {day} (`{tp.name}`)")
            w("")
            w("| bucket | n | NET | hit% |")
            w("|--------|---|-----|------|")
            for k, s in a.items():
                w(f"| {k} | {s['n']} | ${s['net']:+,.0f} | {s['hit']:.1%} |")
            w("")

    args.o.parent.mkdir(parents=True, exist_ok=True)
    args.o.write_text("\n".join(lines) + "\n")
    print(args.o.read_text())


if __name__ == "__main__":
    main()