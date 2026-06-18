#!/usr/bin/env python3
"""Audit consecutive-loss clusters in shadow-final JSONL + gate counterfactuals.

Joins would_enter telemetry (spot_ret_30s_bps, regime, p_side, touch) with
resolutions, enriches with Binance 1m momentum at entry, and scores which
gates would have blocked cluster trades.

Usage:
  python3 scripts/directional_cluster_audit.py \\
    --shadow-dir /home/ubuntu/data/pm-alpha/shadow-final \\
    --start 2026-06-18T03:00:00Z --end 2026-06-18T05:00:00Z
"""
from __future__ import annotations

import argparse
import glob
import json
import urllib.request
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path

HORIZONS_S = (30, 60, 300, 600, 900)


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def load_events(shadow_dir: Path) -> tuple[list[dict], list[dict]]:
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
    entries.sort(key=lambda e: e["ts_utc"])
    resolutions.sort(key=lambda r: r["ts_utc"])
    return entries, resolutions


def index_res_by_clip(entries: list[dict], resolutions: list[dict]) -> dict:
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
    out = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = (ent, res)
    return out


@dataclass
class SessionState:
    consec_losses: int = 0
    last_side: str | None = None
    last_won: bool | None = None

    def observe(self, won: bool, side: str) -> None:
        self.last_side = side
        self.last_won = won
        self.consec_losses = 0 if won else self.consec_losses + 1


def side_aligned_30s(side: str, ret_bps: float | None) -> bool | None:
    if ret_bps is None:
        return None
    return ret_bps > 0 if side == "up" else ret_bps < 0


def gate_loss2(state: SessionState) -> bool:
    return state.consec_losses < 2


def gate_flip_misalign_after_win(side: str, state: SessionState, aligned: bool | None) -> bool:
    if state.last_side is None or state.last_won is not True:
        return True
    if side == state.last_side:
        return True
    return aligned is not False


def gate_trend_against(side: str, ret_300s: float | None, thresh_bps: float = 8.0) -> bool:
    """Block fade when medium-term spot move strongly favors resolution side."""
    if ret_300s is None:
        return True
    # UP entry (fade) blocked if spot rallied hard; DOWN entry blocked if spot sold off
    if side == "up" and ret_300s > thresh_bps:
        return False
    if side == "down" and ret_300s < -thresh_bps:
        return False
    return True


def gate_trend_against_60(side: str, ret_60s: float | None, thresh_bps: float = 5.0) -> bool:
    if ret_60s is None:
        return True
    if side == "up" and ret_60s > thresh_bps:
        return False
    if side == "down" and ret_60s < -thresh_bps:
        return False
    return True


def gate_high_gap_fav(side: str, p_side: float, touch: float) -> bool:
    """Skip :00-style favourites where model >> book."""
    if p_side > 0.88 and touch < 0.62:
        return False
    return True


def gate_regime(regime: str | None) -> bool:
    return regime != "expanded_high_flip"


def fetch_klines(start_ms: int, end_ms: int) -> list[tuple[int, float]]:
    out: list[tuple[int, float]] = []
    cursor = start_ms
    while cursor < end_ms:
        url = (
            "https://api.binance.com/api/v3/klines?"
            f"symbol=BTCUSDT&interval=1m&startTime={cursor}&endTime={end_ms}&limit=1000"
        )
        with urllib.request.urlopen(url, timeout=20) as resp:
            kl = json.loads(resp.read())
        if not kl:
            break
        for k in kl:
            out.append((int(k[0]), float(k[4])))
        cursor = int(kl[-1][0]) + 60_000
        if len(kl) < 1000:
            break
    return out


def ret_bps(klines: list[tuple[int, float]], end_ms: int, lookback_s: int) -> float | None:
    if not klines:
        return None
    target = end_ms - lookback_s * 1000
    end_px = start_px = None
    for ts, px in klines:
        if ts <= end_ms:
            end_px = px
        if ts <= target:
            start_px = px
    if end_px is None or start_px is None or start_px <= 0:
        return None
    return (end_px / start_px - 1.0) * 10_000.0


@dataclass
class Trade:
    ts: str
    slug: str
    side: str
    clip: int
    won: bool
    pnl: float
    p_side: float
    touch: float
    spot_ret_30s: float | None
    regime: str | None
    ret_60s: float | None = None
    ret_300s: float | None = None
    ret_600s: float | None = None
    ret_900s: float | None = None
    aligned_30s: bool | None = None
    consec_before: int = 0
    gates: dict[str, bool] = field(default_factory=dict)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--start", required=True, help="ISO UTC start (inclusive)")
    ap.add_argument("--end", required=True, help="ISO UTC end (exclusive)")
    ap.add_argument("--context-hours", type=float, default=2.0, help="klines prefetch")
    args = ap.parse_args()

    t0 = parse_ts(args.start)
    t1 = parse_ts(args.end)
    shadow_dir = Path(args.shadow_dir)

    entries, resolutions = load_events(shadow_dir)
    paired = index_res_by_clip(entries, resolutions)

    # klines for enrichment
    pad_ms = int(args.context_hours * 3600 * 1000)
    k_start = int((t0.timestamp() - args.context_hours * 3600) * 1000)
    k_end = int(t1.timestamp() * 1000) + pad_ms
    print(f"Fetching BTC 1m klines {k_start}..{k_end} ...")
    klines = fetch_klines(k_start, k_end)
    print(f"  {len(klines)} bars")

    trades: list[Trade] = []
    state = SessionState()
    for (slug, side, clip), (ent, res) in sorted(
        paired.items(), key=lambda x: x[1][0]["ts_utc"]
    ):
        ts = parse_ts(ent["ts_utc"])
        if ts < t0 or ts >= t1:
            # still advance session state for gates
            won = bool(res.get("won"))
            state.observe(won, side)
            continue
        won = bool(res.get("won"))
        pnl = float(res.get("ladder_settle_pnl_usd") or 0)
        spot30 = ent.get("spot_ret_30s_bps")
        if spot30 is not None:
            spot30 = float(spot30)
        regime = ent.get("regime")
        end_ms = int(ts.timestamp() * 1000)
        r60 = ret_bps(klines, end_ms, 60)
        r300 = ret_bps(klines, end_ms, 300)
        r600 = ret_bps(klines, end_ms, 600)
        r900 = ret_bps(klines, end_ms, 900)
        aligned = side_aligned_30s(side, spot30)
        consec_before = state.consec_losses
        p_side = float(ent.get("p_side", 0))
        touch = float(ent.get("touch_price", 0))

        gates = {
            "loss2": gate_loss2(state),
            "flip_misalign": gate_flip_misalign_after_win(side, state, aligned),
            "loss2_or_flip_misalign": gate_loss2(state)
            and gate_flip_misalign_after_win(side, state, aligned),
            "trend300_8bps": gate_trend_against(side, r300, 8.0),
            "trend60_5bps": gate_trend_against_60(side, r60, 5.0),
            "trend300_15bps": gate_trend_against(side, r300, 15.0),
            "high_gap_fav": gate_high_gap_fav(side, p_side, touch),
            "no_expanded_flip": gate_regime(regime),
            "misalign30": aligned is not False,
            "combo_trend300+loss2": gate_loss2(state) and gate_trend_against(side, r300, 8.0),
        }

        trades.append(
            Trade(
                ts=ent["ts_utc"],
                slug=slug,
                side=side,
                clip=clip,
                won=won,
                pnl=pnl,
                p_side=p_side,
                touch=touch,
                spot_ret_30s=spot30,
                regime=regime,
                ret_60s=r60,
                ret_300s=r300,
                ret_600s=r600,
                ret_900s=r900,
                aligned_30s=aligned,
                consec_before=consec_before,
                gates=gates,
            )
        )
        state.observe(won, side)

    if not trades:
        print("No trades in window.")
        return 0

    print(f"\n=== Cluster audit {args.start} .. {args.end} ===")
    print(f"Trades: {len(trades)}")
    w = sum(1 for t in trades if t.won)
    pnl = sum(t.pnl for t in trades)
    print(f"Baseline: {w}W/{len(trades)-w}L  pnl=${pnl:+.0f}")

    print("\nPer-trade:")
    for t in trades:
        mark = "W" if t.won else "L"
        print(
            f"  {t.ts[:19]} {t.side:4} c{t.clip} {mark} ${t.pnl:+.0f}  "
            f"p={t.p_side:.3f} touch={t.touch:.2f}  "
            f"spot30={t.spot_ret_30s} regime={t.regime}  "
            f"r60={t.ret_60s} r300={t.ret_300s} r600={t.ret_600s}  "
            f"aligned30={t.aligned_30s} consec_before={t.consec_before}"
        )

    print("\nGate counterfactuals (would KEEP trade if True):")
    for gname in trades[0].gates:
        kept = [t for t in trades if t.gates[gname]]
        blocked = [t for t in trades if not t.gates[gname]]
        kept_pnl = sum(t.pnl for t in kept)
        blocked_pnl = sum(t.pnl for t in blocked)
        blocked_losses = sum(1 for t in blocked if not t.won)
        print(
            f"  {gname:28s}  keep={len(kept):2d} block={len(blocked):2d}  "
            f"kept_pnl=${kept_pnl:+.0f}  blocked_losses={blocked_losses}  "
            f"blocked_pnl=${blocked_pnl:+.0f}"
        )

    # streak analysis within window
    streak = 0
    max_streak = 0
    for t in trades:
        if not t.won:
            streak += 1
            max_streak = max(max_streak, streak)
        else:
            streak = 0
    print(f"\nMax consecutive losses in window: {max_streak}")

    # aggregate: how often was spot trending WITH resolution (against our fade)
    against = 0
    for t in trades:
        if t.side == "up" and t.ret_300s is not None and t.ret_300s > 5:
            against += 1
        if t.side == "down" and t.ret_300s is not None and t.ret_300s < -5:
            against += 1
    print(f"Entries fading against 5m trend (|ret300|>5bps): {against}/{len(trades)}")

    # Extended model-book gap gates (anytime, not just :00 open)
    gap_gates = [
        ("open_fav_5s", lambda t, e: not (e <= 5 and t.p_side > 0.90 and t.touch < 0.60)),
        ("gap_p88_touch62", lambda t, e: not (t.p_side > 0.88 and t.touch < 0.62)),
        ("gap_p90_touch60", lambda t, e: not (t.p_side > 0.90 and t.touch < 0.60)),
        ("gap_p85_touch65", lambda t, e: not (t.p_side > 0.85 and t.touch < 0.65)),
        ("gap_gt_0.40", lambda t, e: not (t.p_side - t.touch > 0.40)),
    ]
    import re

    def elapsed_s(tr: Trade) -> int:
        m = re.search(r"-(\d+)$", tr.slug)
        if not m:
            return 999
        open_ts = int(m.group(1))
        ent_ts = parse_ts(tr.ts).timestamp()
        return int(ent_ts - open_ts)

    print("\nModel-book gap gates:")
    for name, fn in gap_gates:
        kept = [t for t in trades if fn(t, elapsed_s(t))]
        blocked = [t for t in trades if not fn(t, elapsed_s(t))]
        print(
            f"  {name:20s} keep={len(kept):2d}(${sum(t.pnl for t in kept):+.0f}) "
            f"block={len(blocked):2d}(${sum(t.pnl for t in blocked):+.0f}, "
            f"{sum(1 for t in blocked if not t.won)}L)"
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())