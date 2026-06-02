#!/usr/bin/env python3
"""Cluster diagnostics for CompetitorRecycler market failures.

Joins per-market strategy output with local Polymarket book snapshots,
optional Polymarket trades, and Binance spot tape. The output is intentionally
flat CSV plus a compact markdown summary so failed days can be compared against
adjacent profitable days without re-running the Rust engine.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
from collections import defaultdict
from pathlib import Path
from typing import Any

import pandas as pd


BOOK_ROOT = Path("data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25")
TRADE_ROOT = Path("data/cache/raw/telonex/exchange=polymarket/channel=trades")
SPOT_ROOT = Path("data/cache/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT")


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser()
    p.add_argument("--markets", required=True, type=Path)
    p.add_argument("--dates", default="2026-05-12,2026-05-13,2026-05-14")
    p.add_argument("--out-csv", required=True, type=Path)
    p.add_argument("--out-md", required=True, type=Path)
    return p.parse_args()


def read_markets(path: Path, dates: set[str]) -> list[dict[str, Any]]:
    rows = []
    with path.open() as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            date = dt.datetime.fromtimestamp(row["close_ts"], dt.UTC).date().isoformat()
            if date in dates:
                row["date"] = date
                rows.append(row)
    return rows


def as_float(s: pd.Series) -> pd.Series:
    return pd.to_numeric(s, errors="coerce")


def path_stats(values: pd.Series) -> tuple[float, float, float]:
    values = values.dropna()
    if len(values) < 3:
        return (0.0, 0.0, 0.0)
    diffs = values.diff().dropna()
    path_abs = diffs.abs().sum()
    net = abs(values.iloc[-1] - values.iloc[0])
    path_eff = float(net / path_abs) if path_abs > 0 else 0.0
    signs = diffs.where(diffs.abs() > 1e-12).dropna().map(lambda x: 1 if x > 0 else -1)
    flips = int((signs != signs.shift()).sum()) - 1 if len(signs) > 1 else 0
    flip_rate = flips / max(len(signs) - 1, 1) if len(signs) > 1 else 0.0
    return (path_eff, flip_rate, float(path_abs))


def realized_vol_bps(prices: pd.Series) -> float:
    prices = prices.dropna()
    if len(prices) < 3:
        return 0.0
    rets = prices.pct_change().dropna()
    return float(rets.std(ddof=0) * 10_000.0) if len(rets) else 0.0


def load_spot(date: str) -> pd.DataFrame:
    path = SPOT_ROOT / f"date={date}" / f"BTCUSDT-aggTrades-{date}.parquet"
    df = pd.read_parquet(
        path,
        columns=["transact_time_ms", "price", "quantity", "is_buyer_maker"],
    )
    # Some cached Binance files use microsecond values despite the legacy
    # `*_ms` column name.
    divisor = 1_000_000.0 if df["transact_time_ms"].max() > 10_000_000_000_000 else 1000.0
    df["ts_s"] = df["transact_time_ms"] / divisor
    df["price"] = as_float(df["price"])
    df["quantity"] = as_float(df["quantity"])
    return df.sort_values("ts_s")


def spot_window_features(spot: pd.DataFrame, open_s: int, close_s: int) -> dict[str, float]:
    win = spot[(spot["ts_s"] >= open_s) & (spot["ts_s"] <= close_s)]
    if win.empty:
        return {}
    last180 = win[win["ts_s"] >= close_s - 180]
    last30 = win[win["ts_s"] >= close_s - 30]
    buy_qty = win.loc[~win["is_buyer_maker"], "quantity"].sum()
    sell_qty = win.loc[win["is_buyer_maker"], "quantity"].sum()
    denom = buy_qty + sell_qty
    path_eff, flip_rate, path_abs = path_stats(win["price"])
    return {
        "spot_open": float(win["price"].iloc[0]),
        "spot_close": float(win["price"].iloc[-1]),
        "spot_net_bps": float((win["price"].iloc[-1] / win["price"].iloc[0] - 1.0) * 10_000.0),
        "spot_realized_vol_180s_bps": realized_vol_bps(last180["price"]),
        "spot_abs_ret_30s_bps": float(abs(last30["price"].iloc[-1] / last30["price"].iloc[0] - 1.0) * 10_000.0) if len(last30) > 1 else 0.0,
        "spot_path_efficiency": path_eff,
        "spot_sign_flip_rate": flip_rate,
        "spot_path_abs_bps": path_abs / win["price"].iloc[0] * 10_000.0 if win["price"].iloc[0] else 0.0,
        "spot_trade_count": float(len(win)),
        "spot_flow_imbal": float((buy_qty - sell_qty) / denom) if denom else 0.0,
    }


def book_path(date: str, asset_id: str) -> Path:
    return BOOK_ROOT / f"date={date}" / f"asset_id={asset_id}" / f"{asset_id}_{date}_book_snapshot_25.parquet"


def trade_path(date: str, asset_id: str) -> Path:
    return TRADE_ROOT / f"date={date}" / f"asset_id={asset_id}" / f"{asset_id}_{date}_trades.parquet"


def book_features(date: str, asset_id: str) -> dict[str, float]:
    path = book_path(date, asset_id)
    if not path.exists():
        return {"book_present": 0.0}
    cols = [
        "timestamp_us",
        "bid_price_0",
        "bid_size_0",
        "ask_price_0",
        "ask_size_0",
    ]
    df = pd.read_parquet(path, columns=cols)
    if df.empty:
        return {"book_present": 0.0}
    bid = as_float(df["bid_price_0"])
    ask = as_float(df["ask_price_0"])
    bid_size = as_float(df["bid_size_0"])
    ask_size = as_float(df["ask_size_0"])
    mid = (bid + ask) * 0.5
    spread = ask - bid
    pair_cost = 1.0 - spread
    path_eff, flip_rate, path_abs = path_stats(mid)
    attractive = pair_cost <= 0.970
    return {
        "book_present": 1.0,
        "book_rows": float(len(df)),
        "yes_mid_start": float(mid.dropna().iloc[0]) if mid.notna().any() else math.nan,
        "yes_mid_end": float(mid.dropna().iloc[-1]) if mid.notna().any() else math.nan,
        "yes_mid_min": float(mid.min()),
        "yes_mid_max": float(mid.max()),
        "yes_mid_range": float(mid.max() - mid.min()),
        "yes_mid_path_efficiency": path_eff,
        "yes_mid_sign_flip_rate": flip_rate,
        "yes_mid_path_abs": path_abs,
        "avg_spread": float(spread.mean()),
        "p90_spread": float(spread.quantile(0.90)),
        "avg_pair_cost": float(pair_cost.mean()),
        "min_pair_cost": float(pair_cost.min()),
        "attractive_pair_frac": float(attractive.mean()),
        "avg_top_bid_size": float(bid_size.mean()),
        "avg_top_ask_size": float(ask_size.mean()),
        "min_top_bid_size": float(bid_size.min()),
        "min_top_ask_size": float(ask_size.min()),
    }


def trade_features(date: str, asset_id: str) -> dict[str, float]:
    path = trade_path(date, asset_id)
    if not path.exists():
        return {"trade_present": 0.0}
    df = pd.read_parquet(path, columns=["price", "size", "side"])
    if df.empty:
        return {"trade_present": 0.0}
    df["price"] = as_float(df["price"])
    df["size"] = as_float(df["size"])
    notional = df["price"] * df["size"]
    side = df["side"].astype(str).str.lower()
    buy = df.loc[side == "buy", "size"].sum()
    sell = df.loc[side == "sell", "size"].sum()
    denom = buy + sell
    return {
        "trade_present": 1.0,
        "pm_trade_count": float(len(df)),
        "pm_trade_shares": float(df["size"].sum()),
        "pm_trade_notional": float(notional.sum()),
        "pm_trade_buy_sell_imbal": float((buy - sell) / denom) if denom else 0.0,
        "pm_trade_avg_price": float((notional.sum() / df["size"].sum())) if df["size"].sum() else 0.0,
    }


def strategy_features(row: dict[str, Any]) -> dict[str, float | str]:
    s = row["per_strategy"]["competitor_recycler"]
    fills = s.get("fills_detail", [])
    yes_pnl = 0.0
    no_pnl = 0.0
    yes_notional = 0.0
    no_notional = 0.0
    for f in fills:
        side = f["side"]
        yes_win = row["outcome_label"] in ("Up", "Yes")
        win = (side == "BuyYes" and yes_win) or (side == "BuyNo" and not yes_win)
        pnl = (f["shares"] if win else 0.0) - f["notional"] + f.get("rebate_usdc", 0.0)
        if side == "BuyYes":
            yes_pnl += pnl
            yes_notional += f["notional"]
        elif side == "BuyNo":
            no_pnl += pnl
            no_notional += f["notional"]
    return {
        "date": row["date"],
        "slug": row["slug"],
        "asset_id": row["asset_id"],
        "close_ts": row["close_ts"],
        "outcome": row["outcome_label"],
        "pnl": float(s["pnl_usdc"]),
        "fills": float(s["fills"]),
        "filled_notional": float(s["filled_notional_usdc"]),
        "yes_pnl": yes_pnl,
        "no_pnl": no_pnl,
        "yes_notional": yes_notional,
        "no_notional": no_notional,
    }


def percentile(series: pd.Series, q: float) -> float:
    series = series.dropna()
    return float(series.quantile(q)) if len(series) else 0.0


def write_summary(df: pd.DataFrame, path: Path) -> None:
    lines = ["# Competitor Recycler Cluster Microstructure", ""]
    lines.append("## Daily Aggregate")
    lines.append("")
    for date, g in df.groupby("date"):
        lines.append(
            f"- `{date}`: pnl `{g.pnl.sum():+.2f}`, markets `{len(g)}`, "
            f"active `{int((g.fills > 0).sum())}`, worst `{g.pnl.min():+.2f}`, "
            f"median `{g.pnl.median():+.2f}`, avg spot vol180 `{g.spot_realized_vol_180s_bps.mean():.3f}`, "
            f"avg spot flip `{g.spot_sign_flip_rate.mean():.3f}`, avg PM range `{g.yes_mid_range.mean():.3f}`, "
            f"avg attractive pair frac `{g.attractive_pair_frac.mean():.3f}`"
        )
    lines += ["", "## Worst Markets", ""]
    cols = [
        "date",
        "slug",
        "outcome",
        "pnl",
        "yes_pnl",
        "no_pnl",
        "fills",
        "spot_net_bps",
        "spot_realized_vol_180s_bps",
        "spot_sign_flip_rate",
        "spot_path_efficiency",
        "yes_mid_range",
        "yes_mid_sign_flip_rate",
        "attractive_pair_frac",
        "trade_present",
    ]
    for _, r in df.sort_values("pnl").head(20)[cols].iterrows():
        lines.append(
            "- "
            + ", ".join(
                f"{c}=`{r[c]:+.3f}`" if isinstance(r[c], float) else f"{c}=`{r[c]}`"
                for c in cols
            )
        )
    lines += ["", "## Notes", ""]
    lines.append(
        "- `attractive_pair_frac` is the fraction of book snapshots where mirrored bid-side pair cost is `<= 0.970`."
    )
    lines.append(
        "- Missing Polymarket trade files are marked with `trade_present=0`; book and Binance spot are the primary diagnostics here."
    )
    path.write_text("\n".join(lines) + "\n")


def main() -> int:
    args = parse_args()
    dates = {d.strip() for d in args.dates.split(",") if d.strip()}
    rows = read_markets(args.markets, dates)
    spots = {date: load_spot(date) for date in dates}
    out = []
    for i, row in enumerate(rows, 1):
        open_s = int(row["close_ts"]) - 300
        close_s = int(row["close_ts"])
        rec = strategy_features(row)
        rec.update(spot_window_features(spots[row["date"]], open_s, close_s))
        rec.update(book_features(row["date"], row["asset_id"]))
        rec.update(trade_features(row["date"], row["asset_id"]))
        out.append(rec)
        if i % 100 == 0:
            print(f"processed {i}/{len(rows)}", flush=True)
    df = pd.DataFrame(out)
    args.out_csv.parent.mkdir(parents=True, exist_ok=True)
    args.out_md.parent.mkdir(parents=True, exist_ok=True)
    df.to_csv(args.out_csv, index=False)
    write_summary(df, args.out_md)
    print(f"wrote {args.out_csv}")
    print(f"wrote {args.out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
