#!/usr/bin/env python3
"""Diff run_config blocks from two backtest summary.json files.

This is a preflight/sanity helper for strategy comparisons. It fails when a
candidate differs from the baseline outside the explicitly allowed config keys.
Both local paths and s3:// paths are supported.
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any


AWS_PROFILE: str | None = None


def materialize(path: str) -> tuple[Path, bool]:
    if not path.startswith("s3://"):
        return Path(path), False
    tmp = tempfile.NamedTemporaryFile(prefix="pm-summary-", suffix=".json", delete=False)
    tmp.close()
    cmd = ["aws", "s3", "cp", path, tmp.name]
    env = os.environ.copy()
    if AWS_PROFILE:
        env["AWS_PROFILE"] = AWS_PROFILE
    try:
        subprocess.run(cmd, check=True, capture_output=True, text=True, env=env)
    except subprocess.CalledProcessError as exc:
        Path(tmp.name).unlink(missing_ok=True)
        raise RuntimeError(f"aws s3 cp failed for {path}: {exc.stderr.strip()}") from exc
    return Path(tmp.name), True


def load_run_config(path: str) -> dict[str, Any]:
    materialized, should_delete = materialize(path)
    try:
        with materialized.open() as f:
            summary = json.load(f)
    finally:
        if should_delete:
            materialized.unlink(missing_ok=True)
    cfg = summary.get("run_config")
    if not isinstance(cfg, dict):
        raise RuntimeError(f"{path} has no run_config object")
    return cfg


def parse_csv(values: list[str]) -> set[str]:
    out: set[str] = set()
    for value in values:
        out.update(part.strip() for part in value.split(",") if part.strip())
    return out


def fmt(value: Any) -> str:
    if isinstance(value, float):
        return f"{value:.10g}"
    return repr(value)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("baseline_summary", help="baseline summary.json, local or s3://")
    parser.add_argument("candidate_summary", help="candidate summary.json, local or s3://")
    parser.add_argument(
        "--allow",
        action="append",
        default=[],
        help="Allowed differing run_config key, or comma-separated keys. May repeat.",
    )
    parser.add_argument("--aws-profile", help="AWS profile for s3:// inputs")
    args = parser.parse_args()

    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile
    allowed = parse_csv(args.allow)

    baseline = load_run_config(args.baseline_summary)
    candidate = load_run_config(args.candidate_summary)
    all_keys = sorted(set(baseline) | set(candidate))
    diffs = [
        (key, baseline.get(key), candidate.get(key))
        for key in all_keys
        if baseline.get(key) != candidate.get(key)
    ]

    unexpected = [(key, a, b) for key, a, b in diffs if key not in allowed]
    allowed_diffs = [(key, a, b) for key, a, b in diffs if key in allowed]

    if allowed_diffs:
        print("allowed differences:")
        for key, a, b in allowed_diffs:
            print(f"  {key}: baseline={fmt(a)} candidate={fmt(b)}")
    if unexpected:
        print("unexpected differences:", file=sys.stderr)
        for key, a, b in unexpected:
            print(f"  {key}: baseline={fmt(a)} candidate={fmt(b)}", file=sys.stderr)
        return 1
    print(f"configs match outside {len(allowed)} allowed key(s)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
