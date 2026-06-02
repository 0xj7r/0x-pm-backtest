#!/usr/bin/env python3
"""BR2 entry-timing x vol-floor sweep matching the prior calibrated May ablation.

This intentionally leaves meta-calibration enabled. The frozen-snapshot variant
is useful as a gate diagnostic, but the existing May ablation artifacts that
produced orders used this calibrated replay mode.
"""
import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target/release/pm-app")
MARKETS = os.environ.get(
    "SWEEP_MARKETS",
    os.path.join(ROOT, "data/runs/volgate/markets-may-labeled.jsonl"),
)
CACHE = os.path.join(ROOT, "data/cache")
OUTROOT = os.environ.get(
    "SWEEP_OUTROOT",
    os.path.join(ROOT, "data/runs/volgate/entry_sweep_calibrated_labeled"),
)

def float_list_env(name: str, default: list[float]) -> list[float]:
    raw = os.environ.get(name)
    if not raw:
        return default
    return [float(item) for item in raw.split(",") if item.strip()]


ENTRY_SECS = float_list_env("SWEEP_ENTRY_SECS", [180.0, 120.0, 90.0, 60.0, 45.0, 30.0])
VOL_FLOORS = float_list_env("SWEEP_VOL_FLOORS", [1.25, 0.0])
MAX_MARKETS = int(os.environ.get("SWEEP_MAX_MARKETS", "0"))

BASE_FLAGS = [
    "--max-clip-usdc", "30",
    "--max-order-clip-multiplier", "10",
    "--max-per-market-exposure-usdc", "250",
    "--max-per-market-exposure-frac", "0.12",
    "--kelly-fraction", "0.5",
    "--spot-symbol", "BTCUSDT",
    "--clip-fraction-of-equity", "0.015",
    "--clip-drawdown-soft-pct", "0.2",
    "--clip-drawdown-hard-pct", "0.4",
    "--clip-drawdown-min-multiplier", "0.1",
    "--br2-participation-clip-frac", "0.0",
    "--br2-participation-max-pair-cost", "0.99",
    "--br2-participation-max-orders-per-leg", "500",
    "--br2-participation-max-inventory-delta-shares", "25.0",
    "--br2-participation-repair-inventory-delta-shares", "5.0",
    "--br2-participation-refresh-secs", "0.50",
    "--br2-participation-stop-secs-before-close", "20.0",
    "--br2-min-composite-direction", "0.10",
    "--br2-early-clip-frac", "0.00",
    "--br2-mid-clip-frac", "0.00",
    "--br2-late-clip-frac", "1.0",
    "--br2-late-max-fires", "3",
    "--br2-late-confirm-min-model-confidence", "0.58",
    "--br2-late-confirm-max-model-risk", "0.80",
    "--br2-late-confirm-min-model-side-p", "0.58",
    "--br2-late-confirm-min-model-edge", "0.02",
    "--br2-late-confirm-min-book-skew", "0.06",
    "--br2-late-confirm-max-whipsaw-score", "0.85",
    "--br2-late-confirm-max-observed-range", "0.50",
    "--br2-recent-regime-gate-min-edge", "0.08",
    "--br2-high-skew-clip-frac", "0.60",
    "--br2-high-skew-max-clips", "5",
    "--br2-high-skew-max-whipsaw-score", "0.75",
    "--br2-late-favourite-threshold", "0.22",
    "--br2-late-favourite-max-ask", "0.97",
    "--br2-late-favourite-clip-frac", "1.00",
    "--br2-late-favourite-high-cert-clip-frac", "1.00",
    "--br2-late-favourite-high-cert-full-clip-edge", "0.09",
    "--br2-late-favourite-max-clips", "12",
    "--br2-late-favourite-min-sustain-secs", "0.0",
    "--br2-late-favourite-sweep-depth", "7",
    "--br2-late-favourite-min-model-confidence", "0.68",
    "--br2-late-favourite-min-model-direction-abs", "0.0",
    "--br2-late-favourite-max-model-risk", "0.72",
    "--br2-late-favourite-min-model-side-p", "0.62",
    "--br2-late-favourite-high-cert-min-model-edge", "0.06",
    "--br2-late-favourite-max-whipsaw-score", "0.75",
    "--br2-late-favourite-max-reversal-pressure", "0.85",
    "--br2-late-favourite-min-path-efficiency", "0.0",
    "--br2-late-favourite-max-observed-range", "0.70",
    "--br2-late-favourite-range-soft-throttle", "0.55",
    "--br2-late-favourite-range-hard-throttle", "0.70",
    "--br2-late-favourite-range-extra-edge", "0.08",
    "--br2-late-favourite-range-extra-confidence", "0.12",
    "--br2-late-favourite-max-adverse-fast-momentum", "1.0",
    "--br2-late-favourite-max-adverse-broad-momentum", "1.0",
    "--br2-late-favourite-max-entry-pullback", "1.0",
    "--br2-late-favourite-max-avg-entry-drawdown", "1.0",
    "--br2-tail-clip-frac", "0.10",
    "--br2-tail-max-clips", "6",
    "--br2-tail-sweep-depth", "3",
    "--br2-tail-min-ask", "0.01",
    "--br2-tail-max-ask", "0.08",
    "--br2-tail-min-seconds-to-close", "10.0",
    "--br2-tail-min-favourite-unrealized-edge", "0.0",
    "--br2-tail-min-observed-range", "0.0",
    "--br2-tail-target-favourite-loss-coverage-frac", "0.50",
    "--br2-tail-reversal-coverage-frac", "0.00",
    "--br2-tail-reversal-min-seconds-to-close", "10.0",
    "--br2-tail-reversal-max-seconds-to-close", "35.0",
    "--br2-tail-reversal-min-favourite-ask", "0.85",
    "--br2-tail-extreme-threshold", "0.30",
    "--br2-tail-min-skew-step", "0.02",
    "--br2-tail-budget-favourite-spend-frac", "0.20",
    "--br2-tail-budget-favourite-upside-frac", "0.25",
    "--br2-tail-regime-boost-coverage-frac", "0.0",
    "--br2-tail-regime-boost-budget-spend-frac", "0.0",
    "--br2-tail-regime-boost-budget-upside-frac", "0.0",
    "--br2-tail-regime-boost-min-whipsaw-score", "1.0",
    "--br2-tail-regime-boost-min-reversal-pressure", "1.0",
    "--br2-tail-regime-boost-min-realized-vol-180s-bps", "1000000000.0",
    "--br2-tail-regime-boost-max-path-efficiency", "0.0",
    "--model-gate-min-confidence", "0.68",
    "--model-gate-max-risk", "0.72",
    "--model-gate-min-edge", "0.00",
    "--max-concurrent-fetches", "64",
    "--replay-sample-ms", "1000",
    "--taker-latency-ms", "500",
    "--portfolio-checkpoint-every-markets", "250",
    "--br2-late-favourite-min-ask", "0.60",
    "--br2-late-favourite-min-model-edge", "0.06",
]


def cell_dir(entry, floor):
    return os.path.join(OUTROOT, f"e{int(entry)}_f{str(floor).replace('.', 'p')}")


def run_cell(entry, floor):
    out = cell_dir(entry, floor)
    os.makedirs(out, exist_ok=True)
    summary = os.path.join(out, "summary.json")
    cmd = [
        BIN, "walk-forward",
        "--markets", MARKETS,
        "--local-cache-dir", CACHE,
        "--use-outcome-label",
        "--strategies", "bonereaper_v2",
        "--starting-cash", "1000",
        "--portfolio-mode",
    ] + BASE_FLAGS + [
        "--br2-late-favourite-start-secs", str(entry),
        "--br2-late-confirm-min-realized-vol-180s-bps", str(floor),
        "--br2-high-skew-min-realized-vol-180s-bps", str(floor),
        "--br2-late-favourite-min-realized-vol-180s-bps", str(floor),
        "--out-summary", summary,
        "--out-markets", os.path.join(out, "markets.jsonl"),
    ]
    if MAX_MARKETS > 0:
        cmd += ["--max-markets", str(MAX_MARKETS)]
    log = os.path.join(out, "run.log")
    with open(log, "w") as lf:
        rc = subprocess.run(cmd, stdout=lf, stderr=subprocess.STDOUT).returncode
    if rc != 0:
        sys.stderr.write(f"FAILED entry={entry} floor={floor} rc={rc}; see {log}\n")
    return summary


def metrics(summary_path):
    d = json.load(open(summary_path))
    return d["per_strategy"]["bonereaper_v2"]


def main():
    os.makedirs(OUTROOT, exist_ok=True)
    results = {}
    for entry in ENTRY_SECS:
        for floor in VOL_FLOORS:
            sys.stderr.write(f"running calibrated entry={entry} floor={floor} ...\n")
            sys.stderr.flush()
            path = run_cell(entry, floor)
            try:
                results[(entry, floor)] = metrics(path)
            except Exception as exc:
                sys.stderr.write(f"parse fail {path}: {exc}\n")
                results[(entry, floor)] = None

    print("entry,floor,pnl,max_dd,markets_with_orders,fills,hit_rate,sharpe,worst")
    for entry in ENTRY_SECS:
        for floor in VOL_FLOORS:
            row = results.get((entry, floor))
            if not row:
                print(f"{entry},{floor},PARSE_FAIL")
                continue
            print(
                f"{int(entry)},{floor},"
                f"{row.get('total_pnl_usdc', 0):.2f},"
                f"{row.get('path_max_drawdown_pct', 0):.2f},"
                f"{row.get('markets_with_orders', 0)},"
                f"{row.get('total_orders_filled', 0)},"
                f"{row.get('hit_rate', 0):.4f},"
                f"{row.get('sharpe_ratio', 0):.3f},"
                f"{row.get('worst_market_pnl', 0):.2f}"
            )


if __name__ == "__main__":
    main()
