"""Shared helpers for shadow-final labeled entry analysis (stdlib-first)."""
from __future__ import annotations

import glob
import json
import math
import random
import statistics as st
import urllib.request
from collections import defaultdict
from dataclasses import dataclass
from datetime import date, datetime, timezone
from functools import lru_cache
from pathlib import Path
from typing import Callable

CLIP_USD = 50.0

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

# pm_alpha::regime v1 thresholds (regime.rs)
CALM_VOL_BPS = 4.5
TREND_EFFICIENCY = 0.30
HIGH_FLIP = 0.55


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
                touch = float(ent.get("touch_price") or 0.5)
                # `or` would turn a genuine 0.0 notional into CLIP_USD.
                # Since the reset, target_notional is shares * price, so
                # zero is a real value (a strategy that sized to nothing)
                # and papering over it with a plausible $50 would hide it.
                _tn = ent.get("target_notional")
                clip = float(CLIP_USD if _tn is None else _tn)
                pnl = (clip / touch * (1.0 - touch)) if res.get("won") else -clip
            row = {
                "ts_utc": ent["ts_utc"],
                "entry_ms": int(parse_ts(ent["ts_utc"]).timestamp() * 1000),
                "utc_day": ent["ts_utc"][:10],
                "slug": slug,
                "side": side,
                "clip": int(ent.get("clip", 1)),
                "won": bool(res.get("won")),
                "pnl_usd": pnl,
                "regime_logged": ent.get("regime"),
                "touch_price": float(ent.get("touch_price") or 0),
                "p_side": float(ent.get("p_side") or ent.get("p_exo") or 0.5),
                "p_exo": float(ent.get("p_exo") or 0.5),
                "model_book_gap": float(ent.get("model_book_gap") or 0),
                "sigma_bar_bps": float(ent.get("sigma_bar_bps") or 0),
                "secs_from_open": int(ent.get("secs_from_open") or 0),
            }
            for col in SPOT_RET_COLS + FLOW_COLS:
                v = ent.get(col)
                row[col] = float(v) if v is not None else None
            for i, name in enumerate(EXO_NAMES):
                if i < len(ent.get("exo_features") or []):
                    row[f"exo_{name}"] = float(ent["exo_features"][i])
            for i, name in enumerate(DIR_NAMES):
                if i < len(ent.get("dir_features") or []):
                    row[f"dir_{name}"] = float(ent["dir_features"][i])
            rows.append(row)
    rows.sort(key=lambda r: r["ts_utc"])
    return rows


@lru_cache(maxsize=96)
def fetch_day_klines(day: str) -> tuple[tuple[int, float], ...]:
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
    return tuple(out)


def klines_window(entry_ms: int, lookback_ms: int = 1_800_000) -> list[tuple[int, float]]:
    """1m close prices from lookback_ms before entry through entry."""
    day = datetime.fromtimestamp(entry_ms / 1000, tz=timezone.utc).date().isoformat()
    prev = (datetime.fromtimestamp(entry_ms / 1000, tz=timezone.utc).date() - __import__("datetime").timedelta(days=1)).isoformat()
    series: list[tuple[int, float]] = []
    for d in (prev, day):
        series.extend(fetch_day_klines(d))
    series.sort(key=lambda x: x[0])
    start = entry_ms - lookback_ms
    return [(t, p) for t, p in series if start <= t <= entry_ms]


def realized_vol_180s_bps(klines: list[tuple[int, float]]) -> float:
    if len(klines) < 4:
        return 0.0
    rets = []
    for i in range(1, len(klines)):
        if klines[i - 1][1] > 0:
            rets.append(math.log(klines[i][1] / klines[i - 1][1]))
    if len(rets) < 3:
        return 0.0
    tail = rets[-3:]
    step_std = st.pstdev(tail) if len(tail) > 1 else abs(tail[0])
    bar_std = step_std * math.sqrt(180.0)
    return bar_std * 10_000.0


def path_stats_from_klines(klines: list[tuple[int, float]], step_ms: int = 60_000) -> dict | None:
    if len(klines) < 12:
        return None
    # Resample to step_ms grid
    t0, t1 = klines[0][0], klines[-1][0]
    prices: list[float] = []
    idx = 0
    for t in range(t0, t1 + 1, step_ms):
        while idx + 1 < len(klines) and klines[idx + 1][0] <= t:
            idx += 1
        if klines[idx][0] <= t and klines[idx][1] > 0:
            prices.append(klines[idx][1])
    if len(prices) < 12:
        return None
    sum_abs = 0.0
    flips = 0
    steps = 0
    prev_sign = 0
    for i in range(1, len(prices)):
        d = prices[i] - prices[i - 1]
        sum_abs += abs(d)
        sign = 1 if d > 0 else (-1 if d < 0 else 0)
        if sign:
            if prev_sign and sign != prev_sign:
                flips += 1
            prev_sign = sign
            steps += 1
    net = abs(prices[-1] - prices[0])
    efficiency = net / sum_abs if sum_abs > 0 else 0.0
    flip_rate = flips / (steps - 1) if steps > 1 else 0.0
    vol_180s = realized_vol_180s_bps(klines)
    return {
        "path_efficiency": efficiency,
        "sign_flip_rate": flip_rate,
        "vol_180s_bps": vol_180s,
    }


def classify_regime_from_stats(stats: dict) -> str:
    if stats["vol_180s_bps"] < CALM_VOL_BPS:
        return "calm_low_vol"
    if stats["path_efficiency"] >= TREND_EFFICIENCY:
        return "clean_directional"
    if stats["sign_flip_rate"] >= HIGH_FLIP:
        return "expanded_high_flip"
    return "expanded_mixed"


def day_spot_stats(day: str) -> dict | None:
    klines = list(fetch_day_klines(day))
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
    label = "directional_trend" if trend_eff >= 0.35 and abs(ret_bps) >= 40 else None
    if label is None:
        if flips >= 400 and trend_eff < 0.15:
            label = "chop_whipsaw"
        elif rv >= 80:
            label = "high_vol"
        elif rv < 35:
            label = "low_vol"
        else:
            label = "mixed"
    return {
        "ret_bps": ret_bps,
        "abs_ret_bps": abs(ret_bps),
        "rv_bps": rv,
        "range_bps": rng,
        "sign_flips": flips,
        "trend_eff": trend_eff,
        "day_regime": label,
    }


def enrich_rows(rows: list[dict], day_regime_path: Path | None = None) -> None:
    day_cache: dict[str, dict] = {}
    if day_regime_path and day_regime_path.is_file():
        day_cache = json.loads(day_regime_path.read_text(encoding="utf-8"))

    for r in rows:
        day = r["utc_day"]
        if day not in day_cache:
            sp = day_spot_stats(day)
            if sp:
                day_cache[day] = sp
        dr = day_cache.get(day, {})
        r["day_regime"] = dr.get("day_regime", "unknown")
        r["day_rv_bps"] = dr.get("rv_bps")
        r["day_trend_eff"] = dr.get("trend_eff")
        r["day_ret_bps"] = dr.get("ret_bps")

        # Entry-time regime reconstruction from 1m klines (all rows)
        kl = klines_window(r["entry_ms"])
        stats = path_stats_from_klines(kl)
        if stats:
            r["path_efficiency"] = stats["path_efficiency"]
            r["sign_flip_rate"] = stats["sign_flip_rate"]
            r["vol_180s_bps"] = stats["vol_180s_bps"]
            r["regime_reconstructed"] = classify_regime_from_stats(stats)
        else:
            r["regime_reconstructed"] = "unknown"
        r["regime"] = r["regime_logged"] or r["regime_reconstructed"]

        touch = r["touch_price"]
        if touch < 0.50:
            r["ask_band"] = "underdog"
        elif touch < 0.65:
            r["ask_band"] = "mid"
        else:
            r["ask_band"] = "fav"

        ret120 = r.get("spot_ret_120s_bps")
        if ret120 is not None:
            if r["side"] == "up":
                r["spot_aligned_120s"] = ret120 <= 2.0
                r["spot_against_120s"] = ret120 > 2.0
            else:
                r["spot_aligned_120s"] = ret120 >= -2.0
                r["spot_against_120s"] = ret120 < -2.0
        else:
            r["spot_aligned_120s"] = None
            r["spot_against_120s"] = None

        period = "good_tape" if day <= "2026-06-16" else ("drawdown" if day <= "2026-06-19" else "post_deploy")
        r["period"] = period


@dataclass
class BucketStats:
    n: int = 0
    wins: int = 0
    pnl: float = 0.0
    pnls: list[float] | None = None

    def add(self, won: bool, pnl: float) -> None:
        self.n += 1
        self.wins += int(won)
        self.pnl += pnl
        if self.pnls is not None:
            self.pnls.append(pnl)

    @property
    def hit(self) -> float:
        return self.wins / self.n if self.n else 0.0

    @property
    def mean_pnl(self) -> float:
        return self.pnl / self.n if self.n else 0.0


def group_by(rows: list[dict], key: str) -> dict[str, BucketStats]:
    out: dict[str, BucketStats] = defaultdict(lambda: BucketStats(pnls=[]))
    for r in rows:
        out[str(r.get(key) or "unknown")].add(r["won"], r["pnl_usd"])
    return dict(out)


def bootstrap_ci(values: list[float], n_boot: int = 2000, alpha: float = 0.05) -> tuple[float, float, float]:
    if not values:
        return float("nan"), float("nan"), float("nan")
    if len(values) == 1:
        return values[0], values[0], values[0]
    means = []
    n = len(values)
    for _ in range(n_boot):
        sample = [values[random.randrange(n)] for _ in range(n)]
        means.append(sum(sample) / n)
    means.sort()
    lo = means[int((alpha / 2) * n_boot)]
    hi = means[int((1 - alpha / 2) * n_boot) - 1]
    return st.mean(values), lo, hi


def smd(a: list[float], b: list[float]) -> float:
    if len(a) < 3 or len(b) < 3:
        return float("nan")
    ma, mb = st.mean(a), st.mean(b)
    sa = st.pstdev(a) or 1e-9
    sb = st.pstdev(b) or 1e-9
    sp = math.sqrt(0.5 * (sa * sa + sb * sb))
    return (ma - mb) / sp if sp > 1e-12 else 0.0


def auc_binary(y_true: list[int], scores: list[float]) -> float:
    pairs = sorted(zip(scores, y_true), key=lambda x: x[0])
    n_pos = sum(y_true)
    n_neg = len(y_true) - n_pos
    if n_pos == 0 or n_neg == 0:
        return float("nan")
    rank_sum = 0.0
    for i, (_, y) in enumerate(pairs, 1):
        if y:
            rank_sum += i
    return (rank_sum - n_pos * (n_pos + 1) / 2) / (n_pos * n_neg)


def fmt_money(v: float) -> str:
    return f"${v:+,.0f}"


def filter_period(rows: list[dict], since: str, until: str | None) -> list[dict]:
    s = parse_ts(since + "T00:00:00Z")
    u = parse_ts((until or "2099-12-31") + "T23:59:59Z")
    return [r for r in rows if s <= parse_ts(r["ts_utc"]) <= u]


def cross_tab(
    rows: list[dict], key_a: str, key_b: str
) -> dict[tuple[str, str], BucketStats]:
    out: dict[tuple[str, str], BucketStats] = defaultdict(lambda: BucketStats(pnls=[]))
    for r in rows:
        a = str(r.get(key_a) or "unknown")
        b = str(r.get(key_b) or "unknown")
        out[(a, b)].add(r["won"], r["pnl_usd"])
    return dict(out)


def bucket_from_rows(subset: list[dict]) -> BucketStats:
    b = BucketStats(pnls=[r["pnl_usd"] for r in subset])
    b.n = len(subset)
    b.wins = sum(1 for r in subset if r["won"])
    b.pnl = sum(r["pnl_usd"] for r in subset)
    return b


def gate_counterfactual(
    rows: list[dict], pred: Callable[[dict], bool], label: str
) -> tuple[str, BucketStats, BucketStats]:
    kept = [r for r in rows if pred(r)]
    removed = [r for r in rows if not pred(r)]
    return label, bucket_from_rows(kept), bucket_from_rows(removed)