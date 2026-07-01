#!/usr/bin/env python3
"""Poll a post-fill backtest artifact and run diagnostics once ready."""

from __future__ import annotations

import argparse
import os
import subprocess
import sys
import time
from pathlib import Path


def run(cmd: list[str], env: dict[str, str] | None = None, check: bool = True) -> subprocess.CompletedProcess[str]:
    print("+ " + " ".join(cmd), flush=True)
    return subprocess.run(cmd, check=check, env=env, text=True, capture_output=True)


def download(source: str, local_path: Path, aws_profile: str | None) -> bool:
    env = os.environ.copy()
    if aws_profile:
        env["AWS_PROFILE"] = aws_profile
    local_path.parent.mkdir(parents=True, exist_ok=True)
    if source.startswith("s3://"):
        proc = run(["aws", "s3", "cp", source, str(local_path)], env=env, check=False)
        if proc.returncode != 0:
            print(proc.stderr.strip(), flush=True)
            return False
        return True
    source_path = Path(source)
    if not source_path.exists():
        return False
    local_path.write_bytes(source_path.read_bytes())
    return True


def line_count(path: Path) -> int:
    if not path.exists():
        return 0
    count = 0
    with path.open() as file:
        for line in file:
            if line.strip():
                count += 1
    return count


def run_diagnostics(args: argparse.Namespace, local_path: Path) -> None:
    cmd = [
        sys.executable,
        "scripts/run_postfill_diagnostics.py",
        str(local_path),
        "--recent-days",
        str(args.recent_days),
        "--last-markets",
        str(args.last_markets),
        "--test-days",
        str(args.test_days),
        "--gate-min-train-fills",
        str(args.gate_min_train_fills),
        "--gate-test-fills",
        str(args.gate_test_fills),
        "--gate-step-fills",
        str(args.gate_step_fills),
        "--gate-epochs",
        str(args.gate_epochs),
        "--out-prefix",
        args.out_prefix,
        "--min-fills",
        str(args.min_fills),
    ]
    run(cmd, env=os.environ.copy(), check=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("markets", help="local path or s3://.../markets.jsonl")
    parser.add_argument("--aws-profile")
    parser.add_argument("--ready-markets", type=int, default=23705)
    parser.add_argument("--poll-seconds", type=int, default=300)
    parser.add_argument("--max-polls", type=int, default=0, help="0 means poll forever")
    parser.add_argument("--local-cache", default="/tmp/btc5m_postfill_watch_markets.jsonl")
    parser.add_argument("--out-prefix", default="docs/btc5m_postfill_full")
    parser.add_argument("--recent-days", type=int, default=30)
    parser.add_argument("--last-markets", type=int, default=8633)
    parser.add_argument("--test-days", type=int, default=30)
    parser.add_argument("--min-fills", type=int, default=500)
    parser.add_argument("--gate-min-train-fills", type=int, default=1000)
    parser.add_argument("--gate-test-fills", type=int, default=500)
    parser.add_argument("--gate-step-fills", type=int, default=500)
    parser.add_argument("--gate-epochs", type=int, default=1200)
    parser.add_argument("--run-once", action="store_true", help="check once and exit if not ready")
    args = parser.parse_args()

    local_path = Path(args.local_cache)
    polls = 0
    while True:
        polls += 1
        ok = download(args.markets, local_path, args.aws_profile)
        count = line_count(local_path) if ok else 0
        print(
            f"poll={polls} markets={count} ready_markets={args.ready_markets} "
            f"source={args.markets}",
            flush=True,
        )
        if count >= args.ready_markets:
            run_diagnostics(args, local_path)
            return 0
        if args.run_once:
            return 2
        if args.max_polls and polls >= args.max_polls:
            return 2
        time.sleep(args.poll_seconds)


if __name__ == "__main__":
    raise SystemExit(main())
