#!/usr/bin/env python3
"""Compute daily PnL + aligned average signals from markets JSONL and decision log JSONL.

Useful to find leading indicators for daily PnL / DD (e.g., high whipsaw or reversal_pressure on days before negative PnL).

Usage:
  python scripts/daily_pnl_and_leading_signals.py \
    --markets data/runs/.../markets.jsonl \
    --decision-log data/runs/.../decision_log.jsonl \
    --log data/runs/.../log.txt \
    --strategy back_to_explore
"""
import argparse
import datetime as dt
import json
from collections import defaultdict
from pathlib import Path
from typing import Any, Dict, List

def day_key(ts: int) -> str:
    return dt.datetime.fromtimestamp(ts, tz=dt.timezone.utc).date().isoformat()

def main():
    p = argparse.ArgumentParser()
    p.add_argument("--markets", required=True)
    p.add_argument("--decision-log", required=True)
    p.add_argument("--log", help="Optional backtest log.txt to parse STRAT_SIGNAL eprints for ladder internals (net_vs_target etc)")
    p.add_argument("--strategy", default="back_to_explore")
    args = p.parse_args()

    # Daily PnL from markets
    daily_pnl: Dict[str, float] = defaultdict(float)
    daily_markets: Dict[str, int] = defaultdict(int)
    with open(args.markets) as f:
        for line in f:
            if not line.strip(): continue
            row = json.loads(line)
            strat = (row.get("per_strategy") or {}).get(args.strategy) or {}
            if not strat: continue
            close_ts = row.get("close_ts")
            if close_ts is None:
                slug = row.get("slug", "")
                try:
                    close_ts = int(slug.rsplit("-", 1)[1]) + 300
                except:
                    continue
            d = day_key(int(close_ts))
            daily_pnl[d] += float(strat.get("pnl_usdc", 0))
            daily_markets[d] += 1

    # Signals from decision log (features at decision time). Keys match DecisionLogRow fields (feature_* + top level scores).
    # spot_ret_* may be present via model_context in some rows; feature_* always for this log.
    # Use sum+cnt (streaming, low mem) not full lists; supports large logs.
    daily_signal_stats: Dict[str, Dict[str, Dict[str, float]]] = defaultdict(lambda: defaultdict(lambda: {"sum": 0.0, "cnt": 0}))
    signal_keys = [
        "feature_whipsaw", "feature_path_risk", "feature_markov_reversal_risk",
        "feature_dir_flip_rate_8", "feature_volatility_regime",
        "confidence_score", "risk_score", "direction_score",
        "feature_spot_score", "feature_spot_fast_momentum", "feature_spot_broad_momentum",
        "spot_ret_5s", "spot_ret_15s", "spot_ret_30s",
        "ladder_net", "net_vs_target", "pair_sig", "target_net", "window_delta_bps"
    ]
    with open(args.decision_log) as f:
        for line in f:
            if not line.strip(): continue
            row = json.loads(line)
            ts = row.get("ts_ns") or row.get("ts") or row.get("decision_ts")
            if ts is None: continue
            d = day_key(int(ts) // 1_000_000_000)  # ns to sec
            for k in signal_keys:
                v = row.get(k)
                if v is not None:
                    st = daily_signal_stats[d][k]
                    st["sum"] += float(v)
                    st["cnt"] += 1

    # Optional: parse STRAT_SIGNAL from full log.txt (includes ts_ns for day align + ladder internals like net_vs_target, pair_sig)
    strat_signal_keys = ["ladder_net", "target_net", "net_vs_target", "pair_sig", "time_mult", "directional"]
    if args.log:
        import re
        strat_re = re.compile(r"STRAT_SIGNAL ts_ns=(\d+)(?: window_delta_bps=([-\d.]+))? ladder_net=([-\d.]+) target_net=([-\d.]+) net_vs_target=([-\d.]+) pair_sig=([-\d.]+) time_mult=([-\d.]+) directional=([-\d.]+)")
        with open(args.log) as f:
            for line in f:
                m = strat_re.search(line)
                if not m: continue
                ts_ns = int(m.group(1))
                d = day_key(ts_ns // 1_000_000_000)
                # groups: 1=ts, 2=delta (opt), 3=ladder,4=target,5=net_vs,6=pair,7=time,8=dir
                delta = float(m.group(2)) if m.group(2) else 0.0
                vals = [delta] + [float(m.group(i)) for i in range(3,9)]
                keys = ["window_delta_bps"] + strat_signal_keys
                for k, v in zip(keys, vals):
                    if k in daily_signal_stats[d] or k not in ("window_delta_bps",) or True:
                        st = daily_signal_stats[d][k]
                        st["sum"] += v
                        st["cnt"] += 1

    # Print table (aligned daily PnL + avg signals at decision times that day)
    print("date,daily_pnl_usdc,markets,avg_whipsaw,avg_reversal,avg_net_vs,avg_pair_sig,avg_win_delta")
    for d in sorted(daily_pnl.keys()):
        pnl = daily_pnl[d]
        mkt = daily_markets[d]
        sigs = daily_signal_stats.get(d, {})
        def avg(k):
            st = sigs.get(k, {"sum":0.0,"cnt":0})
            return (st["sum"]/st["cnt"]) if st["cnt"] > 0 else 0.0
        print(f"{d},{pnl:.2f},{mkt},{avg('feature_whipsaw'):.4f},{avg('feature_markov_reversal_risk'):.4f},{avg('net_vs_target'):.2f},{avg('pair_sig'):.3f},{avg('window_delta_bps'):.1f}")

    # Simple leading signal analysis: avg features on negative vs positive PnL days
    # Use these diffs to add preemptive logic in strategy (e.g. if reversal_risk high on bad days -> cut size or force pair)
    neg_pnls = [d for d, p in daily_pnl.items() if p < 0]
    pos_pnls = [d for d, p in daily_pnl.items() if p > 0]
    print("\n=== Potential leading signals (avg on neg-PnL days vs pos-PnL days) ===")
    print("Use diffs e.g. high net_vs_target or reversal on neg days -> add preemptive size cut / pair force in strategy")
    for k in signal_keys:
        neg_sum = 0.0
        neg_cnt = 0
        pos_sum = 0.0
        pos_cnt = 0
        for d in neg_pnls:
            st = daily_signal_stats.get(d, {}).get(k, {"sum":0.0,"cnt":0})
            neg_sum += st["sum"]
            neg_cnt += st["cnt"]
        for d in pos_pnls:
            st = daily_signal_stats.get(d, {}).get(k, {"sum":0.0,"cnt":0})
            pos_sum += st["sum"]
            pos_cnt += st["cnt"]
        if neg_cnt or pos_cnt:
            n = neg_sum / neg_cnt if neg_cnt else 0.0
            p = pos_sum / pos_cnt if pos_cnt else 0.0
            print(f"{k}: neg_pnl_days={n:.4f} pos_pnl_days={p:.4f} diff={n-p:.4f}")

if __name__ == "__main__":
    main()
