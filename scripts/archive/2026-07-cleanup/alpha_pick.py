#!/usr/bin/env python3
"""Rank pm-alpha tune-grid results (data/runs/alpha/tune/*.json)."""
import json
import sys
from pathlib import Path

def rows(path: Path):
    d = json.loads(path.read_text())
    vol = d["model_cfg"]["vol_lookback_s"]
    mom = d["model_cfg"]["momentum_lookback_s"]
    mw = d["model_cfg"]["momentum_weight"]
    for e in d["sweep"]:
        a = e["report"]["aggregate"]
        yield {
            "vol": vol, "mom": mom, "mw": mw,
            "lat": e["latency_ms"], "thr": e["edge_threshold"],
            "n": a["n_markets"], "trades": a["n_trades"],
            "pnl": a["total_pnl"], "per": a["mean_pnl_per_trade"],
            "hit": a["hit_rate"], "ll_exo": a["log_loss_exo"],
            "ll_book": a["log_loss_book"], "samples": a["n_samples"],
        }

def main():
    paths = [Path(p) for p in sys.argv[1:]] or sorted(Path("data/runs/alpha/tune").glob("*.json"))
    all_rows = [r for p in paths for r in rows(p)]
    if not all_rows:
        sys.exit("no tune jsons found")
    all_rows.sort(key=lambda r: -r["pnl"])
    hdr = f"{'vol':>5} {'mom':>4} {'mw':>4} {'lat':>5} {'thr':>6} {'trades':>6} {'PnL$':>9} {'per$':>8} {'hit%':>5} {'LLexo':>7} {'LLbook':>7}"
    print(hdr)
    for r in all_rows:
        print(f"{r['vol']:>5} {r['mom']:>4} {r['mw']:>4} {r['lat']:>5} {r['thr']:>6.3f} "
              f"{r['trades']:>6} {r['pnl']:>9.2f} {r['per']:>8.4f} {100*r['hit']:>5.1f} "
              f"{r['ll_exo']:>7.4f} {r['ll_book']:>7.4f}")
    at150 = [r for r in all_rows if r["lat"] == 150]
    if at150:
        best = max(at150, key=lambda r: r["pnl"])
        print(f"\nbest @150ms: vol={best['vol']} thr={best['thr']} pnl={best['pnl']:.2f} "
              f"({best['trades']} trades, hit {100*best['hit']:.1f}%, ll_exo {best['ll_exo']:.4f} vs book {best['ll_book']:.4f})")
    print(f"\nconfigs evaluated: {len(all_rows)} (multiple-testing disclosure)")

if __name__ == "__main__":
    main()
