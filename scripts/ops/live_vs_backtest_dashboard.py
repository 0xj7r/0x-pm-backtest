#!/usr/bin/env python3
"""Rolling live vs backtest dashboard (hit%, $/trade, CI, days-to-significance).

Designed for the JSONL-tail clean era (post fade_live). Uses shadow SUBMITTED
fills matched to shadow-final would_enter + resolution for LIVE P&L @ touch.

Usage (Dublin):
  python3 scripts/ops/live_vs_backtest_dashboard.py
  python3 scripts/ops/live_vs_backtest_dashboard.py --json

From Mac:
  ./scripts/ops/live_vs_backtest_dashboard.sh

Appends JSONL snapshots to --out-dir/live_bt_dashboard.jsonl when not --json.
"""
from __future__ import annotations

import argparse
import glob
import json
import math
import os
import re
from collections import defaultdict
from dataclasses import dataclass, asdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
ISO_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")
TS_RE = re.compile(r"(?P<ts>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
SUBMITTED_RE = re.compile(
    r"shadow SUBMITTED.*slug=(?P<slug>\S+).*accepted=(?P<acc>true|false)"
)
REDEEM_RE = re.compile(r"redeem OK slug=(?P<slug>\S+)")

# LIVE executor began tailing gated shadow-final JSONL at this instant.
GATED_LIVE_SINCE = datetime.fromisoformat("2026-06-17T10:06:57+00:00")


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def curve_fee(rate: float, price: float, shares: float) -> float:
    """Polymarket taker fee: rate * p * (1-p) per share (pm-alpha harness)."""
    return rate * price * (1.0 - price) * shares


def leg_pnl(
    touch: float, won: bool, clip_usd: float, fee_curve_rate: float
) -> tuple[float, float, float]:
    """Return (gross_usd, fee_usd, net_usd) for hold-to-resolution entry."""
    if touch <= 0:
        return 0.0, 0.0, 0.0
    shares = clip_usd / touch
    sps = (1.0 - touch) if won else (-touch)
    gross = sps * shares
    fee = curve_fee(fee_curve_rate, touch, shares)
    return gross, fee, gross - fee


def wilson_ci(wins: int, n: int, z: float = 1.96) -> tuple[float, float]:
    if n == 0:
        return 0.0, 0.0
    p = wins / n
    denom = 1 + z * z / n
    centre = p + z * z / (2 * n)
    margin = z * math.sqrt((p * (1 - p) + z * z / (4 * n)) / n)
    lo = (centre - margin) / denom
    hi = (centre + margin) / denom
    return max(0.0, lo), min(1.0, hi)


@dataclass
class EraStats:
    name: str
    n_resolved: int
    n_open: int
    n_submitted: int
    wins: int
    losses: int
    hit_pct: float
    hit_ci_lo: float
    hit_ci_hi: float
    gross_usd: float
    net_usd: float
    fee_usd: float
    usd_per_trade: float
    gross_usd_per_trade: float
    span_days: float
    usd_per_day: float
    first_ts: str | None
    last_ts: str | None


def index_resolutions(
    entries: list[dict], resolutions: list[dict]
) -> dict[tuple[str, str, int], dict]:
    by_ent: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for e in entries:
        by_ent[(e["slug"], e["side"])].append(e)
    for ents in by_ent.values():
        ents.sort(key=lambda x: x["ts_utc"])

    by_res: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for r in resolutions:
        by_res[(r["slug"], r["side"])].append(r)
    for ress in by_res.values():
        ress.sort(key=lambda x: x["ts_utc"])

    out: dict[tuple[str, str, int], dict] = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = res
    return out


def load_shadow_history(shadow_dir: Path) -> tuple[list[dict], list[dict]]:
    entries: list[dict] = []
    resolutions: list[dict] = []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        with open(fp, encoding="utf-8") as f:
            for line in f:
                try:
                    e = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if e.get("type") == "would_enter":
                    entries.append(e)
                elif e.get("type") == "resolution":
                    resolutions.append(e)
    return entries, resolutions


def load_live_submits(live_log: Path) -> tuple[list[dict], set[str], datetime | None]:
    fills: list[dict] = []
    redeemed: set[str] = set()
    first: datetime | None = None
    if not live_log.is_file():
        return fills, redeemed, first
    for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
        line = strip_ansi(raw)
        iso = ISO_RE.search(line)
        ts: datetime | None = None
        if iso:
            ts = parse_ts(iso.group(1))
        else:
            ts_m = TS_RE.search(line)
            if ts_m:
                ts = parse_ts(ts_m.group("ts") + "Z")
        if ts is None:
            continue
        sm = SUBMITTED_RE.search(line)
        if sm and sm.group("acc") == "true":
            if first is None:
                first = ts
            fills.append({"ts": ts, "slug": sm.group("slug")})
        rm = REDEEM_RE.search(line)
        if rm:
            redeemed.add(rm.group("slug"))
    return fills, redeemed, first


def match_legs(
    fills: list[dict],
    entries: list[dict],
    res_idx: dict[tuple[str, str, int], dict],
    redeemed: set[str],
    clip_usd: float,
    fee_curve_rate: float = 0.07,
    *,
    since: datetime | None = None,
    until: datetime | None = None,
    gated_live_only: bool | None = None,
) -> list[dict]:
    by_slug: dict[str, list[dict]] = defaultdict(list)
    for e in entries:
        by_slug[e["slug"]].append(e)
    for ents in by_slug.values():
        ents.sort(key=lambda x: x["ts_utc"])

    legs: list[dict] = []
    seen: set[tuple[str, int]] = set()
    for fill in fills:
        t = fill["ts"]
        if since and t < since:
            continue
        if until and t >= until:
            continue
        if gated_live_only is True and t < GATED_LIVE_SINCE:
            continue
        if gated_live_only is False and t >= GATED_LIVE_SINCE:
            continue

        slug = fill["slug"]
        fe = t.timestamp()
        best = None
        best_dt = 1e18
        for ent in by_slug.get(slug, []):
            dt = abs(parse_ts(ent["ts_utc"]).timestamp() - fe)
            if dt <= 120 and dt < best_dt:
                best_dt = dt
                best = ent
        if not best:
            continue
        clip = int(best.get("clip", 1))
        key = (slug, clip)
        if key in seen:
            continue
        seen.add(key)

        touch = float(best.get("touch_price") or 0)
        res = res_idx.get((slug, best["side"], clip))
        won: bool | None = None
        gross_pnl: float | None = None
        fee: float | None = None
        net_pnl: float | None = None
        if slug in redeemed and res:
            won = bool(res.get("won"))
            gross_pnl, fee, net_pnl = leg_pnl(touch, won, clip_usd, fee_curve_rate)
        legs.append(
            {
                "ts": t,
                "slug": slug,
                "side": best["side"],
                "clip": clip,
                "touch": touch,
                "won": won,
                "gross_pnl": gross_pnl,
                "fee": fee,
                "net_pnl": net_pnl,
                "pnl": net_pnl,
            }
        )
    return legs


def ref_legs_in_window(
    entries: list[dict],
    resolutions: list[dict],
    res_idx: dict[tuple[str, str, int], dict],
    clip_usd: float,
    since: datetime,
    until: datetime | None = None,
    fee_curve_rate: float = 0.07,
) -> list[dict]:
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

    legs: list[dict] = []
    for (slug, side), ents in by_ss_ent.items():
        for ent, res in zip(ents, by_ss_res.get((slug, side), [])):
            t = parse_ts(res["ts_utc"])
            if t < since:
                continue
            if until and t >= until:
                continue
            touch = float(ent.get("touch_price") or 0)
            won = bool(res.get("won"))
            gross_pnl, fee, net_pnl = leg_pnl(touch, won, clip_usd, fee_curve_rate)
            legs.append(
                {
                    "ts": t,
                    "won": won,
                    "gross_pnl": gross_pnl,
                    "fee": fee,
                    "net_pnl": net_pnl,
                    "pnl": net_pnl,
                }
            )
    return legs


def summarize(name: str, legs: list[dict]) -> EraStats:
    resolved = [l for l in legs if l.get("net_pnl") is not None]
    open_n = len(legs) - len(resolved)
    wins = sum(1 for l in resolved if l.get("won"))
    losses = len(resolved) - wins
    n = len(resolved)
    hit = 100.0 * wins / n if n else 0.0
    lo, hi = wilson_ci(wins, n)
    gross = sum(float(l.get("gross_pnl") or 0) for l in resolved)
    fee = sum(float(l.get("fee") or 0) for l in resolved)
    net = sum(float(l["net_pnl"]) for l in resolved)
    upt = net / n if n else 0.0
    gupt = gross / n if n else 0.0
    ts_list = [l["ts"] for l in legs if isinstance(l.get("ts"), datetime)]
    span = 0.0
    first_s = last_s = None
    if len(ts_list) >= 2:
        span = max((max(ts_list) - min(ts_list)).total_seconds() / 86400, 1 / 24)
        first_s = min(ts_list).isoformat()
        last_s = max(ts_list).isoformat()
    elif ts_list:
        span = 1 / 24
        first_s = last_s = ts_list[0].isoformat()
    return EraStats(
        name=name,
        n_resolved=n,
        n_open=open_n,
        n_submitted=len(legs),
        wins=wins,
        losses=losses,
        hit_pct=hit,
        hit_ci_lo=100 * lo,
        hit_ci_hi=100 * hi,
        gross_usd=gross,
        net_usd=net,
        fee_usd=fee,
        usd_per_trade=upt,
        gross_usd_per_trade=gupt,
        span_days=span,
        usd_per_day=net / span if span else 0.0,
        first_ts=first_s,
        last_ts=last_s,
    )


def load_backtest_tsv(path: Path, variant: str | None = None) -> list[dict]:
    if not path.is_file():
        return []
    rows = []
    lines = path.read_text().splitlines()
    if not lines:
        return []
    has_variant = "variant" in lines[0]
    for i, line in enumerate(lines):
        if i == 0 or not line.strip():
            continue
        parts = line.split("\t")
        if has_variant and len(parts) >= 6:
            row_var = parts[1]
            if variant and row_var != variant:
                continue
            rows.append(
                {
                    "date": parts[0],
                    "variant": row_var,
                    "n_trades": int(parts[2]),
                    "net_usd": float(parts[3]),
                    "hit_pct": float(parts[4]),
                }
            )
        elif len(parts) >= 4:
            rows.append(
                {
                    "date": parts[0],
                    "variant": "base",
                    "n_trades": int(parts[1]),
                    "net_usd": float(parts[2]),
                    "hit_pct": float(parts[3]),
                }
            )
    if variant:
        rows = [r for r in rows if r.get("variant") == variant]
    return rows


def load_fill_realization_summary(out_dir: Path) -> dict | None:
    fp = out_dir / "fill_realization_audit.jsonl"
    if not fp.is_file():
        return None
    last = None
    for line in fp.read_text().splitlines():
        if line.strip():
            try:
                last = json.loads(line)
            except json.JSONDecodeError:
                continue
    return last


def backtest_aggregate(rows: list[dict]) -> dict:
    if not rows:
        return {}
    n = sum(r["n_trades"] for r in rows)
    net = sum(r["net_usd"] for r in rows)
    w_hit = sum(r["hit_pct"] * r["n_trades"] for r in rows) / n if n else 0.0
    return {
        "n_trades": n,
        "net_usd": net,
        "hit_pct": w_hit,
        "usd_per_trade": net / n if n else 0.0,
        "days": len(rows),
        "usd_per_day": net / len(rows) if rows else 0.0,
    }


def pnl_std(pnls: list[float]) -> float:
    if len(pnls) < 2:
        return 35.0
    m = sum(pnls) / len(pnls)
    var = sum((x - m) ** 2 for x in pnls) / (len(pnls) - 1)
    return math.sqrt(var)


def trades_to_detect_mean_shift(
    live_pnls: list[float],
    target_per_trade: float,
    alpha: float = 0.05,
    power: float = 0.8,
) -> int | None:
    """Rough n for one-sided test: live mean < target (backtest) by $2/trade."""
    if not live_pnls:
        return None
    n = len(live_pnls)
    live_mean = sum(live_pnls) / n
    s = pnl_std(live_pnls)
    delta = target_per_trade - live_mean
    if delta <= 2.0:
        return 0
    z_alpha = 1.645
    z_beta = 0.84
    need = ((z_alpha + z_beta) * s / (delta - 2.0)) ** 2
    return max(0, int(math.ceil(need - n)))


def trades_to_bound_hit_rate(n: int, wins: int, target_hit: float, margin: float = 0.04) -> int:
    """Extra trades to narrow Wilson CI to ±margin around target (heuristic)."""
    if n == 0:
        return 500
    lo, hi = wilson_ci(wins, n)
    width = hi - lo
    if width <= 2 * margin:
        return 0
    # width ~ 2*z*sqrt(p(1-p)/n) => n scales with 1/width^2
    p = wins / n if n else 0.5
    current_w = width
    want_w = 2 * margin
    scale = (current_w / want_w) ** 2
    need_n = int(math.ceil(n * scale))
    return max(0, need_n - n)


def parity_submitted_recent(
    shadow_jsonl: Path, live_log: Path, since_hours: float
) -> dict:
    cutoff = datetime.now(timezone.utc).timestamp() - since_hours * 3600
    refs: list[dict] = []
    if shadow_jsonl.is_file():
        for line in shadow_jsonl.read_text(encoding="utf-8", errors="replace").splitlines():
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            if e.get("type") != "would_enter":
                continue
            ts = e.get("ts_utc", "")
            epoch = parse_ts(ts).timestamp() if ts else 0.0
            if epoch < cutoff:
                continue
            refs.append({"slug": e["slug"], "side": e["side"], "clip": int(e.get("clip", 1)), "epoch": epoch})

    subs: list[dict] = []
    if live_log.is_file():
        for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
            line = strip_ansi(raw)
            iso = ISO_RE.search(line)
            if not iso:
                continue
            epoch = parse_ts(iso.group(1)).timestamp()
            if epoch < cutoff:
                continue
            m = SUBMITTED_RE.search(line)
            if m and m.group("acc") == "true":
                subs.append({"slug": m.group("slug"), "epoch": epoch})

    matched_ref: set[int] = set()
    orphans = 0
    for sub in subs:
        ok = False
        for i, ref in enumerate(refs):
            if i in matched_ref:
                continue
            if ref["slug"] == sub["slug"] and abs(ref["epoch"] - sub["epoch"]) <= 120:
                matched_ref.add(i)
                ok = True
                break
        if not ok:
            orphans += 1
    missed = len(refs) - len(matched_ref)
    return {
        "since_hours": since_hours,
        "ref_entries": len(refs),
        "live_submitted": len(subs),
        "matched": len(matched_ref),
        "orphans": orphans,
        "missed_ref": missed,
    }


def fmt_era(e: EraStats) -> str:
    return (
        f"{e.name}\n"
        f"  resolved: {e.n_resolved}  open: {e.n_open}  submits: {e.n_submitted}\n"
        f"  hit: {e.hit_pct:.1f}%  CI [{e.hit_ci_lo:.1f}%, {e.hit_ci_hi:.1f}%]  "
        f"({e.wins}W/{e.losses}L)\n"
        f"  GROSS: ${e.gross_usd:+,.0f}  fees: ${e.fee_usd:,.0f}  "
        f"NET: ${e.net_usd:+,.0f}\n"
        f"  ${e.usd_per_trade:+.2f}/trade net  ${e.gross_usd_per_trade:+.2f}/trade gross  "
        f"${e.usd_per_day:+,.0f}/day net over {e.span_days:.1f}d"
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", default=os.path.expanduser("~/data/pm-alpha/shadow-final"))
    ap.add_argument("--live-log", default=os.path.expanduser("~/data/pm-alpha/shadow_exec_tail.log"))
    ap.add_argument("--clip-usd", type=float, default=50.0)
    ap.add_argument(
        "--fee-curve-rate",
        type=float,
        default=0.07,
        help="Polymarket taker fee curve rate (fee = rate * p * (1-p) per share)",
    )
    ap.add_argument("--tz", default="Europe/Dublin")
    ap.add_argument(
        "--clean-since",
        default="",
        help="UTC ISO start of clean JSONL era (default: first SUBMITTED)",
    )
    ap.add_argument(
        "--backtest-baseline-tsv",
        default="",
        help="june_baseline_daily/daily.tsv (ungated replay)",
    )
    ap.add_argument(
        "--backtest-gated-tsv",
        default="",
        help="june_gated_daily/daily.tsv (mom30+ask0.45 replay)",
    )
    ap.add_argument(
        "--verify-hit",
        type=float,
        default=56.8,
        help="Long-run VERIFY ungated hit%% benchmark",
    )
    ap.add_argument(
        "--verify-upt",
        type=float,
        default=5.25,
        help="Long-run VERIFY $/trade benchmark ($26026/4952)",
    )
    ap.add_argument("--parity-hours", type=float, default=4.0)
    ap.add_argument("--out-dir", default=os.path.expanduser("~/data/pm-alpha/week_monitor"))
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    tz = ZoneInfo(args.tz)
    shadow_dir = Path(args.shadow_dir)
    live_log = Path(args.live_log)
    entries, resolutions = load_shadow_history(shadow_dir)
    res_idx = index_resolutions(entries, resolutions)
    fills, redeemed, first_submit = load_live_submits(live_log)

    if args.clean_since:
        clean_since = parse_ts(args.clean_since.replace(" ", "T") + "Z" if "T" not in args.clean_since else args.clean_since)
        if clean_since.tzinfo is None:
            clean_since = clean_since.replace(tzinfo=timezone.utc)
    elif first_submit:
        clean_since = first_submit
    else:
        clean_since = datetime.now(timezone.utc) - timedelta(days=1)

    fee_rate = args.fee_curve_rate
    live_clean = match_legs(
        fills, entries, res_idx, redeemed, args.clip_usd, fee_rate, since=clean_since
    )
    live_ungated = match_legs(
        fills,
        entries,
        res_idx,
        redeemed,
        args.clip_usd,
        fee_rate,
        since=clean_since,
        gated_live_only=False,
    )
    live_gated = match_legs(
        fills,
        entries,
        res_idx,
        redeemed,
        args.clip_usd,
        fee_rate,
        since=max(clean_since, GATED_LIVE_SINCE),
        gated_live_only=True,
    )
    ref_clean = ref_legs_in_window(
        entries, resolutions, res_idx, args.clip_usd, clean_since, fee_curve_rate=fee_rate
    )
    ref_gated = ref_legs_in_window(
        entries, resolutions, res_idx, args.clip_usd, GATED_LIVE_SINCE, fee_curve_rate=fee_rate
    )

    eras = [
        summarize("LIVE clean (all)", live_clean),
        summarize("LIVE ungated follow", live_ungated),
        summarize("LIVE gated follow", live_gated),
        summarize("REF shadow (since clean)", ref_clean),
        summarize("REF gated (since 10:07 UTC)", ref_gated),
    ]

    baseline_rows = load_backtest_tsv(Path(args.backtest_baseline_tsv)) if args.backtest_baseline_tsv else []
    gated_base = (
        load_backtest_tsv(Path(args.backtest_gated_tsv), variant="base")
        if args.backtest_gated_tsv
        else []
    )
    gated_depth25 = (
        load_backtest_tsv(Path(args.backtest_gated_tsv), variant="depth25")
        if args.backtest_gated_tsv
        else []
    )
    gated_stress = (
        load_backtest_tsv(Path(args.backtest_gated_tsv), variant="stress")
        if args.backtest_gated_tsv
        else []
    )
    bt_base = backtest_aggregate(baseline_rows)
    bt_gated = backtest_aggregate(gated_base)
    bt_gated_depth25 = backtest_aggregate(gated_depth25)
    bt_gated_stress = backtest_aggregate(gated_stress)
    fill_real = load_fill_realization_summary(Path(args.out_dir))

    live_pnls = [float(l["net_pnl"]) for l in live_clean if l.get("net_pnl") is not None]
    sig_upt = trades_to_detect_mean_shift(live_pnls, args.verify_upt)
    sig_hit = trades_to_bound_hit_rate(
        eras[0].n_resolved, eras[0].wins, args.verify_hit / 100.0
    )
    legs_per_day = eras[0].n_resolved / max(eras[0].span_days, 0.1)

    shadow_files = sorted(shadow_dir.glob("shadow-*.jsonl"), key=lambda p: p.stat().st_mtime)
    parity = parity_submitted_recent(
        shadow_files[-1] if shadow_files else Path("/dev/null"),
        live_log,
        args.parity_hours,
    )

    payload = {
        "ts_utc": datetime.now(timezone.utc).isoformat(),
        "clean_since_utc": clean_since.isoformat(),
        "gated_live_since_utc": GATED_LIVE_SINCE.isoformat(),
        "clip_usd": args.clip_usd,
        "fee_curve_rate": args.fee_curve_rate,
        "eras": {e.name: asdict(e) for e in eras},
        "backtest_baseline": bt_base,
        "backtest_gated_june": bt_gated,
        "backtest_gated_depth25": bt_gated_depth25,
        "backtest_gated_stress": bt_gated_stress,
        "fill_realization": fill_real,
        "benchmarks": {"verify_hit_pct": args.verify_hit, "verify_usd_per_trade": args.verify_upt},
        "significance": {
            "extra_trades_for_upt_vs_verify": sig_upt,
            "extra_trades_for_hit_ci_pm4pp": sig_hit,
            "resolved_per_day": legs_per_day,
            "eta_days_for_upt": (sig_upt / legs_per_day) if sig_upt and legs_per_day else None,
            "eta_days_for_hit": (sig_hit / legs_per_day) if sig_hit and legs_per_day else None,
        },
        "parity_submitted": parity,
    }

    if args.json:
        print(json.dumps(payload, indent=2))
        return 0

    lines = [
        "=" * 68,
        "LIVE vs BACKTEST DASHBOARD",
        f"Now: {datetime.now(tz).strftime('%Y-%m-%d %H:%M')} {args.tz}  "
        f"({datetime.now(timezone.utc).strftime('%H:%M')} UTC)",
        f"Clean era since: {clean_since.astimezone(tz).strftime('%Y-%m-%d %H:%M %Z')}",
        f"Gated LIVE since: {GATED_LIVE_SINCE.astimezone(tz).strftime('%Y-%m-%d %H:%M %Z')}",
        f"Fee curve rate: {args.fee_curve_rate:g} (entry taker, hold-to-resolution)",
        "=" * 68,
        "",
    ]
    for e in eras:
        lines.append(fmt_era(e))
        lines.append("")

    lines.append("--- Backtest replay (offline tapes) ---")
    if bt_base:
        lines.append(
            f"June ungated ({bt_base.get('days', 0)}d): "
            f"n={bt_base['n_trades']} hit={bt_base['hit_pct']:.1f}% "
            f"NET=${bt_base['net_usd']:+,.0f} ${bt_base['usd_per_trade']:+.2f}/tr "
            f"${bt_base['usd_per_day']:+,.0f}/day"
        )
    else:
        lines.append("June ungated: (no TSV: pass --backtest-baseline-tsv)")
    if bt_gated:
        lines.append(
            f"June gated base ({bt_gated.get('days', 0)}d): "
            f"n={bt_gated['n_trades']} hit={bt_gated['hit_pct']:.1f}% "
            f"NET=${bt_gated['net_usd']:+,.0f} ${bt_gated['usd_per_trade']:+.2f}/tr "
            f"${bt_gated['usd_per_day']:+,.0f}/day  [optimistic fills]"
        )
    else:
        lines.append("June gated base: (no TSV: pass --backtest-gated-tsv)")
    if bt_gated_depth25:
        lines.append(
            f"June gated depth25: "
            f"NET=${bt_gated_depth25['net_usd']:+,.0f} ${bt_gated_depth25['usd_per_trade']:+.2f}/tr "
            f"${bt_gated_depth25['usd_per_day']:+,.0f}/day  [realistic floor]"
        )
    if bt_gated_stress:
        lines.append(
            f"June gated stress:  "
            f"NET=${bt_gated_stress['net_usd']:+,.0f} ${bt_gated_stress['usd_per_trade']:+.2f}/tr "
            f"${bt_gated_stress['usd_per_day']:+,.0f}/day  [pessimistic]"
        )
    if bt_gated and not bt_gated_depth25:
        lines.append("  (run scripts/pipeline/june_gated_daily.sh for depth25/stress rows)")
    lines.append(
        f"VERIFY champion: hit={args.verify_hit:.1f}%  ${args.verify_upt:.2f}/trade  (4,952 tr)"
    )
    e0 = eras[0]
    if bt_gated_depth25 and e0.n_resolved:
        floor = bt_gated_depth25["usd_per_trade"]
        live_upt = e0.usd_per_trade
        flag = "ABOVE" if live_upt >= floor else "BELOW"
        lines.append(
            f"LIVE ${live_upt:+.2f}/tr vs gated depth25 floor ${floor:+.2f}/tr → {flag}"
        )
    lines.append("")

    if fill_real:
        rr = fill_real.get("realization_live_over_ref") or {}
        sm = fill_real.get("submit_success_pct")
        lines.append("--- Fill realization (latest audit) ---")
        if sm is not None:
            lines.append(
                f"Submit success: {sm:.1f}%  "
                f"({fill_real.get('submitted', 0)} ok / {fill_real.get('submit_attempts', 0)} attempts)"
            )
        if rr.get("median") is not None:
            lines.append(
                f"LIVE touch / REF touch: median={rr['median']:.4f}  "
                f"(~1.0 = decision parity; venue fill not logged yet)"
            )
        lines.append("")

    lines.append("--- Significance (heuristic, 95%) ---")
    lines.append(
        f"LIVE hit CI vs VERIFY {args.verify_hit:.1f}%: "
        f"[{e0.hit_ci_lo:.1f}%, {e0.hit_ci_hi:.1f}%]: "
        f"need ~{sig_hit or 0} more resolved legs to ±4pp"
    )
    lines.append(
        f"LIVE ${e0.usd_per_trade:+.2f}/tr vs VERIFY ${args.verify_upt:.2f}/tr: "
        f"need ~{sig_upt or 0} more legs to detect ${e0.usd_per_trade:+.0f} vs target"
    )
    if legs_per_day > 0:
        if sig_upt:
            lines.append(f"  ETA @ {legs_per_day:.0f} res/day: ~{sig_upt / legs_per_day:.1f} days ($/tr)")
        if sig_hit:
            lines.append(f"  ETA @ {legs_per_day:.0f} res/day: ~{sig_hit / legs_per_day:.1f} days (hit CI)")
    lines.append("")

    lines.append(f"--- Parity SUBMITTED (last {args.parity_hours:g}h) ---")
    lines.append(
        f"REF would_enter={parity['ref_entries']}  LIVE submitted={parity['live_submitted']}  "
        f"matched={parity['matched']}  orphans={parity['orphans']}  missed_ref={parity['missed_ref']}"
    )
    lines.append("")
    lines.append("Note: backtest hit% often > live REF: compare $/tr & slug parity before verdict.")

    text = "\n".join(lines)
    print(text)

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    (out_dir / "live_bt_dashboard.log").open("a").write(text + "\n\n")
    with (out_dir / "live_bt_dashboard.jsonl").open("a") as f:
        f.write(json.dumps(payload) + "\n")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())