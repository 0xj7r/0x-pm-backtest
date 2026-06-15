#!/usr/bin/env python3
"""F6: are taker-flow book moves without a spot move fadeable on BTC-5m updown?

Flow event: side mid moves >= 3c within <= 2 book ticks while Binance spot
moved < 2bps over the same interval. Info event: same mid move with spot
move >= 4bps. Fade = buy the other side at its post-burst ask (taker,
fee 0.07*p*(1-p)), mark to mid at +30/60/120s.
"""

import json
import math
import random
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq
import zstandard

ROOT = Path("/Users/jackreid/go/polymarket-backtest")
TICKS = ROOT / "data/cache/ticks"
SPOT_DIR = ROOT / "data/cache/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT"
MANIFEST = ROOT / "data/manifests/fullhist/btc5m-20260212-20260520-full-28205.jsonl"

TICK_DT = np.dtype([
    ("ts_ns", "<i8"), ("yes_bid", "<f4"), ("yes_ask", "<f4"),
    ("bids", "<f4", (10,)), ("asks", "<f4", (10,)),
    ("no_bid", "<f4"), ("no_ask", "<f4"),
    ("no_bids", "<f4", (10,)), ("no_asks", "<f4", (10,)),
])

BURST_C = 0.03          # mid move threshold (cents)
FLOW_BPS = 2.0          # spot move below this => flow-driven
INFO_BPS = 4.0          # spot move at/above this => information-driven
FEE_RATE = 0.07
HORIZONS = (30, 60, 120)
COOLDOWN_S = 5.0
VOL_WIN_S = 600


def load_manifest():
    rows = []
    with open(MANIFEST) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def load_ticks(date, asset_id):
    for suffix in ("2s", "1s"):
        p = TICKS / date / f"{asset_id}.{suffix}.btc"
        if p.exists():
            raw = p.read_bytes()
            if raw[:4] != b"PTC2":
                return None
            d = zstandard.ZstdDecompressor().decompress(raw[4:], max_output_size=1 << 31)
            n = int.from_bytes(d[:8], "little")
            return np.frombuffer(d, dtype=TICK_DT, count=n, offset=8)
    return None


def load_spot_day(date):
    p = SPOT_DIR / f"date={date}"
    files = sorted(p.glob("*.parquet")) if p.exists() else []
    if not files:
        return None
    pf = pq.ParquetFile(files[0])
    ts_parts, px_parts = [], []
    for batch in pf.iter_batches(columns=["transact_time_ms", "price"], batch_size=1 << 18):
        ts_parts.append(batch.column(0).to_numpy())
        px_parts.append(batch.column(1).to_numpy(zero_copy_only=False).astype(np.float64))
    ts = np.concatenate(ts_parts)
    px = np.concatenate(px_parts)
    if ts[0] > 1e14:  # microseconds despite the _ms column name
        ts = ts // 1000
    order = np.argsort(ts, kind="stable")
    return ts[order].astype(np.int64), px[order]


def spot_at(spot, ts_ms):
    ts_arr, px = spot
    i = np.searchsorted(ts_arr, ts_ms, side="right") - 1
    if i < 0:
        return None
    return px[i]


def trailing_vol_bps(spot, ts_ms):
    """Std of 1s log returns over the trailing window, in bps."""
    ts_arr, px = spot
    lo = np.searchsorted(ts_arr, ts_ms - VOL_WIN_S * 1000, side="left")
    hi = np.searchsorted(ts_arr, ts_ms, side="right")
    if hi - lo < 30:
        return None
    grid = np.arange(ts_ms - VOL_WIN_S * 1000, ts_ms, 1000)
    gi = np.searchsorted(ts_arr, grid, side="right") - 1
    gi = gi[gi >= 0]
    series = px[gi]
    r = np.diff(np.log(series))
    if len(r) < 30:
        return None
    return float(np.std(r) * 1e4)


def phi(x):
    return 0.5 * (1.0 + math.erf(x / math.sqrt(2.0)))


def scan_market(mkt, spot, events_out):
    close_ts = mkt["close_ts"]
    open_ts = close_ts - 300
    a = load_ticks(mkt["date"], mkt["asset_id"])
    if a is None or len(a) == 0:
        return False
    ts_s = a["ts_ns"] / 1e9
    in_win = (ts_s >= open_ts) & (ts_s <= close_ts)
    valid = (a["yes_bid"] > 0.0) & (a["yes_ask"] > a["yes_bid"]) & (a["yes_ask"] < 1.0)
    sel = in_win & valid
    if sel.sum() < 10:
        return False
    t = a[sel]
    raw_ts = t["ts_ns"] / 1e9
    # The cache is an event tape (sub-second cadence); resample to 2s
    # snapshots so "within <= 2 ticks" matches the intended 2s cadence and
    # spot has room to move over the interval.
    grid = np.arange(open_ts, close_ts + 1e-9, 2.0)
    gi = np.searchsorted(raw_ts, grid, side="right") - 1
    keep = gi >= 0
    gi = gi[keep]
    t = t[gi]
    ts = grid[keep]
    mid = (t["yes_bid"].astype(np.float64) + t["yes_ask"].astype(np.float64)) / 2.0

    strike = spot_at(spot, int(open_ts * 1000))
    if strike is None:
        return False

    n = len(t)
    i = 0
    last_event_ts = -1e18
    while i < n - 1:
        hit = None
        for j in (i + 1, i + 2):
            if j >= n:
                break
            d = mid[j] - mid[i]
            if abs(d) >= BURST_C:
                hit = j
                break
        if hit is None:
            i += 1
            continue
        j = hit
        if ts[j] - last_event_ts < COOLDOWN_S:
            i = j
            continue
        # spot move over the same interval
        s_i = spot_at(spot, int(ts[i] * 1000))
        s_j = spot_at(spot, int(ts[j] * 1000))
        if s_i is None or s_j is None or s_i <= 0:
            i = j
            continue
        spot_bps = abs(math.log(s_j / s_i)) * 1e4
        if spot_bps < FLOW_BPS:
            klass = "flow"
        elif spot_bps >= INFO_BPS:
            klass = "info"
        else:
            klass = "amb"
        last_event_ts = ts[j]

        if ts[j] > close_ts - 125:  # need the 120s horizon inside the market
            i = j
            continue

        d = mid[j] - mid[i]
        up = d > 0
        # fade entry: buy the other side at its post-burst ask
        tk = t[j]
        has_no = 0.0 < tk["no_bid"] < tk["no_ask"] < 1.0
        if up:
            entry = float(tk["no_ask"]) if has_no else 1.0 - float(tk["yes_bid"])
        else:
            entry = float(tk["yes_ask"])
        if not (0.02 <= entry <= 0.98):
            i = j
            continue
        fee = FEE_RATE * entry * (1.0 - entry)

        # BSM fair value at the event (strike proxy = spot at open)
        sigma_1s = trailing_vol_bps(spot, int(ts[j] * 1000))
        fair = None
        if sigma_1s and sigma_1s > 0:
            tau = close_ts - ts[j]
            denom = (sigma_1s / 1e4) * math.sqrt(max(tau, 1.0))
            fair = phi(math.log(s_j / strike) / denom) if denom > 0 else None

        s_fwd = spot_at(spot, int((ts[j] + 60) * 1000))
        fwd_bps = math.log(s_fwd / s_j) * 1e4 if s_fwd else None
        ev = {
            "window": mkt["window"], "klass": klass, "spot_bps": spot_bps,
            "spot_fwd60_bps": fwd_bps,
            "dmid": float(d), "mid_pre": float(mid[i]), "mid_post": float(mid[j]),
            "entry": entry, "fee": fee, "up": bool(up),
            "tau": close_ts - ts[j], "sigma_1s_bps": sigma_1s, "fair": fair,
            "date": mkt["date"],
        }
        ok = True
        for h in HORIZONS:
            k = np.searchsorted(ts, ts[j] + h, side="left")
            if k >= n:
                ok = False
                break
            mh = mid[k]
            ev[f"mid_{h}"] = float(mh)
            ev[f"rev_{h}"] = float((mid[j] - mh) / d)  # 1 = full reversion
            val = (1.0 - mh) if up else mh
            ev[f"pnl_{h}"] = float(val - entry - fee)
        if ok:
            events_out.append(ev)
        i = j
    return True


def summarize(events, label):
    out = [f"== {label}: {len(events)} events =="]
    for klass in ("flow", "amb", "info"):
        sub = [e for e in events if e["klass"] == klass]
        if not sub:
            out.append(f"  {klass}: 0 events")
            continue
        out.append(f"  {klass}: n={len(sub)}")
        for h in HORIZONS:
            rev = np.array([e[f"rev_{h}"] for e in sub])
            pnl = np.array([e[f"pnl_{h}"] for e in sub])
            tstat = pnl.mean() / (pnl.std() / math.sqrt(len(pnl))) if len(pnl) > 2 and pnl.std() > 0 else float("nan")
            out.append(
                f"    h={h:3d}s rev_frac mean={rev.mean():+.3f} med={np.median(rev):+.3f} | "
                f"fade pnl/share mean={pnl.mean()*100:+.2f}c med={np.median(pnl)*100:+.2f}c "
                f"win={float((pnl > 0).mean()):.2f} t={tstat:+.2f}"
            )
    return "\n".join(out)


def main():
    random.seed(20260612)
    rows = load_manifest()
    w2 = [r for r in rows if "2026-04-01" <= r["date"] <= "2026-04-30"]
    w3 = [r for r in rows if "2026-05-01" <= r["date"] <= "2026-05-18"]
    by_day_w2 = defaultdict(list)
    for r in w2:
        by_day_w2[r["date"]].append(r)
    sample = []
    for d in sorted(by_day_w2):
        picks = random.sample(by_day_w2[d], min(10, len(by_day_w2[d])))
        for p in picks:
            p["window"] = "W2"
            sample.append(p)
    w3s = random.sample(w3, min(50, len(w3)))
    for p in w3s:
        p["window"] = "W3"
        sample.append(p)

    by_date = defaultdict(list)
    for m in sample:
        by_date[m["date"]].append(m)

    events = []
    n_run = n_skip = 0
    for date in sorted(by_date):
        spot = load_spot_day(date)
        if spot is None:
            n_skip += len(by_date[date])
            continue
        for m in by_date[date]:
            if scan_market(m, spot, events):
                n_run += 1
            else:
                n_skip += 1
        print(f"{date}: cum markets={n_run} events={len(events)}", file=sys.stderr)

    print(f"\nmarkets run={n_run} skipped={n_skip} total events={len(events)}")
    w2_ev = [e for e in events if e["window"] == "W2"]
    w3_ev = [e for e in events if e["window"] == "W3"]
    w2_mkts = sum(1 for m in sample if m["window"] == "W2")
    flow_w2 = sum(1 for e in w2_ev if e["klass"] == "flow")
    print(f"W2 flow events per market = {flow_w2 / max(w2_mkts,1):.3f} "
          f"(~{flow_w2 / max(w2_mkts,1) * 288:.0f}/day at 288 markets/day)")
    print(summarize(w2_ev, "W2 (2026-04)"))
    print(summarize(w3_ev, "W3 (2026-05-01..18)"))

    # vol-regime split within W2 flow events
    fl = [e for e in w2_ev if e["klass"] == "flow" and e["sigma_1s_bps"]]
    if fl:
        med = float(np.median([e["sigma_1s_bps"] for e in fl]))
        lo = [e for e in fl if e["sigma_1s_bps"] < med]
        hi = [e for e in fl if e["sigma_1s_bps"] >= med]
        print(f"\nW2 flow vol split at sigma_1s={med:.2f}bps")
        print(summarize(lo, "W2 flow LOW-vol"))
        print(summarize(hi, "W2 flow HIGH-vol"))

    # does the flow move push price away from BSM fair?
    fl_fair = [e for e in events if e["klass"] == "flow" and e["fair"] is not None]
    if fl_fair:
        away = sum(1 for e in fl_fair
                   if (e["up"] and e["mid_post"] > e["fair"]) or (not e["up"] and e["mid_post"] < e["fair"]))
        print(f"\nflow moves ending beyond BSM fair: {away}/{len(fl_fair)} = {away/len(fl_fair):.2f}")

    with open(ROOT / "docs/research/autoloop/f6_events.jsonl", "w") as f:
        for e in events:
            f.write(json.dumps(e) + "\n")


if __name__ == "__main__":
    main()
