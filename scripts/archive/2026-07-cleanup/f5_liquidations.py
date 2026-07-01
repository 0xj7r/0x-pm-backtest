#!/usr/bin/env python3
"""F5: Do Binance BTC futures liquidation cascades predict short-horizon
continuation, and do fade losses concentrate during cascades?

Ground truth: Tardis free first-of-month liquidation tapes (3 days).
Proxy: futures aggTrades taker-flow bursts (25 days), validated vs ground truth.
Join: feemin base/W1_base/W2_base trades, window_secs=300 only.
"""
import glob
import gzip
import json
import os
import zipfile
from collections import defaultdict

import numpy as np
import pandas as pd

ROOT = "/Users/jackreid/go/polymarket-backtest"
LIQ = f"{ROOT}/data/external/liquidations"
TRUE_DAYS = ["2026-03-01", "2026-04-01", "2026-05-01"]
HORIZONS = [60, 180, 300]
BUCKET = 10  # seconds

agg_days = sorted(
    os.path.basename(p).split("-aggTrades-")[1][:10]
    for p in glob.glob(f"{LIQ}/aggtrades/*.zip")
)


def day_bucket_range(day):
    t0 = int(pd.Timestamp(day, tz="UTC").timestamp())
    return t0 // BUCKET, (t0 + 86400) // BUCKET


def load_liq_10s(day):
    """True liquidations -> {bucket: [buy_notional, sell_notional]}."""
    out = defaultdict(lambda: [0.0, 0.0])
    with gzip.open(f"{LIQ}/liq_{day}.csv.gz", "rt") as f:
        df = pd.read_csv(f)
    df["bucket"] = (df["timestamp"] // 1_000_000) // BUCKET
    df["notional"] = df["price"] * df["amount"]
    for (b, side), v in df.groupby(["bucket", "side"])["notional"].sum().items():
        # liq order side 'buy' = short liquidated (upward pressure)
        out[b][0 if side == "buy" else 1] += v
    return out


def load_agg_10s(day):
    """Futures aggTrades -> DataFrame[bucket, taker_buy, taker_sell] notional."""
    path = f"{LIQ}/aggtrades/BTCUSDT-aggTrades-{day}.zip"
    with zipfile.ZipFile(path) as z:
        with z.open(z.namelist()[0]) as f:
            df = pd.read_csv(
                f,
                usecols=["price", "quantity", "transact_time", "is_buyer_maker"],
                dtype={"price": float, "quantity": float, "transact_time": np.int64},
            )
    df["bucket"] = (df["transact_time"] // 1000) // BUCKET
    df["notional"] = df["price"] * df["quantity"]
    # is_buyer_maker True => taker sold
    g = df.groupby(["bucket", "is_buyer_maker"])["notional"].sum().unstack(fill_value=0.0)
    g = g.rename(columns={False: "taker_buy", True: "taker_sell"}).reset_index()
    for c in ("taker_buy", "taker_sell"):
        if c not in g:
            g[c] = 0.0
    return g[["bucket", "taker_buy", "taker_sell"]]


def load_spot_closes(day):
    """Spot 1s klines -> {epoch_second: close}."""
    path = f"{LIQ}/spot1s/BTCUSDT-1s-{day}.zip"
    with zipfile.ZipFile(path) as z:
        with z.open(z.namelist()[0]) as f:
            df = pd.read_csv(f, header=None, usecols=[0, 4], names=["open_us", "close"])
    sec = (df["open_us"] // 1_000_000).to_numpy()
    return dict(zip(sec, df["close"].to_numpy()))


def fwd_signed_returns(events, closes_by_day):
    """events: list of (day, t0_sec, dirsign). Returns {h: np.array of signed bps}."""
    out = {h: [] for h in HORIZONS}
    for day, t0, sgn in events:
        closes = closes_by_day[day]
        p0 = closes.get(t0) or closes.get(t0 - 1)
        if p0 is None:
            continue
        for h in HORIZONS:
            ph = closes.get(t0 + h) or closes.get(t0 + h - 1)
            if ph is None:
                continue
            out[h].append(sgn * (ph / p0 - 1.0) * 1e4)
    return {h: np.array(v) for h, v in out.items()}


def tstat(x):
    if len(x) < 2:
        return float("nan")
    return x.mean() / (x.std(ddof=1) / np.sqrt(len(x)))


def episodes(buckets, gap_buckets=3):
    """Collapse sorted cascade buckets into episodes; return first bucket of each."""
    eps = []
    prev = None
    for b in sorted(buckets):
        if prev is None or b - prev > gap_buckets:
            eps.append(b)
        prev = b
    return eps


def main():
    print("=== Phase 1: ground truth (Tardis liq, 3 free days) ===")
    liq_by_day = {d: load_liq_10s(d) for d in TRUE_DAYS}
    closes = {d: load_spot_closes(d) for d in TRUE_DAYS}

    totals, per_bucket = [], {}
    for d in TRUE_DAYS:
        lo, hi = day_bucket_range(d)
        for b in range(lo, hi):
            buy, sell = liq_by_day[d].get(b, (0.0, 0.0))
            totals.append(buy + sell)
            per_bucket[(d, b)] = (buy, sell)
    totals = np.array(totals)
    p99 = np.percentile(totals, 99)
    nz = totals[totals > 0]
    print(f"buckets={len(totals)} nonzero={len(nz)} p99_all=${p99:,.0f} "
          f"p99_nonzero=${np.percentile(nz, 99):,.0f}")

    casc = {k: v for k, v in per_bucket.items() if sum(v) > p99}
    print(f"cascade buckets: {len(casc)}")

    ev_bucket, casc_by_day = [], defaultdict(list)
    for (d, b), (buy, sell) in casc.items():
        sgn = 1 if buy > sell else -1
        ev_bucket.append((d, (b + 1) * BUCKET, sgn))
        casc_by_day[d].append((b, sgn))
    ev_ep = []
    for d, items in casc_by_day.items():
        bs = {b: s for b, s in items}
        for b in episodes(bs.keys()):
            ev_ep.append((d, (b + 1) * BUCKET, bs[b]))

    for label, evs in [("bucket-level", ev_bucket), ("episode-level", ev_ep)]:
        rets = fwd_signed_returns(evs, closes)
        print(f"-- {label} (n={len(evs)}) signed fwd return (bps, +=continuation)")
        for h in HORIZONS:
            r = rets[h]
            print(f"  {h:>3}s: mean={r.mean():+6.2f} med={np.median(r):+6.2f} "
                  f"t={tstat(r):+5.2f} n={len(r)}")

    for sgn, name in [(1, "buy(shorts liq, up)"), (-1, "sell(longs liq, down)")]:
        evs = [e for e in ev_ep if e[2] == sgn]
        rets = fwd_signed_returns(evs, closes)
        line = " ".join(
            f"{h}s:{rets[h].mean():+.1f}(t={tstat(rets[h]):+.1f},n={len(rets[h])})"
            for h in HORIZONS)
        print(f"  episodes dir={name}: {line}")

    # baseline: random non-cascade buckets, direction by liq side if any else taker flow sign
    rng = np.random.default_rng(7)
    base_ev = []
    for d in TRUE_DAYS:
        lo, hi = day_bucket_range(d)
        for b in rng.choice(np.arange(lo, hi - 30), size=400, replace=False):
            buy, sell = per_bucket.get((d, int(b)), (0.0, 0.0))
            if buy + sell > p99:
                continue
            base_ev.append((d, (int(b) + 1) * BUCKET, 1 if buy >= sell else -1))
    rets = fwd_signed_returns(base_ev, closes)
    print(f"-- baseline random buckets (n={len(base_ev)})")
    for h in HORIZONS:
        r = rets[h]
        print(f"  {h:>3}s: mean={r.mean():+6.2f} t={tstat(r):+5.2f}")

    print("\n=== Phase 2: aggTrades proxy (all days), validate on true days ===")
    agg = {}
    for d in agg_days:
        g = load_agg_10s(d)
        g.to_csv(f"{LIQ}/derived/agg10s_{d}.csv", index=False)
        agg[d] = g
        print(f"  {d}: {len(g)} buckets", flush=True)

    allcat = pd.concat(agg.values())
    tot = (allcat["taker_buy"] + allcat["taker_sell"]).to_numpy()
    agg_p99 = np.percentile(tot, 99)
    print(f"proxy threshold p99(total taker notional/10s)=${agg_p99:,.0f}")

    proxy = {}  # day -> {bucket: dirsign}
    for d, g in agg.items():
        m = g[(g["taker_buy"] + g["taker_sell"]) > agg_p99]
        proxy[d] = {
            int(r.bucket): (1 if r.taker_buy > r.taker_sell else -1)
            for r in m.itertuples()
        }

    # validation: true cascade buckets flagged by proxy within +/-1 bucket
    hits = same_dir = tot_true = 0
    for d in TRUE_DAYS:
        for b, sgn in casc_by_day[d]:
            tot_true += 1
            for off in (0, -1, 1):
                if b + off in proxy[d]:
                    hits += 1
                    same_dir += proxy[d][b + off] == sgn
                    break
    print(f"true cascades flagged by proxy: {hits}/{tot_true} "
          f"({100*hits/max(tot_true,1):.0f}%), direction match {same_dir}/{hits}")
    nproxy = sum(len(v) for v in proxy.values())
    print(f"proxy cascade buckets across {len(agg_days)} days: {nproxy} "
          f"({nproxy/len(agg_days):.0f}/day)")

    # proxy continuation on true days (sanity that proxy carries same signal)
    evp = []
    for d in TRUE_DAYS:
        bs = proxy[d]
        for b in episodes(bs.keys()):
            evp.append((d, (b + 1) * BUCKET, bs[b]))
    rets = fwd_signed_returns(evp, closes)
    line = " ".join(f"{h}s:{rets[h].mean():+.1f}(t={tstat(rets[h]):+.1f})"
                    for h in HORIZONS)
    print(f"proxy episodes on true days (n={len(evp)}): {line}")

    print("\n=== Phase 3: trade join (window_secs=300) ===")
    trade_files = ["base", "W1_base", "W2_base"]
    rows = []
    covered = set(agg_days)
    for tf in trade_files:
        with open(f"{ROOT}/data/runs/alpha/feemin/{tf}.trades.jsonl") as fh:
            for line in fh:
                t = json.loads(line)
                if t.get("window_secs") != 300:
                    continue
                sec = t["decision_ts_ns"] // 1_000_000_000
                day = str(pd.Timestamp(sec, unit="s", tz="UTC").date())
                if day not in covered:
                    continue
                rows.append({
                    "file": tf, "day": day, "sec": sec,
                    "pnl": t["pnl"], "won": bool(t["won"]), "side": t["side"],
                })
    tr = pd.DataFrame(rows)
    print(f"trades on covered days: {len(tr)} across {tr['day'].nunique()} days")

    def flag(row, lookback=300):
        bmap = proxy.get(row["day"], {})
        b_now = row["sec"] // BUCKET
        for b in range(b_now - lookback // BUCKET, b_now + 1):
            if b in bmap:
                return bmap[b]
        return 0

    tr["casc_dir"] = tr.apply(flag, axis=1)
    tr["flagged"] = tr["casc_dir"] != 0
    tr["against"] = ((tr["casc_dir"] == 1) & (tr["side"] == "No")) | \
                    ((tr["casc_dir"] == -1) & (tr["side"] == "Yes"))

    def report(df, name):
        if len(df) == 0:
            print(f"  {name}: n=0")
            return
        losses = df.loc[df["pnl"] < 0, "pnl"].sum()
        print(f"  {name}: n={len(df)} win%={100*df['won'].mean():.1f} "
              f"avg_pnl={df['pnl'].mean():+.3f} tot_pnl={df['pnl'].sum():+,.0f} "
              f"tot_losses={losses:+,.0f}")

    for tf in trade_files + ["ALL"]:
        sub = tr if tf == "ALL" else tr[tr["file"] == tf]
        print(f"[{tf}]")
        report(sub[~sub["flagged"]], "no-cascade   ")
        report(sub[sub["flagged"]], "post-cascade ")
        report(sub[sub["flagged"] & sub["against"]], " vs-cascade  ")
        report(sub[sub["flagged"] & ~sub["against"]], " with-cascade")

    fl = tr[tr["flagged"]]
    share_n = len(fl) / len(tr)
    share_loss = fl.loc[fl["pnl"] < 0, "pnl"].sum() / tr.loc[tr["pnl"] < 0, "pnl"].sum()
    print(f"\nflagged trades = {100*share_n:.1f}% of trades, "
          f"{100*share_loss:.1f}% of total losses")

    # filter economics
    print("\nfilter economics (drop trades within 300s after proxy cascade):")
    for tf in trade_files:
        sub = tr[tr["file"] == tf]
        kept = sub[~sub["flagged"]]
        print(f"  {tf}: pnl {sub['pnl'].sum():+,.0f} -> {kept['pnl'].sum():+,.0f} "
              f"(drop {sub['flagged'].sum()} trades, "
              f"{sub.loc[sub['flagged'],'pnl'].sum():+,.0f} pnl removed)")
    print("\nfilter economics (drop only vs-cascade trades):")
    for tf in trade_files:
        sub = tr[tr["file"] == tf]
        drop = sub["flagged"] & sub["against"]
        print(f"  {tf}: pnl {sub['pnl'].sum():+,.0f} -> {sub.loc[~drop,'pnl'].sum():+,.0f} "
              f"(drop {drop.sum()} trades, {sub.loc[drop,'pnl'].sum():+,.0f} pnl removed)")


if __name__ == "__main__":
    main()
