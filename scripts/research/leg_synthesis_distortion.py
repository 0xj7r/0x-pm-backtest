"""Quantify backtest distortion from synthesizing the NO book as 1 - yes_mid.

The walk-forward engine loads only one leg's book per market and prices BuyNo as
(1 - yes_mid) (runner.rs:1797). This script fetches the REAL down-token book for a
sample of markets and measures how far real NO quotes / real pair cost
(yes_ask + no_ask) deviate from the synthetic assumption (which implies pair
cost == 1.0 exactly, with no spread and no cheap-leg arb).

Usage: python scripts/leg_synthesis_distortion.py [pairs.json] [date]
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

import pandas as pd
import pyarrow.parquet as pq

BUCKET = "s3://pm-research-data-prod"
PREFIX = "raw/telonex/exchange=polymarket/channel=book_snapshot_25"
PROFILE = "visumlabs"
CACHE = "data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25"
TMP = "/tmp/down_books"
COLS = ["timestamp_us", "bid_price_0", "ask_price_0"]


def local_up_path(asset: str, date: str) -> str | None:
    d = f"{CACHE}/date={date}/asset_id={asset}"
    if not os.path.isdir(d):
        return None
    fs = [f for f in os.listdir(d) if f.endswith(".parquet")]
    return os.path.join(d, fs[0]) if fs else None


def fetch_down(asset: str, date: str) -> str | None:
    fn = f"{asset}_{date}_book_snapshot_25.parquet"
    dst = f"{TMP}/{fn}"
    if os.path.exists(dst):
        return dst
    os.makedirs(TMP, exist_ok=True)
    key = f"{BUCKET}/{PREFIX}/date={date}/asset_id={asset}/{fn}"
    r = subprocess.run(["aws", "s3", "cp", key, dst, "--profile", PROFILE,
                        "--region", "us-east-1", "--no-progress"],
                       capture_output=True, text=True)
    return dst if r.returncode == 0 and os.path.exists(dst) else None


def load_top(path: str) -> pd.DataFrame:
    df = pq.ParquetFile(path).read(columns=COLS).to_pandas()
    for c in ("bid_price_0", "ask_price_0"):
        df[c] = pd.to_numeric(df[c], errors="coerce")
    df["timestamp_us"] = pd.to_numeric(df["timestamp_us"], errors="coerce")
    df = df.dropna(subset=["timestamp_us"]).sort_values("timestamp_us")
    return df


def main() -> int:
    pairs_path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/leg_pairs_may14.json"
    date = sys.argv[2] if len(sys.argv) > 2 else "2026-05-14"
    pairs = json.load(open(pairs_path))

    rows = []
    n_markets = n_missing_down = 0
    for p in pairs:
        up_path = local_up_path(p["up"], date)
        if not up_path:
            continue
        dn_path = fetch_down(p["down"], date)
        if not dn_path:
            n_missing_down += 1
            continue
        up = load_top(up_path).rename(columns={"bid_price_0": "yes_bid", "ask_price_0": "yes_ask"})
        dn = load_top(dn_path).rename(columns={"bid_price_0": "no_bid", "ask_price_0": "no_ask"})
        if up.empty or dn.empty:
            continue
        # Align each NO snapshot to the prevailing YES quote (asof backward).
        m = pd.merge_asof(dn, up, on="timestamp_us", direction="backward").dropna(
            subset=["yes_bid", "yes_ask", "no_bid", "no_ask"])
        if m.empty:
            continue
        m["yes_mid"] = (m.yes_bid + m.yes_ask) / 2
        m["synth_no"] = 1.0 - m.yes_mid           # what the sim charges for BuyNo
        m["no_ask_err"] = m.no_ask - m.synth_no    # real - synthetic
        m["pair_ask"] = m.yes_ask + m.no_ask       # real taker cost both legs
        rows.append(m[["yes_ask", "no_ask", "yes_mid", "synth_no", "no_ask_err", "pair_ask"]])
        n_markets += 1

    if not rows:
        print("no data aligned"); return 1
    a = pd.concat(rows, ignore_index=True)
    print(f"markets compared: {n_markets} (down-book missing: {n_missing_down}) · snapshots: {len(a):,}\n")
    print("REAL pair cost (yes_ask + no_ask) vs synthetic's implied 1.000:")
    print(f"  mean {a.pair_ask.mean():.4f} · median {a.pair_ask.median():.4f} · "
          f"p10 {a.pair_ask.quantile(.1):.4f} · p90 {a.pair_ask.quantile(.9):.4f}")
    print(f"  > 1.02 (real spread cost the sim ignores): {(a.pair_ask>1.02).mean()*100:.1f}%")
    print(f"  > 1.05: {(a.pair_ask>1.05).mean()*100:.1f}%")
    print(f"  < 1.00 (real cheap-leg arb the sim can't see): {(a.pair_ask<1.0).mean()*100:.1f}%")
    print(f"  < 0.98: {(a.pair_ask<0.98).mean()*100:.1f}%")
    print("\nNO-ask synthesis error (real_no_ask - (1 - yes_mid)):")
    print(f"  mean {a.no_ask_err.mean():+.4f} · median {a.no_ask_err.median():+.4f} · "
          f"mean abs {a.no_ask_err.abs().mean():.4f} · p90 abs {a.no_ask_err.abs().quantile(.9):.4f}")
    print(f"  |err| > 0.02: {(a.no_ask_err.abs()>0.02).mean()*100:.1f}% · "
          f"|err| > 0.05: {(a.no_ask_err.abs()>0.05).mean()*100:.1f}%")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
