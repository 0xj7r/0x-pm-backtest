#!/usr/bin/env python3
"""Day-trading-style momentum features at entry vs loss / loss-cluster outcomes.

Enriches harness trade tapes with multi-horizon spot returns, EMA crossover
state, RSI, and acceleration — then screens which combinations precede
consecutive-loss clusters.

Uses local Binance agg_trades (same source as the backtester). No re-run needed.

Usage:
  python3 scripts/momentum_cluster_screen.py \\
      data/runs/strategy_validation/HOLDOUT_baseline.trades.jsonl
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq

ROOT = Path(__file__).resolve().parents[1]
SPOT_DIR = ROOT / "data/cache/raw/binance/exchange=binance/channel=agg_trades"

HORIZONS_S = (10, 30, 60, 120, 300)
EMA_FAST_S = 30
EMA_SLOW_S = 120
RSI_PERIOD_S = 14


def load_trades(path: Path) -> list[dict]:
    rows = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    return sorted(rows, key=lambda t: int(t["decision_ts_ns"]))


def won(t: dict) -> bool:
    return float(t["pnl"]) > 0


def side_up(t: dict) -> bool:
    return t["side"] == "Yes"


def spot_ret_bps(us: np.ndarray, px: np.ndarray, ts_ns: int, lookback_s: int) -> float | None:
    end = ts_ns // 1000
    start = end - lookback_s * 1_000
    i_end = int(np.searchsorted(us, end, side="right")) - 1
    i_start = int(np.searchsorted(us, start, side="right")) - 1
    if i_end < 0 or i_start < 0 or i_end == i_start:
        return None
    p1, p0 = float(px[i_end]), float(px[i_start])
    if p0 <= 0:
        return None
    return (p1 / p0 - 1.0) * 1e4


def resample_1s(us: np.ndarray, px: np.ndarray, end_us: int, window_s: int) -> np.ndarray:
    """Last price each second over trailing window (oldest→newest)."""
    start_us = end_us - window_s * 1_000_000
    i0 = int(np.searchsorted(us, start_us, side="left"))
    i1 = int(np.searchsorted(us, end_us, side="right"))
    if i1 <= i0:
        return np.array([], dtype=float)
    out = []
    for sec_us in range(start_us, end_us + 1_000_000, 1_000_000):
        idx = int(np.searchsorted(us, sec_us, side="right")) - 1
        if idx >= i0 and px[idx] > 0:
            out.append(float(px[idx]))
    return np.asarray(out, dtype=float)


def ema(series: np.ndarray, span: int) -> float | None:
    if len(series) < span // 2:
        return None
    alpha = 2.0 / (span + 1.0)
    v = series[0]
    for x in series[1:]:
        v = alpha * x + (1 - alpha) * v
    return float(v)


def ema_series(series: np.ndarray, span: int) -> np.ndarray:
    if len(series) == 0:
        return series
    alpha = 2.0 / (span + 1.0)
    out = np.empty(len(series))
    out[0] = series[0]
    for i in range(1, len(series)):
        out[i] = alpha * series[i] + (1 - alpha) * out[i - 1]
    return out


def rsi(series: np.ndarray, period: int = 14) -> float | None:
    if len(series) < period + 1:
        return None
    d = np.diff(series[-(period + 1) :])
    gains = np.maximum(d, 0)
    losses = np.maximum(-d, 0)
    avg_gain = gains.mean()
    avg_loss = losses.mean()
    if avg_loss < 1e-15:
        return 100.0
    rs = avg_gain / avg_loss
    return 100.0 - 100.0 / (1.0 + rs)


@lru_cache(maxsize=32)
def load_spot_day(symbol: str, date: str) -> tuple[np.ndarray, np.ndarray] | None:
    d = SPOT_DIR / f"symbol={symbol}/date={date}"
    files = list(d.glob("*.parquet")) if d.exists() else []
    if not files:
        return None
    t = pq.ParquetFile(files[0]).read(columns=["price", "transact_time_ms"])
    us = t.column("transact_time_ms").to_numpy()
    px = np.asarray(t.column("price").to_pylist(), dtype=float)
    order = np.argsort(us)
    return us[order], px[order]


def ts_to_date(ts_ns: int) -> str:
    from datetime import datetime, timezone

    return datetime.fromtimestamp(ts_ns / 1e9, tz=timezone.utc).strftime("%Y-%m-%d")


@dataclass
class MomFeatures:
    ret_10: float | None = None
    ret_30: float | None = None
    ret_60: float | None = None
    ret_120: float | None = None
    ret_300: float | None = None
    ema_fast: float | None = None
    ema_slow: float | None = None
    ema_cross: str | None = None  # bull / bear / flat
    ema_cross_recent: bool | None = None
    rsi_14: float | None = None
    accel_30_60: float | None = None  # ret_30 - ret_60/2
    aligned_30: bool | None = None  # spot 30s agrees with trade side


def compute_mom(
    us: np.ndarray, px: np.ndarray, ts_ns: int, trade_up: bool
) -> MomFeatures:
    f = MomFeatures()
    f.ret_10 = spot_ret_bps(us, px, ts_ns, 10)
    f.ret_30 = spot_ret_bps(us, px, ts_ns, 30)
    f.ret_60 = spot_ret_bps(us, px, ts_ns, 60)
    f.ret_120 = spot_ret_bps(us, px, ts_ns, 120)
    f.ret_300 = spot_ret_bps(us, px, ts_ns, 300)
    if f.ret_30 is not None and f.ret_60 is not None:
        f.accel_30_60 = f.ret_30 - f.ret_60 / 2.0
    if f.ret_30 is not None:
        bullish = f.ret_30 > 0
        f.aligned_30 = bullish if trade_up else not bullish


def aligned_from_trade(t: dict) -> bool | None:
    if t.get("side_aligned_30s") is not None:
        return bool(t["side_aligned_30s"])
    r = t.get("spot_ret_30s_bps")
    if r is None:
        return None
    r = float(r)
    up = t["side"] == "Yes"
    return r > 0 if up else r < 0

    end_us = ts_ns // 1000
    series = resample_1s(us, px, end_us, max(EMA_SLOW_S + 30, 150))
    if len(series) >= EMA_SLOW_S // 2:
        f.ema_fast = ema(series, EMA_FAST_S)
        f.ema_slow = ema(series, EMA_SLOW_S)
        if f.ema_fast is not None and f.ema_slow is not None:
            diff = f.ema_fast - f.ema_slow
            if abs(diff) < 1e-6:
                f.ema_cross = "flat"
            elif diff > 0:
                f.ema_cross = "bull"
            else:
                f.ema_cross = "bear"
            # Cross in last ~30s?
            if len(series) >= EMA_SLOW_S:
                es = ema_series(series, EMA_FAST_S)
                el = ema_series(series, EMA_SLOW_S)
                recent = es[-30:] - el[-30:]
                signs = np.sign(recent)
                changes = np.where(np.diff(signs) != 0)[0]
                f.ema_cross_recent = len(changes) > 0
        f.rsi_14 = rsi(series, RSI_PERIOD_S)
    return f


def annotate_streaks(trades: list[dict]) -> list[dict]:
    """Add consec_before, starts_cluster, is_second_loss."""
    consec = 0
    out = []
    for t in trades:
        row = dict(t)
        row["_consec_before"] = consec
        row["_starts_cluster"] = consec == 0 and not won(t)
        row["_is_second_loss"] = consec == 1 and not won(t)
        row["_blocked_loss2"] = consec >= 2
        out.append(row)
        consec = 0 if won(t) else consec + 1
    return out


def summarize(sub: list[dict], label: str) -> None:
    n = len(sub)
    if n == 0:
        return
    pnl = sum(float(t["pnl"]) for t in sub)
    hit = sum(1 for t in sub if won(t)) / n
    print(
        f"  {label:40s}  n={n:5d}  NET=${pnl:9,.0f}  "
        f"hit={hit * 100:5.1f}%  $/tr={pnl / n:6.2f}"
    )


def screen_bucket(
    enriched: list[dict],
    key_fn,
    label: str,
    min_n: int = 40,
) -> None:
    by: dict[str, list[dict]] = defaultdict(list)
    for row in enriched:
        k = key_fn(row)
        if k is not None:
            by[k].append(row)
    if not by:
        return
    print(f"\n### {label}")
    base_lr = sum(1 for r in enriched if not won(r)) / len(enriched)
    rows = []
    for k, sub in by.items():
        if len(sub) < min_n:
            continue
        lr = sum(1 for t in sub if not won(t)) / len(sub)
        second = sum(1 for t in sub if t.get("_is_second_loss")) / len(sub)
        rows.append((lr, second, k, len(sub)))
    rows.sort(key=lambda x: -x[0])
    print(f"  baseline loss%={100 * base_lr:.1f}%")
    print(f"  {'bucket':32s}  {'n':>5s}  {'loss%':>6s}  {'2ndLoss%':>8s}  {'lift':>5s}")
    for lr, second, k, n in rows:
        print(
            f"  {k:32s}  {n:5d}  {100 * lr:5.1f}%  {100 * second:7.1f}%  "
            f"{lr / base_lr:5.2f}x"
        )


def main() -> None:
    path = Path(sys.argv[1])
    trades = load_trades(path)
    trades = annotate_streaks(trades)
    print(f"# Momentum cluster screen — {path.name}\n")
    print(f"  trades={len(trades)}")

    spot_cache: dict[str, tuple[np.ndarray, np.ndarray] | None] = {}
    enriched: list[dict] = []
    skipped = 0
    for t in trades:
        ts = int(t["decision_ts_ns"])
        date = ts_to_date(ts)
        if date not in spot_cache:
            spot_cache[date] = load_spot_day("BTCUSDT", date)
        spot = spot_cache[date]
        if spot is None:
            skipped += 1
            continue
        us, px = spot
        mom = compute_mom(us, px, ts, side_up(t))
        tape_aligned = aligned_from_trade(t)
        if tape_aligned is not None:
            mom.aligned_30 = tape_aligned
        row = dict(t)
        row["_mom"] = mom
        enriched.append(row)

    print(f"  enriched={len(enriched)}  skipped_no_spot={skipped}")
    summarize(enriched, "ALL enriched")

    starters = [r for r in enriched if r["_starts_cluster"]]
    second = [r for r in enriched if r["_is_second_loss"]]
    after_win = [
        enriched[i]
        for i in range(1, len(enriched))
        if won(enriched[i - 1]) and enriched[i - 1] is enriched[i - 1]
    ]
    summarize(starters, "cluster-start (1st loss)")
    summarize(second, "2nd consecutive loss")

    print("\n## Momentum at cluster-start vs all trades")
    for name, fn in [
        ("ret_30s bps", lambda r: _bucket_ret(r["_mom"].ret_30)),
        ("ret_60s bps", lambda r: _bucket_ret(r["_mom"].ret_60)),
        ("accel 30-60", lambda r: _bucket_accel(r["_mom"].accel_30_60)),
        ("EMA cross", lambda r: r["_mom"].ema_cross),
        ("EMA cross recent 30s", lambda r: "cross_yes" if r["_mom"].ema_cross_recent else "cross_no"),
        ("RSI 14", lambda r: _bucket_rsi(r["_mom"].rsi_14)),
        ("side-aligned 30s", lambda r: "aligned" if r["_mom"].aligned_30 else "against"),
        ("multi-horizon agree", lambda r: _bucket_agree(r["_mom"])),
    ]:
        screen_bucket(enriched, fn, name)

    print("\n## Combo signals (day-trader style)")
    screen_bucket(
        enriched,
        lambda r: _combo_cross_rsi(r),
        "EMA cross × RSI zone",
        min_n=25,
    )
    screen_bucket(
        enriched,
        lambda r: _combo_align_accel(r),
        "side misalign × negative accel",
        min_n=25,
    )
    screen_bucket(
        after_win,
        lambda r: _combo_after_win(r, enriched),
        "after win: flip × misalign 30s",
        min_n=25,
    )

    print("\n## Loss2-blocked trades — momentum on would-skip legs")
    blocked = [r for r in enriched if r["_blocked_loss2"]]
    summarize(blocked, "blocked by loss2 gate")
    screen_bucket(blocked, lambda r: r["_mom"].ema_cross, "blocked: EMA cross", min_n=20)
    screen_bucket(blocked, lambda r: "against" if not r["_mom"].aligned_30 else "aligned", "blocked: 30s align", min_n=20)

    print("\n## Notes")
    print("- EMA 30s/120s on 1s-resampled BTC last price at decision_ts.")
    print("- '2ndLoss%' = fraction of bucket that is the 2nd leg of a loss cluster.")
    print("- Promote combos with loss lift AND high 2ndLoss% on 1st-loss features.")


def _bucket_ret(v: float | None) -> str | None:
    if v is None:
        return None
    if v < -3:
        return "ret_<-3bps"
    if v < 0:
        return "ret_-3_0"
    if v < 3:
        return "ret_0_3"
    return "ret_>3bps"


def _bucket_accel(v: float | None) -> str | None:
    if v is None:
        return None
    if v < -2:
        return "decel_<-2"
    if v < 2:
        return "accel_flat"
    return "accel_>2"


def _bucket_rsi(v: float | None) -> str | None:
    if v is None:
        return None
    if v < 35:
        return "rsi_oversold"
    if v > 65:
        return "rsi_overbought"
    return "rsi_mid"


def _bucket_agree(m: MomFeatures) -> str | None:
    vals = [m.ret_10, m.ret_30, m.ret_60]
    if any(v is None for v in vals):
        return None
    signs = [np.sign(v) for v in vals]
    if all(s > 0 for s in signs):
        return "all_up"
    if all(s < 0 for s in signs):
        return "all_down"
    return "mixed"


def _combo_cross_rsi(r: dict) -> str | None:
    m = r["_mom"]
    if m.ema_cross is None or m.rsi_14 is None:
        return None
    zone = _bucket_rsi(m.rsi_14)
    return f"{m.ema_cross}_{zone}"


def _combo_align_accel(r: dict) -> str | None:
    m = r["_mom"]
    if m.aligned_30 is None or m.accel_30_60 is None:
        return None
    align = "aligned" if m.aligned_30 else "against"
    acc = "decel" if m.accel_30_60 < -1 else "ok"
    return f"{align}_{acc}"


def _combo_after_win(r: dict, all_rows: list[dict]) -> str | None:
    idx = all_rows.index(r)
    if idx == 0:
        return None
    prev = all_rows[idx - 1]
    flip = "flip" if side_up(prev) != side_up(r) else "same"
    align = "misalign" if not r["_mom"].aligned_30 else "align"
    return f"{flip}_{align}"


if __name__ == "__main__":
    main()