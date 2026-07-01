#!/usr/bin/env python3
"""Daily UTC spot regime labels for shadow discovery joins.

Computes trend_eff, rv_bps, sign_flips, and a coarse regime tag from Binance
BTC spot agg_trades (same logic as scripts/research/june_regime_compare.py).

Writes JSON sidecar:
  {"2026-06-19": {"trend_eff": 0.22, "rv_bps": 55.1, ...}, ...}

Usage:
  python3 scripts/research/shadow_day_regime.py
  python3 scripts/research/shadow_day_regime.py --start 2026-06-14 --end 2026-06-20 \\
    --out data/runs/shadow_features/day_regime.json
"""
from __future__ import annotations

import argparse
import json
from dataclasses import asdict, dataclass
from datetime import date, datetime, timedelta, timezone
from functools import lru_cache
from pathlib import Path

import math

try:
    import pyarrow.parquet as pq
    HAS_PARQUET = True
except ImportError:
    HAS_PARQUET = False

ROOT = Path(__file__).resolve().parents[2]
SPOT_DIR = ROOT / "data/cache/raw/binance/exchange=binance/channel=agg_trades"
DEFAULT_OUT = ROOT / "data/runs/shadow_features/day_regime.json"


@dataclass
class DaySpot:
    day: str
    ret_bps: float
    abs_ret_bps: float
    rv_bps: float
    range_bps: float
    sign_flips: int
    trend_eff: float


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


def _stats_from_klines(klines: list[tuple[int, float]]) -> DaySpot | None:
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
    day = datetime.fromtimestamp(klines[0][0] / 1000, tz=timezone.utc).date().isoformat()
    return DaySpot(day, ret_bps, abs(ret_bps), rv, rng, flips, trend_eff)


@lru_cache(maxsize=64)
def fetch_day_klines(day: str) -> list[tuple[int, float]]:
    import json
    import urllib.request
    from datetime import datetime, timezone

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


@lru_cache(maxsize=64)
def load_spot_day(day: str) -> DaySpot | None:
    if HAS_PARQUET:
        ddir = SPOT_DIR / f"symbol=BTCUSDT/date={day}"
        if ddir.is_dir():
            files = sorted(ddir.glob("*.parquet"))
            if files:
                tbl = pq.ParquetFile(files[0]).read(columns=["price", "transact_time_ms"])
                rows = sorted(
                    zip(tbl.column("transact_time_ms").to_pylist(), tbl.column("price").to_pylist()),
                    key=lambda x: x[0],
                )
                bucket_ms = 300_000
                buckets: list[float] = []
                if rows:
                    t0, _ = rows[0]
                    last_ts = rows[-1][0]
                    for t in range(int(t0), int(last_ts), bucket_ms):
                        idx = max(i for i, (ts, _) in enumerate(rows) if ts <= t + bucket_ms) if rows else -1
                        if idx >= 0 and float(rows[idx][1]) > 0:
                            buckets.append(float(rows[idx][1]))
                if len(buckets) >= 10:
                    px = buckets
                    rets = [(px[i] / px[i - 1] - 1.0) for i in range(1, len(px))]
                    ret_bps = (px[-1] / px[0] - 1.0) * 1e4
                    abs_1m = [abs(r) * 1e4 for r in rets]
                    rv = math.sqrt(sum(r * r for r in rets)) * 1e4
                    rng = (max(px) / min(px) - 1.0) * 1e4
                    signs = [1 if r > 0 else (-1 if r < 0 else 0) for r in rets]
                    flips = sum(1 for i in range(1, len(signs)) if signs[i] * signs[i - 1] < 0)
                    path_len = sum(abs_1m) or 1.0
                    trend_eff = abs(ret_bps) / path_len
                    return DaySpot(day, ret_bps, abs(ret_bps), rv, rng, flips, trend_eff)
    return _stats_from_klines(fetch_day_klines(day))


def day_range(start: date, end: date) -> list[str]:
    out: list[str] = []
    d = start
    while d <= end:
        out.append(d.isoformat())
        d += timedelta(days=1)
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--start", default="2026-06-01")
    ap.add_argument("--end", default=date.today().isoformat())
    ap.add_argument("--out", default=str(DEFAULT_OUT))
    args = ap.parse_args()

    start = date.fromisoformat(args.start)
    end = date.fromisoformat(args.end)
    sidecar: dict[str, dict] = {}
    for day in day_range(start, end):
        sp = load_spot_day(day)
        if sp is None:
            continue
        row = asdict(sp)
        row["day_regime"] = classify_regime(sp)
        sidecar[day] = row

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(sidecar, indent=2) + "\n", encoding="utf-8")
    print(f"wrote {out_path} days={len(sidecar)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())