#!/usr/bin/env python3
"""Compare baseline vs treatment vol-gate ablation runs.

Reads the two summary.json + markets.jsonl outputs and prints a side-by-side
table (total P&L, P&L/day, participation, win rate, mean P&L/trade, max drawdown,
worst-market P&L, notional/fills) plus per-lane and per-volatility-band attribution.
"""
import json
import sys
from collections import defaultdict

DAYS = 14.0  # May 7-20 inclusive


def load(path):
    return json.load(open(path))


def mkts(path):
    rows = []
    for line in open(path):
        line = line.strip()
        if line:
            rows.append(json.loads(line))
    return rows


def bv(r):
    return r.get("per_strategy", {}).get("bonereaper_v2", {})


def participated(r):
    return bv(r).get("orders_filled", 0) > 0


def summarize_markets(rows):
    n = len(rows)
    traded = [r for r in rows if participated(r)]
    pnls = [bv(r).get("pnl_usdc", 0.0) for r in traded]
    wins = [p for p in pnls if p > 0]
    band_traded = defaultdict(int)
    for r in traded:
        band_traded[r.get("volatility_band", "?")] += 1
    return {
        "n_markets": n,
        "n_participated": len(traded),
        "participation_rate": len(traded) / n if n else 0.0,
        "win_rate_traded": len(wins) / len(pnls) if pnls else 0.0,
        "mean_pnl_per_trade": (sum(pnls) / len(pnls)) if pnls else 0.0,
        "worst_market_pnl": min(pnls) if pnls else 0.0,
        "traded_low_band": band_traded.get("Low", 0),
        "traded_high_band": band_traded.get("High", 0),
    }


def fmt(v, w=14, p=2):
    if isinstance(v, float):
        return f"{v:>{w}.{p}f}"
    return f"{str(v):>{w}}"


def main(base_dir, treat_dir):
    bs = load(f"{base_dir}/summary.json")["per_strategy"]["bonereaper_v2"]
    ts = load(f"{treat_dir}/summary.json")["per_strategy"]["bonereaper_v2"]
    bm = summarize_markets(mkts(f"{base_dir}/markets.jsonl"))
    tm = summarize_markets(mkts(f"{treat_dir}/markets.jsonl"))

    rows = [
        ("total_pnl_usdc", bs["total_pnl_usdc"], ts["total_pnl_usdc"]),
        ("pnl_per_day", bs["total_pnl_usdc"] / DAYS, ts["total_pnl_usdc"] / DAYS),
        ("end_equity", bs["last_end_equity_usdc"], ts["last_end_equity_usdc"]),
        ("compounded_return_pct", bs["compounded_return_pct"], ts["compounded_return_pct"]),
        ("max_drawdown_pct", bs["path_max_drawdown_pct"], ts["path_max_drawdown_pct"]),
        ("markets_with_orders", bs["markets_with_orders"], ts["markets_with_orders"]),
        ("participation_rate", bs["markets_with_orders"] / 4000.0, ts["markets_with_orders"] / 4000.0),
        ("hit_rate", bs["hit_rate"], ts["hit_rate"]),
        ("mean_pnl_usdc(all mkts)", bs["mean_pnl_usdc"], ts["mean_pnl_usdc"]),
        ("worst_market_pnl", bs["worst_market_pnl"], ts["worst_market_pnl"]),
        ("best_market_pnl", bs["best_market_pnl"], ts["best_market_pnl"]),
        ("sharpe_ratio", bs["sharpe_ratio"], ts["sharpe_ratio"]),
        ("total_orders_filled", bs["total_orders_filled"], ts["total_orders_filled"]),
        ("total_filled_notional_usdc", bs["total_filled_notional_usdc"], ts["total_filled_notional_usdc"]),
        ("avg_slippage_bps", bs["avg_slippage_bps"], ts["avg_slippage_bps"]),
    ]

    print(f"{'metric':<32}{'BASELINE':>16}{'TREATMENT':>16}{'DELTA':>16}")
    print("-" * 80)
    for name, b, t in rows:
        d = t - b
        print(f"{name:<32}{b:>16.3f}{t:>16.3f}{d:>+16.3f}")

    print("\n=== market-level (from markets.jsonl) ===")
    print(f"{'metric':<32}{'BASELINE':>16}{'TREATMENT':>16}")
    for k in bm:
        print(f"{k:<32}{bm[k]:>16.4f}{tm[k]:>16.4f}")

    print("\n=== lane attribution (by_fill_tag) ===")
    lanes = sorted(set(bs.get("by_fill_tag", {})) | set(ts.get("by_fill_tag", {})))
    print(f"{'lane':<22}{'B_fills':>9}{'B_pnl':>11}{'T_fills':>9}{'T_pnl':>11}{'dPnL':>11}")
    for ln in lanes:
        b = bs.get("by_fill_tag", {}).get(ln, {})
        t = ts.get("by_fill_tag", {}).get(ln, {})
        bp = b.get("total_pnl_usdc", 0.0)
        tp = t.get("total_pnl_usdc", 0.0)
        print(
            f"{ln:<22}{b.get('fills',0):>9}{bp:>11.2f}"
            f"{t.get('fills',0):>9}{tp:>11.2f}{tp-bp:>+11.2f}"
        )

    print("\n=== volatility band attribution ===")
    bb = load(f"{base_dir}/summary.json")["by_volatility_band"]
    tb = load(f"{treat_dir}/summary.json")["by_volatility_band"]
    print(f"{'band':<10}{'B_pnl':>12}{'B_mkts':>9}{'T_pnl':>12}{'T_mkts':>9}{'dPnL':>12}")
    for band in ("Low", "High"):
        b = bb.get(band, {}).get("bonereaper_v2", {})
        t = tb.get(band, {}).get("bonereaper_v2", {})
        bp = b.get("total_pnl_usdc", 0.0)
        tp = t.get("total_pnl_usdc", 0.0)
        print(
            f"{band:<10}{bp:>12.2f}{b.get('markets_with_orders',0):>9}"
            f"{tp:>12.2f}{t.get('markets_with_orders',0):>9}{tp-bp:>+12.2f}"
        )


if __name__ == "__main__":
    base = sys.argv[1] if len(sys.argv) > 1 else "data/runs/volgate/baseline"
    treat = sys.argv[2] if len(sys.argv) > 2 else "data/runs/volgate/treatment"
    main(base, treat)
