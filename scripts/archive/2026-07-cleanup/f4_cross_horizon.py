#!/usr/bin/env python3
"""F4: does BTC-15m mid lead BTC-5m mid repricing (or vice versa)?

Aligns 15m/5m tick-cache mid series on a 0.5s grid over each overlapping
5m window, cross-correlates mid CHANGES at leads/lags up to 30s, and runs a
jump-event precedence check. Hard date cap: nothing on/after 2026-05-19.
"""

import json
import os
import sys
import collections

import numpy as np
import zstandard

ROOT = "/Users/jackreid/go/polymarket-backtest/data"
TICKS = f"{ROOT}/cache/ticks"
GRID = 0.5          # seconds
MAX_LAG = 60        # grid steps -> 30s
EDGE_TRIM = 10.0    # seconds trimmed at both ends of the 5m window
MIN_TICKS = 30
JUMP_5M = 0.04      # 5m mid jump threshold over 1s
JUMP_15M = 0.02
SAMPLE_EVERY = 6    # every 6th 15m window per day -> 16/day
DATE_CAP = "2026-05-19"

DT = np.dtype([
    ("ts_ns", "<i8"), ("yes_bid", "<f4"), ("yes_ask", "<f4"),
    ("bids", "<f4", (10,)), ("asks", "<f4", (10,)),
    ("no_bid", "<f4"), ("no_ask", "<f4"),
    ("no_bids", "<f4", (10,)), ("no_asks", "<f4", (10,)),
])

_dctx = zstandard.ZstdDecompressor()


def load_mid(date, asset_id):
    for suffix in ("2s", "1s"):
        path = f"{TICKS}/{date}/{asset_id}.{suffix}.btc"
        if os.path.exists(path):
            break
    else:
        return None
    raw = open(path, "rb").read()
    if raw[:4] != b"PTC2":
        return None
    body = _dctx.decompress(raw[4:], max_output_size=600_000_000)
    n = int.from_bytes(body[:8], "little")
    a = np.frombuffer(body, dtype=DT, count=n, offset=8)
    ts = a["ts_ns"] / 1e9
    mid = (a["yes_bid"].astype(np.float64) + a["yes_ask"]) / 2.0
    ok = (a["yes_bid"] > 0) & (a["yes_ask"] > 0) & (a["yes_ask"] < 1)
    return ts[ok], mid[ok]


def grid_series(ts, mid, t0, t1):
    """Forward-filled mid on the 0.5s grid over [t0, t1]."""
    grid = np.arange(t0, t1 + 1e-9, GRID)
    idx = np.searchsorted(ts, grid, side="right") - 1
    if idx[0] < 0:
        return None, None
    n_in = np.count_nonzero((ts >= t0) & (ts <= t1))
    return grid, mid[idx] if n_in >= MIN_TICKS else (None, None)[1]


def xcorr_curves(d_lead, d_lag):
    """corr(d_lead[t-k], d_lag[t]) for k=0..MAX_LAG. NaN where degenerate."""
    out = np.full(MAX_LAG + 1, np.nan)
    for k in range(MAX_LAG + 1):
        a = d_lead[: len(d_lead) - k] if k else d_lead
        b = d_lag[k:]
        if a.std() > 0 and b.std() > 0:
            out[k] = np.corrcoef(a, b)[0, 1]
    return out


def jump_precedence(grid, m_jump, m_other, thresh):
    """For each 1s jump in m_jump, did m_other move same-direction in the
    prior 30s (before) vs the next 30s (after)? Returns (before, after,
    both, neither) counts. Events are de-duplicated to 30s spacing."""
    d1 = m_jump[2:] - m_jump[:-2]  # 1s change on 0.5s grid
    ev = np.where(np.abs(d1) >= thresh)[0] + 2
    counts = [0, 0, 0, 0]
    last = -1e9
    w = int(30 / GRID)
    for i in ev:
        if grid[i] - last < 30:
            continue
        if i - w < 0 or i + w >= len(grid):
            continue
        last = grid[i]
        sgn = np.sign(d1[i - 2])
        pre = sgn * (m_other[i] - m_other[i - w])
        post = sgn * (m_other[i + w] - m_other[i])
        b, a = pre >= 0.01, post >= 0.01
        if b and a:
            counts[2] += 1
        elif b:
            counts[0] += 1
        elif a:
            counts[1] += 1
        else:
            counts[3] += 1
    return counts


def main():
    man15, man5 = {}, {}
    for line in open(f"{ROOT}/manifests/canonical/btc-updown-15m_up.jsonl"):
        r = json.loads(line)
        man15.setdefault(r["date"], []).append(r)
    for line in open(f"{ROOT}/manifests/canonical/btc-updown-5m_up.jsonl"):
        r = json.loads(line)
        man5[(r["date"], r["close_ts"])] = r

    days = [f"2026-05-{d:02d}" for d in range(7, 19)]
    assert all(d < DATE_CAP for d in days)

    per_day = {}
    jp_15_jumps = collections.Counter()  # 15m jumps, does 5m precede/follow
    jp_5_jumps = collections.Counter()
    n_pairs = 0

    for day in days:
        wins = sorted(man15.get(day, []), key=lambda r: r["close_ts"])
        wins = wins[::SAMPLE_EVERY]
        c15_all, c5_all = [], []
        peak_lags = []
        for w in wins:
            t_open15 = w["close_ts"] - 900
            r15 = load_mid(day, w["asset_id"])
            if r15 is None:
                continue
            ts15, mid15 = r15
            for j in range(3):
                c5_close = t_open15 + 300 * (j + 1)
                m5 = man5.get((day, c5_close))
                if m5 is None:
                    continue
                r5 = load_mid(day, m5["asset_id"])
                if r5 is None:
                    continue
                ts5, mid5 = r5
                t0 = c5_close - 300 + EDGE_TRIM
                t1 = c5_close - EDGE_TRIM
                g, s15 = grid_series(ts15, mid15, t0, t1)
                if s15 is None:
                    continue
                g, s5 = grid_series(ts5, mid5, t0, t1)
                if s5 is None:
                    continue
                d15 = np.diff(s15)
                d5 = np.diff(s5)
                if np.count_nonzero(d15) < 10 or np.count_nonzero(d5) < 10:
                    continue
                c15 = xcorr_curves(d15, d5)   # 15m leads 5m
                c5 = xcorr_curves(d5, d15)    # 5m leads 15m
                if np.isnan(c15).all() or np.isnan(c5).all():
                    continue
                c15_all.append(c15)
                c5_all.append(c5)
                full = np.concatenate([c5[1:][::-1], [c15[0]], c15[1:]])
                lags = np.concatenate([-np.arange(1, MAX_LAG + 1)[::-1],
                                       [0], np.arange(1, MAX_LAG + 1)]) * GRID
                if not np.isnan(full).all():
                    peak_lags.append(lags[np.nanargmax(full)])
                for k, key in enumerate("before after both neither".split()):
                    jp = jump_precedence(g, s15, s5, JUMP_15M)
                    jp_15_jumps[key] += jp[k]
                    jp = jump_precedence(g, s5, s15, JUMP_5M)
                    jp_5_jumps[key] += jp[k]
                n_pairs += 1
        if not c15_all:
            continue
        c15_m = np.nanmean(np.vstack(c15_all), axis=0)
        c5_m = np.nanmean(np.vstack(c5_all), axis=0)
        per_day[day] = dict(
            n=len(c15_all),
            corr0=float(c15_m[0]),
            lead15_mean=float(np.nanmean(c15_m[1:])),
            lead5_mean=float(np.nanmean(c5_m[1:])),
            lead15_peak=float(np.nanmax(c15_m[1:])),
            lead5_peak=float(np.nanmax(c5_m[1:])),
            lead15_peak_lag_s=float((np.nanargmax(c15_m[1:]) + 1) * GRID),
            lead5_peak_lag_s=float((np.nanargmax(c5_m[1:]) + 1) * GRID),
            asym=float(np.nanmean(c15_m[1:]) - np.nanmean(c5_m[1:])),
            peak_lag_median_s=float(np.median(peak_lags)) if peak_lags else None,
            c15=c15_m.tolist(),
            c5=c5_m.tolist(),
        )

    print(json.dumps(dict(per_day={d: {k: v for k, v in r.items()
                                       if k not in ("c15", "c5")}
                                   for d, r in per_day.items()},
                          n_pairs=n_pairs,
                          jump15=dict(jp_15_jumps),
                          jump5=dict(jp_5_jumps)), indent=1))

    # pooled correlogram, coarse bins
    if per_day:
        c15_pool = np.nanmean(np.vstack([r["c15"] for r in per_day.values()]), axis=0)
        c5_pool = np.nanmean(np.vstack([r["c5"] for r in per_day.values()]), axis=0)
        print("\nlag_s  corr(15m leads)  corr(5m leads)")
        for k in [1, 2, 4, 6, 10, 20, 40, 60]:
            print(f"{k * GRID:5.1f}  {c15_pool[k]:+.4f}          {c5_pool[k]:+.4f}")
        print(f"  0.0  {c15_pool[0]:+.4f} (contemporaneous)")


if __name__ == "__main__":
    main()
