#!/usr/bin/env python3
"""Run a bounded BR2 low-vol-open walk-forward with per-decision logging."""

from __future__ import annotations

import argparse
import subprocess
from pathlib import Path


DEFAULT_MARKETS = "data/runs/volgate/markets-may-labeled.jsonl"

BASE_FLAGS = [
    "--use-outcome-label",
    "--strategies", "bonereaper_v2",
    "--starting-cash", "1000",
    "--portfolio-mode",
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
    "--br2-late-favourite-start-secs", "180.0",
    "--br2-late-favourite-threshold", "0.22",
    "--br2-late-favourite-min-ask", "0.60",
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
    "--br2-late-favourite-min-model-edge", "0.06",
    "--br2-late-favourite-high-cert-min-model-edge", "0.06",
    "--br2-late-favourite-max-whipsaw-score", "0.75",
    "--br2-late-favourite-max-reversal-pressure", "0.85",
    "--br2-late-favourite-min-path-efficiency", "0.0",
    "--br2-late-favourite-min-realized-vol-180s-bps", "0.0",
    "--br2-late-confirm-min-realized-vol-180s-bps", "0.0",
    "--br2-high-skew-min-realized-vol-180s-bps", "0.0",
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
    "--model-gate-min-confidence", "0.68",
    "--model-gate-max-risk", "0.72",
    "--model-gate-min-edge", "0.00",
    "--max-concurrent-fetches", "64",
    "--replay-sample-ms", "1000",
    "--taker-latency-ms", "500",
    "--portfolio-checkpoint-every-markets", "250",
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-markets", type=int, default=0)
    parser.add_argument("--max-markets", type=int, default=800)
    parser.add_argument("--decision-every-n", type=int, default=5)
    parser.add_argument("--markets", default=DEFAULT_MARKETS)
    parser.add_argument(
        "--local-cache-dir",
        default="data/cache",
        help="Use local Telonex cache; pass empty string to read from S3/env store.",
    )
    parser.add_argument("--out-dir", default="data/runs/low_vol_directional")
    parser.add_argument("--label", required=True)
    args = parser.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    cmd = [
        "target/release/pm-app",
        "walk-forward",
        "--markets", args.markets,
        *BASE_FLAGS,
        "--skip-markets", str(args.skip_markets),
        "--max-markets", str(args.max_markets),
        "--decision-log", str(out_dir / f"decision_log_{args.label}.jsonl"),
        "--decision-log-every-n", str(args.decision_every_n),
        "--out-summary", str(out_dir / f"summary_{args.label}.json"),
        "--out-markets", str(out_dir / f"markets_{args.label}.jsonl"),
    ]
    if args.local_cache_dir:
        cmd.extend(["--local-cache-dir", args.local_cache_dir])
    return subprocess.run(cmd).returncode


if __name__ == "__main__":
    raise SystemExit(main())
