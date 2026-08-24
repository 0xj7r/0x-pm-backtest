#!/usr/bin/env python3
"""Historical market-conditions analysis on shadow-final labeled entries.

Joins would_enter telemetry to resolutions, enriches with daily spot regime
(Binance 1m klines), and reports where fade wins/loses under which conditions.

Usage (Dublin):
  python3 scripts/research/shadow_day_regime.py --start 2026-06-14 --end 2026-06-20
  python3 scripts/research/shadow_market_conditions.py \\
    --shadow-dir ~/data/pm-alpha/shadow-final \\
    --day-regime ~/data/runs/shadow_features/day_regime.json \\
    --since 2026-06-14 \\
    --out ~/data/runs/shadow_market_conditions/report.md
"""
from __future__ import annotations

import argparse
import glob
import json
import math
import statistics as st
import urllib.request
from collections import defaultdict
from dataclasses import dataclass
from datetime import date, datetime, timedelta, timezone
from functools import lru_cache
from pathlib import Path

EXO_NAMES = [
    "z_signed", "z_abs", "tau_fraction", "sigma_bar", "delta_bps",
    "mom_30s_sigma", "mom_120s_sigma", "mom_300s_sigma", "mom_accel",
    "flow_imbalance_60s", "flow_intensity", "large_adverse",
    "vol_ratio_short_long", "tod_sin", "tod_cos", "base_p_centered",
]
DIR_NAMES = [
    "funding_rate_bps", "oi_delta_5m", "oi_delta_30m", "basis_bps",
    "perp_flow_imbal_60s", "perp_burst_300s", "liq_proxy",
    "spot_flow_imbal_60s", "trend_60s_sigma", "trend_300s_sigma",
    "trend_1800s_sigma", "trend_alignment", "vol_expansion", "tau_fraction",
]
FLOW_COLS = [
    "binance_flow_imbal_5s", "binance_flow_imbal_15s", "binance_flow_imbal_30s",
    "binance_adverse_vol_5s", "binance_adverse_vol_15s", "binance_adverse_vol_30s",
    "basis_d60_bps",
]
SPOT_RET_COLS = [
    "spot_ret_10s_bps", "spot_ret_30s_bps", "spot_ret_60s_bps",
    "spot_ret_120s_bps", "spot_ret_300s_bps", "spot_ret_600s_bps", "spot_ret_900s_bps",
]
CLIP_USD = 50.0


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def load_jsonl_events(shadow_dir: Path) -> tuple[list[dict], list[dict]]:
    entries: list[dict] = []
    resolutions: list[dict] = []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for line in Path(fp).read_text().splitlines():
            if not line.strip():
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("type") == "would_enter":
                entries.append(ev)
            elif ev.get("type") == "resolution":
                resolutions.append(ev)
    return entries, resolutions


def pair_entries(entries: list[dict], resolutions: list[dict]) -> list[dict]:
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
    rows: list[dict] = []
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            pnl = float(res.get("ladder_settle_pnl_usd") or 0)
            if pnl == 0 and res.get("won") is not None:
                # Fallback: won/lost at touch if ladder pnl missing
                touch = float(ent.get("touch_price") or 0.5)
                # `or` would turn a genuine 0.0 notional into CLIP_USD.
                # Since the reset, target_notional is shares * price, so
                # zero is a real value (a strategy that sized to nothing)
                # and papering over it with a plausible $50 would hide it.
                _tn = ent.get("target_notional")
                clip = float(CLIP_USD if _tn is None else _tn)
                shares = clip / touch if touch > 0 else 0
                pnl = shares * (1.0 - touch) if res.get("won") else -clip
            row = {
                "ts_utc": ent["ts_utc"],
                "utc_day": ent["ts_utc"][:10],
                "slug": slug,
                "side": side,
                "clip": int(ent.get("clip", 1)),
                "won": bool(res.get("won")),
                "pnl_usd": pnl,
                "regime": ent.get("regime") or "unknown",
                "touch_price": float(ent.get("touch_price") or 0),
                "p_side": float(ent.get("p_side") or ent.get("p_exo") or 0.5),
                "model_book_gap": float(ent.get("model_book_gap") or 0),
                "sigma_bar_bps": float(ent.get("sigma_bar_bps") or 0),
                "secs_from_open": int(ent.get("secs_from_open") or 0),
            }
            for col in SPOT_RET_COLS:
                v = ent.get(col)
                row[col] = float(v) if v is not None else None
            for col in FLOW_COLS:
                v = ent.get(col)
                row[col] = float(v) if v is not None else None
            exo = ent.get("exo_features") or []
            dirf = ent.get("dir_features") or []
            for i, name in enumerate(EXO_NAMES):
                if i < len(exo):
                    row[f"exo_{name}"] = float(exo[i])
            for i, name in enumerate(DIR_NAMES):
                if i < len(dirf):
                    row[f"dir_{name}"] = float(dirf[i])
            rows.append(row)
    rows.sort(key=lambda r: r["ts_utc"])
    return rows


@lru_cache(maxsize=64)
def fetch_day_klines(day: str) -> list[tuple[int, float]]:
    d = date.fromisoformat(day)
    start_ms = int(datetime(d.year, d.month, d.day, tzinfo=timezone.utc).timestamp() * 1000)
    end_ms = start_ms + 86_400_000
    out: list[tuple[int, float]] = []
    cursor = start_ms
    while cursor < end_ms:
        url = (
            "https://api.binance.com/api/v3/klines?"
            f"symbol=BTCUSDT&interval=1m&startTime={cursor}&endTime={end_ms}&limit=1000"
        )
        with urllib.request.urlopen(url, timeout=30) as resp:
            kl = json.loads(resp.read())
        if not kl:
            break
        for k in kl:
            out.append((int(k[0]), float(k[4])))
        cursor = int(kl[-1][0]) + 60_000
        if len(kl) < 1000:
            break
    return out


def day_spot_stats(day: str) -> dict | None:
    klines = fetch_day_klines(day)
    if len(klines) < 30:
        return None
    px = [p for _, p in klines]
    rets = [(px[i] / px[i - 1] - 1.0) for i in range(1, len(px))]
    ret_bps = (px[-1] / px[0] - 1.0) * 1e4
    abs_1m = [abs(r) * 1e4 for r in rets]
    rv = math.sqrt(sum(r * r for r in rets)) * 1e4
    rng = (max(px) / min(px) - 1.0) * 1e4
    signs = [1 if r > 0 else (-1 if r < 0 else 0) for r in rets]
    flips = sum(1 for i in range(1, len(signs)) if signs[i] * signs[i - 1] < 0)
    path_len = sum(abs_1m) or 1.0
    trend_eff = abs(ret_bps) / path_len
    return {
        "ret_bps": ret_bps,
        "abs_ret_bps": abs(ret_bps),
        "rv_bps": rv,
        "range_bps": rng,
        "sign_flips": flips,
        "trend_eff": trend_eff,
    }


def classify_day_regime(sp: dict) -> str:
    if sp["trend_eff"] >= 0.35 and sp["abs_ret_bps"] >= 40:
        return "directional_trend"
    if sp["sign_flips"] >= 400 and sp["trend_eff"] < 0.15:
        return "chop_whipsaw"
    if sp["rv_bps"] >= 80:
        return "high_vol"
    if sp["rv_bps"] < 35:
        return "low_vol"
    return "mixed"


def load_day_regime(path: Path | None, days: set[str]) -> dict[str, dict]:
    out: dict[str, dict] = {}
    if path and path.is_file():
        raw = json.loads(path.read_text(encoding="utf-8"))
        for day, row in raw.items():
            out[day] = row
    for day in sorted(days):
        if day in out:
            continue
        sp = day_spot_stats(day)
        if sp is None:
            continue
        sp["day_regime"] = classify_day_regime(sp)
        out[day] = sp
    return out


@dataclass
class GroupStats:
    n: int = 0
    wins: int = 0
    pnl: float = 0.0

    def add(self, won: bool, pnl: float) -> None:
        self.n += 1
        self.wins += int(won)
        self.pnl += pnl

    @property
    def hit(self) -> float:
        return self.wins / self.n if self.n else 0.0


def smd(a: list[float], b: list[float]) -> float:
    if len(a) < 3 or len(b) < 3:
        return float("nan")
    ma, mb = st.mean(a), st.mean(b)
    sa = st.pstdev(a) or 1e-9
    sb = st.pstdev(b) or 1e-9
    sp = math.sqrt(0.5 * (sa * sa + sb * sb))
    return (ma - mb) / sp if sp > 1e-12 else 0.0


def rank_features(rows: list[dict], feature_cols: list[str]) -> list[tuple[str, float, float, float]]:
    wins = [r for r in rows if r["won"]]
    losses = [r for r in rows if not r["won"]]
    ranked: list[tuple[str, float, float, float]] = []
    for col in feature_cols:
        wv = [r[col] for r in wins if r.get(col) is not None and math.isfinite(r[col])]
        lv = [r[col] for r in losses if r.get(col) is not None and math.isfinite(r[col])]
        if len(wv) < 5 or len(lv) < 5:
            continue
        ranked.append((col, smd(lv, wv), st.mean(wv), st.mean(lv)))
    ranked.sort(key=lambda x: abs(x[1]), reverse=True)
    return ranked


def group_pnl(rows: list[dict], key: str) -> dict[str, GroupStats]:
    out: dict[str, GroupStats] = defaultdict(GroupStats)
    for r in rows:
        out[str(r.get(key) or "unknown")].add(r["won"], r["pnl_usd"])
    return dict(out)


def fmt_money(v: float) -> str:
    return f"${v:+,.0f}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--day-regime", default=None)
    ap.add_argument("--since", default="2026-06-14")
    ap.add_argument("--until", default=None)
    ap.add_argument("--out", default="data/runs/shadow_market_conditions/report.md")
    ap.add_argument("--clip-usd", type=float, default=CLIP_USD)
    args = ap.parse_args()

    shadow_dir = Path(args.shadow_dir)
    entries, resolutions = load_jsonl_events(shadow_dir)
    rows = pair_entries(entries, resolutions)
    since = parse_ts(args.since + "T00:00:00Z")
    until = parse_ts((args.until or "2099-01-01") + "T23:59:59Z")
    rows = [r for r in rows if since <= parse_ts(r["ts_utc"]) <= until]

    days = {r["utc_day"] for r in rows}
    day_regime = load_day_regime(
        Path(args.day_regime) if args.day_regime else None, days
    )
    for r in rows:
        dr = day_regime.get(r["utc_day"], {})
        r["day_regime"] = dr.get("day_regime", "unknown")
        r["day_trend_eff"] = dr.get("trend_eff")
        r["day_rv_bps"] = dr.get("rv_bps")

    total = GroupStats()
    for r in rows:
        total.add(r["won"], r["pnl_usd"])

    by_day = group_pnl(rows, "utc_day")
    by_regime = group_pnl(rows, "regime")
    by_day_regime = group_pnl(rows, "day_regime")

    # Period split: good tape vs drawdown window
    good = [r for r in rows if r["utc_day"] <= "2026-06-16"]
    bad = [r for r in rows if "2026-06-17" <= r["utc_day"] <= "2026-06-19"]

    feature_cols = (
        SPOT_RET_COLS
        + [c for c in FLOW_COLS]
        + [f"exo_{n}" for n in EXO_NAMES]
        + [f"dir_{n}" for n in DIR_NAMES]
    )
    feat_rank = rank_features(rows, feature_cols)
    feat_rank_bad = rank_features(bad, feature_cols) if bad else []

    def spot_against_side(r: dict, horizon: str = "spot_ret_120s_bps", thresh: float = 2.0) -> bool:
        ret = r.get(horizon)
        if ret is None:
            return False
        if r["side"] == "up":
            return ret > thresh
        return ret < -thresh

    for r in rows:
        r["spot_against_120s"] = spot_against_side(r, "spot_ret_120s_bps", 2.0)
        r["spot_against_300s"] = spot_against_side(r, "spot_ret_300s_bps", 5.0)

    regime_known = [r for r in rows if r["regime"] != "unknown"]
    bad_known = [r for r in bad if r["regime"] != "unknown"]

    # Counterfactual skips (regime labels match pm_alpha::regime)
    skip_high_flip = [r for r in rows if r["regime"] != "expanded_high_flip"]
    skip_calm = [r for r in rows if r["regime"] != "calm_low_vol"]
    skip_mixed = [r for r in rows if r["regime"] != "expanded_mixed"]
    skip_spot_against = [r for r in rows if not r["spot_against_120s"]]

    def cf_stats(subset: list[dict]) -> GroupStats:
        g = GroupStats()
        for r in subset:
            g.add(r["won"], r["pnl_usd"])
        return g

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    lines: list[str] = []
    w = lines.append

    w("# Shadow market conditions report")
    w("")
    w(f"Generated from `{shadow_dir}` | since `{args.since}` | n={total.n} trades")
    w("")
    w("## Headline")
    w("")
    w(f"| Metric | Value |")
    w(f"|--------|-------|")
    w(f"| Net P&L (@ ${args.clip_usd:.0f}/clip) | {fmt_money(total.pnl)} |")
    w(f"| Hit rate | {total.hit:.1%} |")
    w(f"| Trades | {total.n} |")
    if good:
        g = cf_stats(good)
        w(f"| Jun 14–16 (good tape) | {fmt_money(g.pnl)} on {g.n} trades ({g.hit:.1%} hit) |")
    if bad:
        b = cf_stats(bad)
        w(f"| Jun 17–19 (drawdown) | {fmt_money(b.pnl)} on {b.n} trades ({b.hit:.1%} hit) |")
    w("")

    w("## Daily P&L + spot regime")
    w("")
    w("| Day | Trades | Hit | P&L | day_regime | trend_eff | rv_bps |")
    w("|-----|--------|-----|-----|------------|-----------|--------|")
    for day in sorted(by_day):
        s = by_day[day]
        dr = day_regime.get(day, {})
        w(
            f"| {day} | {s.n} | {s.hit:.0%} | {fmt_money(s.pnl)} | "
            f"{dr.get('day_regime', '?')} | {dr.get('trend_eff', 0):.2f} | {dr.get('rv_bps', 0):.0f} |"
        )
    w("")

    w("## Per-decision regime (30m path at entry)")
    w("")
    w(f"Regime field present on {len(regime_known)}/{len(rows)} entries (older JSONL rows lack it).")
    w("")
    w("| Regime | Trades | Hit | P&L |")
    w("|--------|--------|-----|-----|")
    for reg, s in sorted(by_regime.items(), key=lambda x: x[1].pnl):
        w(f"| {reg} | {s.n} | {s.hit:.0%} | {fmt_money(s.pnl)} |")
    w("")

    if regime_known:
        w("## Regime-known subset only (post telemetry deploy)")
        w("")
        rk = cf_stats(regime_known)
        w(f"- n={rk.n} | hit={rk.hit:.1%} | P&L={fmt_money(rk.pnl)}")
        by_rk = group_pnl(regime_known, "regime")
        w("")
        w("| Regime | Trades | Hit | P&L |")
        w("|--------|--------|-----|-----|")
        for reg, s in sorted(by_rk.items(), key=lambda x: x[1].pnl):
            w(f"| {reg} | {s.n} | {s.hit:.0%} | {fmt_money(s.pnl)} |")
        if bad_known:
            bk = cf_stats(bad_known)
            w("")
            w(f"Jun 17–19 regime-known: n={bk.n} | hit={bk.hit:.1%} | P&L={fmt_money(bk.pnl)}")
            by_bad = group_pnl(bad_known, "regime")
            w("")
            w("| Regime | Trades | Hit | P&L |")
            w("|--------|--------|-----|-----|")
            for reg, s in sorted(by_bad.items(), key=lambda x: x[1].pnl):
                w(f"| {reg} | {s.n} | {s.hit:.0%} | {fmt_money(s.pnl)} |")
        w("")

    w("## Daily spot regime bucket")
    w("")
    w("| day_regime | Trades | Hit | P&L |")
    w("|------------|--------|-----|-----|")
    for reg, s in sorted(by_day_regime.items(), key=lambda x: x[1].pnl):
        w(f"| {reg} | {s.n} | {s.hit:.0%} | {fmt_money(s.pnl)} |")
    w("")

    w("## Gate counterfactuals (historical replay)")
    w("")
    w("| Filter | Trades | Hit | P&L | Δ vs baseline |")
    w("|--------|--------|-----|-----|---------------|")
    for label, subset in [
        ("baseline", rows),
        ("skip expanded_high_flip", skip_high_flip),
        ("skip calm_low_vol", skip_calm),
        ("skip expanded_mixed", skip_mixed),
        ("prod gates (skip calm + skip mixed)", [r for r in rows if r["regime"] not in ("calm_low_vol", "expanded_mixed")]),
        ("skip spot against 120s (|ret|>2bps wrong way)", skip_spot_against),
    ]:
        s = cf_stats(subset)
        delta = s.pnl - total.pnl
        w(f"| {label} | {s.n} | {s.hit:.0%} | {fmt_money(s.pnl)} | {fmt_money(delta)} |")
    w("")

    w("## Loser separation (SMD: losers vs winners, full sample)")
    w("")
    w("Positive SMD = higher on losers (fade-toxic conditions).")
    w("")
    w("| Feature | SMD | mean(win) | mean(loss) |")
    w("|---------|-----|-----------|------------|")
    for col, sd, mw, ml in feat_rank[:20]:
        w(f"| {col} | {sd:+.2f} | {mw:.3f} | {ml:.3f} |")
    w("")

    if feat_rank_bad:
        w("## Loser separation: Jun 17–19 drawdown only")
        w("")
        w("| Feature | SMD | mean(win) | mean(loss) |")
        w("|---------|-----|-----------|------------|")
        for col, sd, mw, ml in feat_rank_bad[:15]:
            w(f"| {col} | {sd:+.2f} | {mw:.3f} | {ml:.3f} |")
        w("")

    # Flow field coverage
    n_flow = sum(1 for r in rows if r.get("binance_adverse_vol_30s") is not None)
    w("## Telemetry coverage")
    w("")
    w(f"- Entries with `binance_adverse_vol_30s`: {n_flow}/{len(rows)}")
    w(f"- Entries with non-zero `dir_oi_delta_5m`: "
      f"{sum(1 for r in rows if abs(r.get('dir_oi_delta_5m') or 0) > 1e-6)}/{len(rows)}")
    w("")

    w("## Interpretation notes")
    w("")
    w("- **expanded_high_flip** = high vol + whipsaw on the 30m spot path at entry; check if this bucket dominates drawdown days.")
    w("- **day_regime** = full UTC-day spot character (trend vs chop); use to route BR2 on trend days.")
    w("- Large positive SMD on `spot_ret_*` / adverse flow = fade enters against recent spot impulse.")
    w("- Re-run after enriched telemetry warms up to refresh flow/OI ranks.")
    w("")

    out_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"wrote {out_path} trades={total.n} pnl={total.pnl:+.0f}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())