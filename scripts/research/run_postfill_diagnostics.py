#!/usr/bin/env python3
"""Run the standard BTC5m post-fill diagnostic pack."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path


def run(cmd: list[str], env: dict[str, str] | None = None) -> None:
    print("+ " + " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True, env=env)


def run_optional(cmd: list[str], env: dict[str, str] | None = None) -> bool:
    print("+ " + " ".join(cmd), flush=True)
    completed = subprocess.run(cmd, check=False, env=env)
    if completed.returncode != 0:
        print(
            f"warning: optional diagnostic skipped after exit {completed.returncode}: {' '.join(cmd)}",
            flush=True,
        )
        return False
    return True


def maybe_download(source: str, local_path: Path, aws_profile: str | None) -> Path:
    if not source.startswith("s3://"):
        return Path(source)
    env = os.environ.copy()
    if aws_profile:
        env["AWS_PROFILE"] = aws_profile
    local_path.parent.mkdir(parents=True, exist_ok=True)
    run(["aws", "s3", "cp", source, str(local_path)], env=env)
    return local_path


def market_count(path: Path) -> int:
    count = 0
    with path.open() as file:
        for line in file:
            if line.strip():
                count += 1
    return count


def last_summary(path: Path, strategy: str) -> dict[str, float | int | str]:
    rows = []
    total_pnl = 0.0
    with path.open() as file:
        for line in file:
            if line.strip():
                row = json.loads(line)
                rows.append(row)
                strat = ((row.get("per_strategy") or {}).get(strategy)) or {}
                total_pnl += float(strat.get("pnl_usdc") or 0.0)
    if not rows:
        return {}
    strat = ((rows[-1].get("per_strategy") or {}).get(strategy)) or {}
    return {
        "markets": len(rows),
        "last_slug": rows[-1].get("slug") or "",
        "end_equity_usdc": float(strat.get("end_equity_usdc") or 0.0),
        "summed_pnl_usdc": total_pnl,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets", help="local path or s3://.../markets.jsonl")
    parser.add_argument("--aws-profile")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--recent-days", type=int, default=30)
    parser.add_argument("--last-markets", type=int, default=8633)
    parser.add_argument("--test-days", type=int, default=30)
    parser.add_argument("--gate-min-train-fills", type=int, default=1000)
    parser.add_argument("--gate-test-fills", type=int, default=500)
    parser.add_argument("--gate-step-fills", type=int, default=500)
    parser.add_argument("--gate-epochs", type=int, default=1200)
    parser.add_argument("--out-prefix", default="docs/btc5m_postfill_full")
    parser.add_argument("--local-cache", default="/tmp/btc5m_postfill_diagnostics_markets.jsonl")
    parser.add_argument("--min-fills", type=int, default=500)
    args = parser.parse_args()

    source_label = args.markets
    local_markets = maybe_download(args.markets, Path(args.local_cache), args.aws_profile)
    count = market_count(local_markets)
    summary = last_summary(local_markets, args.strategy)
    print(f"markets={count} summary={summary}", flush=True)

    out_prefix = Path(args.out_prefix)
    out_prefix.parent.mkdir(parents=True, exist_ok=True)

    run(
        [
            sys.executable,
            "scripts/postfill_regime_evolution.py",
            str(local_markets),
            "--strategy",
            args.strategy,
            "--source-label",
            source_label,
            "--recent-days",
            str(args.recent_days),
            "--out-md",
            str(out_prefix.with_name(out_prefix.name + "_regime_evolution.md")),
        ]
    )
    run(
        [
            sys.executable,
            "scripts/reversal_tail_diagnostics.py",
            "--markets",
            str(local_markets),
            "--last-markets",
            str(args.last_markets),
            "--out-md",
            str(out_prefix.with_name(out_prefix.name + "_reversal_tail.md")),
        ]
    )
    for target in ("toxic_reversal_path", "crossed_mid_after_fill"):
        run_optional(
            [
                sys.executable,
                "scripts/postfill_reversal_model.py",
                str(local_markets),
                "--strategy",
                args.strategy,
                "--test-days",
                str(args.test_days),
                "--target",
                target,
                "--min-fills",
                str(args.min_fills),
                "--source-label",
                source_label,
                "--out-md",
                str(out_prefix.with_name(out_prefix.name + f"_{target}_model.md")),
            ]
        )
    run_optional(
        [
            sys.executable,
            "scripts/postfill_gate_sim.py",
            str(local_markets),
            "--strategy",
            args.strategy,
            "--target",
            "toxic_reversal_path",
            "--min-train-fills",
            str(args.gate_min_train_fills),
            "--test-fills",
            str(args.gate_test_fills),
            "--step-fills",
            str(args.gate_step_fills),
            "--epochs",
            str(args.gate_epochs),
            "--source-label",
            source_label,
            "--out-md",
            str(out_prefix.with_name(out_prefix.name + "_gate_sim.md")),
        ]
    )
    run_optional(
        [
            sys.executable,
            "scripts/late_break_feature_contrast.py",
            str(local_markets),
            "--strategy",
            args.strategy,
            "--source-label",
            source_label,
            "--out-md",
            str(out_prefix.with_name(out_prefix.name + "_late_break_feature_contrast.md")),
        ]
    )
    run_optional(
        [
            sys.executable,
            "scripts/late_break_gate_search.py",
            str(local_markets),
            "--strategy",
            args.strategy,
            "--source-label",
            source_label,
            "--out-md",
            str(out_prefix.with_name(out_prefix.name + "_late_break_gate_search.md")),
        ]
    )
    run_optional(
        [
            sys.executable,
            "scripts/late_break_gate_portfolio_sim.py",
            str(local_markets),
            "--strategy",
            args.strategy,
            "--source-label",
            source_label,
            "--out-md",
            str(out_prefix.with_name(out_prefix.name + "_late_break_gate_portfolio_sim.md")),
        ]
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
