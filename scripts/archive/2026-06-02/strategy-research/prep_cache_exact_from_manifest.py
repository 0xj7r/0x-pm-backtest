#!/usr/bin/env python3
"""Download manifest-derived parquet objects without S3 prefix listing.

The normal prep-cache path lists S3 prefixes. When local networking makes ListObjects
flaky but exact object downloads still work, this script derives the expected object
keys from the market manifest and copies them with `aws s3 cp`.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import subprocess
from pathlib import Path


BUCKET = "pm-research-data-prod"


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser()
    p.add_argument("--markets", required=True, type=Path)
    p.add_argument("--cache-dir", required=True, type=Path)
    p.add_argument("--profile", default=os.environ.get("AWS_PROFILE"))
    p.add_argument("--spot-symbol", default="BTCUSDT")
    p.add_argument("--max-workers", type=int, default=16)
    p.add_argument("--aws-max-attempts", type=int, default=2)
    p.add_argument("--connect-timeout", type=str, default="5")
    p.add_argument("--read-timeout", type=str, default="20")
    p.add_argument("--skip-existing", action=argparse.BooleanOptionalAction, default=True)
    return p.parse_args()


def load_markets(path: Path) -> list[dict]:
    rows: list[dict] = []
    with path.open() as f:
        for line in f:
            if line.strip():
                rows.append(json.loads(line))
    return rows


def market_targets(rows: list[dict], cache_dir: Path, spot_symbol: str) -> list[tuple[str, Path]]:
    targets: dict[str, Path] = {}
    days = {r["date"] for r in rows}
    for day in sorted(days):
        key = (
            "raw/binance/exchange=binance/channel=agg_trades/"
            f"symbol={spot_symbol}/date={day}/{spot_symbol}-aggTrades-{day}.parquet"
        )
        targets[key] = cache_dir / key

    for r in rows:
        day = r["date"]
        asset_id = r["asset_id"]
        for channel in ("book_snapshot_25", "trades"):
            filename = f"{asset_id}_{day}_{channel}.parquet"
            key = (
                "raw/telonex/exchange=polymarket/"
                f"channel={channel}/date={day}/asset_id={asset_id}/{filename}"
            )
            targets[key] = cache_dir / key
    return sorted(targets.items())


def copy_one(
    key: str,
    dst: Path,
    profile: str | None,
    skip_existing: bool,
    aws_max_attempts: int,
    connect_timeout: str,
    read_timeout: str,
) -> tuple[bool, str]:
    if skip_existing and dst.exists() and dst.stat().st_size > 0:
        return True, "skipped"
    dst.parent.mkdir(parents=True, exist_ok=True)
    cmd = [
        "aws",
        "s3",
        "cp",
        f"s3://{BUCKET}/{key}",
        str(dst),
        "--only-show-errors",
        "--no-progress",
        "--cli-read-timeout",
        read_timeout,
        "--cli-connect-timeout",
        connect_timeout,
    ]
    if profile:
        cmd.extend(["--profile", profile])
    env = os.environ.copy()
    env["AWS_MAX_ATTEMPTS"] = str(aws_max_attempts)
    proc = subprocess.run(cmd, text=True, capture_output=True, env=env)
    if proc.returncode == 0:
        return True, "downloaded"
    detail = (proc.stderr or proc.stdout).strip().splitlines()
    return False, detail[-1] if detail else f"aws exited {proc.returncode}"


def main() -> int:
    args = parse_args()
    rows = load_markets(args.markets)
    targets = market_targets(rows, args.cache_dir, args.spot_symbol)
    print(f"targets={len(targets)} markets={len(rows)} cache={args.cache_dir}", flush=True)

    done = 0
    skipped = 0
    failed: list[tuple[str, str]] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.max_workers) as pool:
        futures = {
            pool.submit(
                copy_one,
                key,
                dst,
                args.profile,
                args.skip_existing,
                args.aws_max_attempts,
                args.connect_timeout,
                args.read_timeout,
            ): key
            for key, dst in targets
        }
        for fut in concurrent.futures.as_completed(futures):
            key = futures[fut]
            ok, status = fut.result()
            if ok:
                if status == "skipped":
                    skipped += 1
                else:
                    done += 1
            else:
                failed.append((key, status))
            seen = done + skipped + len(failed)
            if seen % 100 == 0 or seen == len(targets):
                print(
                    f"progress {seen}/{len(targets)} downloaded={done} "
                    f"skipped={skipped} failed={len(failed)}",
                    flush=True,
                )

    if failed:
        print("failed targets:")
        for key, err in failed[:50]:
            print(f"{key}: {err}")
        if len(failed) > 50:
            print(f"... {len(failed) - 50} more")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
