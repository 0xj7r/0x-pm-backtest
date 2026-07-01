#!/usr/bin/env python3
"""Term-structure relative-value measurement: BTC updown 5m vs 15m.

For each instant on a 5s grid, price BOTH active horizons (5m, 15m) with the
same BSM machinery as crates/pm-alpha/src/fair_value.rs from one Binance
(spot, vol) state, compare to the executable book (Up token; Down side via
the exact CLOB mirror), and record joint opposite-direction dislocations.

Outputs per-event records to <data-root>/data/runs/ts_rv/events.jsonl and
prints a per-day summary (events/day by edge tier, persistence, depth,
hold-to-resolution P&L of a naive >4c rule at $50/leg).

Data: May 21-26 2026 (the only May dates with 15m book coverage).
Note: the canonical manifests' `close_ts` field is actually the window OPEN
timestamp (slug suffix); verified against Binance close-vs-open outcomes.
"""

import argparse
import bisect
import glob
import json
import math
import os

import numpy as np
import pyarrow.parquet as pq

DATA_ROOT = os.environ.get("PM_DATA_ROOT", "/Users/jackreid/go/polymarket-backtest")
BOOK_BASE = os.path.join(
    DATA_ROOT, "data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25"
)
BINANCE_BASE = os.path.join(
    DATA_ROOT, "data/cache/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT"
)
MANI = os.path.join(DATA_ROOT, "data/manifests/canonical")

VOL_LOOKBACK_S = 3600
GRID_STEP_S = 5
MIN_REMAINING_S = 15  # mirror engine stop-before-close; avoid step-function regime
MIN_ELAPSED_S = 10  # skip first seconds of window (book reset)
TIERS = [0.02, 0.04, 0.08]
TRADE_TIER = 0.04
STAKE_PER_LEG = 50.0


def normal_cdf(x):
    return 0.5 * (1.0 + math.erf(x / math.sqrt(2.0)))


def bsm_p_up(spot, strike, rem_s, vol_bps_bar, bar_s):
    """Mirror of pm_alpha::fair_value::estimate_fair_value."""
    if not (spot > 0 and strike > 0 and rem_s >= 0 and vol_bps_bar > 0):
        return None
    lm = math.log(spot / strike)
    if rem_s < 1.0:
        return 1.0 if lm > 0 else 0.0
    sigma = (vol_bps_bar / 1e4) * math.sqrt(min(max(rem_s / bar_s, 0.0), 1.0))
    if sigma < 1e-12:
        return 1.0 if lm > 0 else 0.0
    return min(max(normal_cdf(lm / sigma), 0.0), 1.0)


def load_manifest(name):
    with open(os.path.join(MANI, name)) as f:
        return [json.loads(line) for line in f]


def load_binance_day(date, prev_date):
    import pandas as pd

    frames = []
    for d in (prev_date, date):
        for f in glob.glob(f"{BINANCE_BASE}/date={d}/*.parquet"):
            frames.append(
                pq.ParquetFile(f).read(columns=["transact_time_ms", "price"]).to_pandas()
            )
    t = pd.concat(frames).sort_values("transact_time_ms")
    # column named _ms but holds microseconds
    return t["transact_time_ms"].values / 1e6, t["price"].astype(float).values


def one_second_grid(ts, px, t0, t1):
    """Last price at-or-before each 1s instant in [t0, t1]."""
    grid = np.arange(t0, t1 + 1, 1.0)
    idx = np.searchsorted(ts, grid, side="right") - 1
    valid = idx >= 0
    prices = np.full(grid.shape, np.nan)
    prices[valid] = px[idx[valid]]
    return grid, prices


def rolling_vol_bps_step(prices):
    """Std of 1s log returns over trailing VOL_LOOKBACK_S samples, in bps.

    Mirrors pm_alpha::vol (sample 1s, stale prices repeat -> zero returns).
    Returns array aligned to `prices`; NaN until enough lookback.
    """
    lr = np.diff(np.log(prices), prepend=np.nan)
    n = VOL_LOOKBACK_S
    out = np.full(prices.shape, np.nan)
    if len(lr) <= n:
        return out
    lr0 = np.nan_to_num(lr)
    s = np.cumsum(lr0)
    s2 = np.cumsum(lr0 * lr0)
    win_sum = s[n:] - s[:-n]
    win_sum2 = s2[n:] - s2[:-n]
    var = (win_sum2 - win_sum * win_sum / n) / (n - 1)
    out[n:] = np.sqrt(np.maximum(var, 0.0)) * 1e4
    return out


class Book:
    """Level-0 Up-token book as parallel arrays for at-or-before lookup."""

    __slots__ = ("ts", "bid", "ask", "bid_sz", "ask_sz")

    def __init__(self, files):
        import pandas as pd

        cols = ["timestamp_us", "bid_price_0", "bid_size_0", "ask_price_0", "ask_size_0"]
        t = pd.concat(
            [pq.ParquetFile(f).read(columns=cols).to_pandas() for f in files]
        ).sort_values("timestamp_us")
        self.ts = t["timestamp_us"].values / 1e6
        self.bid = pd.to_numeric(t["bid_price_0"], errors="coerce").values
        self.ask = pd.to_numeric(t["ask_price_0"], errors="coerce").values
        self.bid_sz = pd.to_numeric(t["bid_size_0"], errors="coerce").values
        self.ask_sz = pd.to_numeric(t["ask_size_0"], errors="coerce").values

    def at(self, t):
        i = bisect.bisect_right(self.ts, t) - 1
        if i < 0:
            return None
        b, a = self.bid[i], self.ask[i]
        if not (np.isfinite(b) and np.isfinite(a)) or a <= b:
            return None
        return b, a, self.bid_sz[i], self.ask_sz[i]


def book_files(asset_id, dates):
    fs = []
    for d in dates:
        fs.extend(glob.glob(f"{BOOK_BASE}/date={d}/asset_id={asset_id}/*.parquet"))
    return fs


def edges_for(book_state, p_fair):
    """(buy-Up edge, buy-Down edge) executable at the asks.

    Down book is the exact CLOB mirror of Up: ask_down = 1 - bid_up,
    ask_down_size = bid_up_size (verified empirically on raw Down books).
    """
    bid, ask, bid_sz, ask_sz = book_state
    edge_up = p_fair - ask  # buy Up at Up ask
    edge_dn = bid - p_fair  # buy Down at (1 - bid_up)
    return edge_up, edge_dn, ask_sz, bid_sz


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dates", nargs="+", default=[f"2026-05-{d}" for d in range(21, 27)])
    ap.add_argument("--out", default=os.path.join(DATA_ROOT, "data/runs/ts_rv/events.jsonl"))
    ap.add_argument("--strike-source", choices=["manifest", "binance"], default="manifest",
                    help="5m strike source (15m is always binance proxy)")
    args = ap.parse_args()

    import datetime as dt

    m5 = load_manifest("btc-updown-5m_up.jsonl")
    m15 = load_manifest("btc-updown-15m_up.jsonl")
    strikes5 = {}
    with open(os.path.join(MANI, "strikes_btc5m.jsonl")) as f:
        for line in f:
            r = json.loads(line)
            strikes5[r["open_ts"]] = r["open_price"]

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    events_f = open(args.out, "w")

    day_summaries = []
    for date in args.dates:
        d0 = dt.datetime.strptime(date, "%Y-%m-%d").replace(tzinfo=dt.timezone.utc)
        day_start = int(d0.timestamp())
        prev = (d0 - dt.timedelta(days=1)).strftime("%Y-%m-%d")
        nxt = (d0 + dt.timedelta(days=1)).strftime("%Y-%m-%d")

        ts, px = load_binance_day(date, prev)
        grid_t, grid_px = one_second_grid(
            ts, px, day_start - VOL_LOOKBACK_S - 10, day_start + 86400
        )
        vol_step = rolling_vol_bps_step(grid_px)

        def spot_at(t):
            i = int(t - grid_t[0])
            return grid_px[i] if 0 <= i < len(grid_px) else np.nan

        def vol_bar_at(t, bar_s):
            i = int(t - grid_t[0])
            if 0 <= i < len(vol_step) and np.isfinite(vol_step[i]):
                return vol_step[i] * math.sqrt(bar_s)
            return np.nan

        # manifest `close_ts` is the OPEN ts (slug suffix); window = [open, open+dur]
        def day_markets(mani, dur):
            out = {}
            for r in mani:
                o = r["close_ts"]
                if day_start <= o < day_start + 86400:
                    out[o] = {"open": o, "close": o + dur, "outcome": r["outcome"],
                              "asset_id": r["asset_id"], "slug": r["slug"]}
            return out

        mk5 = day_markets(m5, 300)
        mk15 = day_markets(m15, 900)

        books = {}
        missing = {"5m": 0, "15m": 0}
        for label, mks in (("5m", mk5), ("15m", mk15)):
            for o, m in mks.items():
                fs = book_files(m["asset_id"], [prev, date, nxt])
                if fs:
                    books[m["asset_id"]] = Book(fs)
                else:
                    missing[label] += 1

        strike_err = []
        for o, m in mk5.items():
            sb = spot_at(o)
            sm = strikes5.get(o)
            if sm is not None and np.isfinite(sb):
                strike_err.append((sm - sb) / sb * 1e4)
            m["strike"] = sm if (args.strike_source == "manifest" and sm is not None) else sb
        for o, m in mk15.items():
            m["strike"] = spot_at(o)

        def leg_state(mks, dur, t):
            o = (t // dur) * dur
            m = mks.get(o)
            if m is None:
                return None
            rem = m["close"] - t
            if rem < MIN_REMAINING_S or t - o < MIN_ELAPSED_S:
                return None
            bk = books.get(m["asset_id"])
            if bk is None:
                return None
            st = bk.at(t)
            if st is None:
                return None
            strike = m["strike"]
            if strike is None or not np.isfinite(strike):
                return None
            s = spot_at(t)
            v = vol_bar_at(t, dur)
            if not (np.isfinite(s) and np.isfinite(v) and v > 0):
                return None
            p = bsm_p_up(s, strike, rem, v, float(dur))
            if p is None:
                return None
            eu, ed, sz_u, sz_d = edges_for(st, p)
            return {"m": m, "rem": rem, "p": p, "edge_up": eu, "edge_dn": ed,
                    "sz_up": sz_u, "sz_dn": sz_d, "bid": st[0], "ask": st[1]}

        def joint_at(t):
            """Best opposite-direction pair at instant t, or None."""
            l5 = leg_state(mk5, 300, t)
            l15 = leg_state(mk15, 900, t)
            if l5 is None or l15 is None:
                return None, l5 is not None and l15 is not None
            cands = []
            if l5["edge_up"] > 0 and l15["edge_dn"] > 0:
                cands.append(("up", "dn", l5["edge_up"] + l15["edge_dn"]))
            if l5["edge_dn"] > 0 and l15["edge_up"] > 0:
                cands.append(("dn", "up", l5["edge_dn"] + l15["edge_up"]))
            if not cands:
                return None, True
            d5, d15, comb = max(cands, key=lambda c: c[2])
            return {"l5": l5, "l15": l15, "d5": d5, "d15": d15, "comb": comb}, True

        tier_seconds = {tier: 0 for tier in TIERS}
        onsets = {tier: 0 for tier in TIERS}
        n_grid_both = 0
        traded_keys = set()
        trades = []
        persistences = []
        prev_comb = 0.0
        t = day_start
        while t < day_start + 86400:
            j, both = joint_at(t)
            comb = j["comb"] if j else 0.0
            if both:
                n_grid_both += 1
            for tier in TIERS:
                if comb >= tier:
                    tier_seconds[tier] += GRID_STEP_S
                    if prev_comb < tier:
                        onsets[tier] += 1
            if j and comb >= TRADE_TIER and prev_comb < TRADE_TIER:
                dur_s = 0
                tt = t + 1
                while tt < day_start + 86400:
                    jj, _ = joint_at(tt)
                    if jj is None or jj["comb"] < TRADE_TIER or \
                       jj["d5"] != j["d5"] or jj["d15"] != j["d15"]:
                        break
                    dur_s += 1
                    tt += 1
                persistences.append(dur_s)

                l5, l15 = j["l5"], j["l15"]
                key = (l5["m"]["asset_id"], l15["m"]["asset_id"], j["d5"], j["d15"])
                rec = {
                    "date": date, "t": t, "d5": j["d5"], "d15": j["d15"],
                    "comb_edge": round(comb, 4),
                    "edge5": round(l5["edge_up"] if j["d5"] == "up" else l5["edge_dn"], 4),
                    "edge15": round(l15["edge_up"] if j["d15"] == "up" else l15["edge_dn"], 4),
                    "rem5": l5["rem"], "rem15": l15["rem"],
                    "p5": round(l5["p"], 4), "p15": round(l15["p"], 4),
                    "ask5": l5["ask"] if j["d5"] == "up" else round(1 - l5["bid"], 4),
                    "ask15": l15["ask"] if j["d15"] == "up" else round(1 - l15["bid"], 4),
                    "depth5_sh": l5["sz_up"] if j["d5"] == "up" else l5["sz_dn"],
                    "depth15_sh": l15["sz_up"] if j["d15"] == "up" else l15["sz_dn"],
                    "persist_s": dur_s,
                    "slug5": l5["m"]["slug"], "slug15": l15["m"]["slug"],
                }
                if key not in traded_keys:
                    traded_keys.add(key)
                    pnl = 0.0
                    for leg, d in (("5", j["d5"]), ("15", j["d15"])):
                        m = (l5 if leg == "5" else l15)["m"]
                        ask = rec[f"ask{leg}"]
                        if ask <= 0 or ask >= 1:
                            continue
                        shares = STAKE_PER_LEG / ask
                        won = (m["outcome"] == "Up") == (d == "up")
                        pnl += shares * ((1.0 if won else 0.0) - ask)
                    rec["pnl"] = round(pnl, 2)
                    rec["traded"] = True
                    trades.append(rec)
                else:
                    rec["traded"] = False
                events_f.write(json.dumps(rec) + "\n")
            prev_comb = comb
            t += GRID_STEP_S

        day_pnl = sum(tr["pnl"] for tr in trades)
        pers = np.array(persistences) if persistences else np.array([0.0])
        se = np.array(strike_err) if strike_err else np.array([0.0])
        summary = {
            "date": date,
            "markets_5m": len(mk5), "markets_15m": len(mk15),
            "missing_books": missing,
            "grid_pts_both_priced": n_grid_both,
            "onsets": {f"{int(t_*100)}c": onsets[t_] for t_ in TIERS},
            "seconds_above": {f"{int(t_*100)}c": tier_seconds[t_] for t_ in TIERS},
            "trades": len(trades),
            "day_pnl_usd": round(day_pnl, 2),
            "persist_s_med": float(np.median(pers)),
            "persist_s_p25": float(np.percentile(pers, 25)),
            "persist_s_p75": float(np.percentile(pers, 75)),
            "strike_proxy_err_bps_med_abs": float(np.median(np.abs(se))),
        }
        day_summaries.append(summary)
        print(json.dumps(summary), flush=True)

    events_f.close()
    pnls = np.array([s["day_pnl_usd"] for s in day_summaries], dtype=float)
    print(json.dumps({
        "total_pnl": float(pnls.sum()),
        "daily_mean": float(pnls.mean()),
        "daily_std": float(pnls.std(ddof=1)) if len(pnls) > 1 else 0.0,
        "ann_sharpe": float(pnls.mean() / pnls.std(ddof=1) * math.sqrt(365))
        if len(pnls) > 1 and pnls.std(ddof=1) > 0 else None,
    }))


if __name__ == "__main__":
    main()
