#!/usr/bin/env python3
"""Compare Polymarket backtest summary files.

Usage:
  python scripts/compare_results.py results/run1/summary.json results/run2/summary.json ...
  AWS_PROFILE=visumlabs python scripts/compare_results.py s3://bucket/results/run/label/summary.json
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict


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
        stderr = exc.stderr.strip()
        raise RuntimeError(f"aws s3 cp failed for {path}: {stderr}") from exc
    return Path(tmp.name), True


def load_json(path: Path) -> Dict[str, Any]:
    with open(path) as f:
        return json.load(f)


def load_summary(path: str) -> Dict[str, Any]:
    materialized, should_delete = materialize(path)
    try:
        return load_json(materialized)
    finally:
        if should_delete:
            materialized.unlink(missing_ok=True)


def sibling_path(path: str, sibling: str) -> str:
    if path.startswith("s3://"):
        return path.rsplit("/", 1)[0] + f"/{sibling}"
    return str(Path(path).parent / sibling)


def load_optional_json(path: str) -> Dict[str, Any]:
    materialized: Path | None = None
    should_delete = False
    try:
        materialized, should_delete = materialize(path)
        return load_json(materialized)
    except (FileNotFoundError, RuntimeError, json.JSONDecodeError):
        return {}
    finally:
        if should_delete and materialized is not None:
            materialized.unlink(missing_ok=True)


def get_strategy(summary: Dict, strategy: str) -> Dict:
    per_strat = summary.get("per_strategy", {})
    if strategy:
        return per_strat.get(strategy, {})
    return per_strat.get("bonereaper_v2", {}) or per_strat.get("bonereaper_lite", {})


def fmt_num(value: Any, decimals: int = 2) -> str:
    if value is None:
        return "N/A"
    try:
        return f"{float(value):.{decimals}f}"
    except (TypeError, ValueError):
        return "N/A"


def fill_tag_pnl(strategy: Dict[str, Any], tag: str) -> Any:
    return ((strategy.get("by_fill_tag") or {}).get(tag) or {}).get("total_pnl_usdc")


def short_path(path: str) -> str:
    if path.startswith("s3://"):
        parts = path.rstrip("/").split("/")
        if len(parts) >= 3:
            return "/".join(parts[-3:])
    return path


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("summaries", nargs="+", help="local path or s3://.../summary.json")
    parser.add_argument("--strategy", default="bonereaper_v2")
    parser.add_argument("--aws-profile", help="AWS profile for s3:// inputs")
    args = parser.parse_args()
    global AWS_PROFILE
    AWS_PROFILE = args.aws_profile

    rows = []
    for path in args.summaries:
        try:
            s = load_summary(path)
            br = get_strategy(s, args.strategy)
            manifest = load_optional_json(sibling_path(path, "run_manifest.json"))
            cfg = s.get("run_config") or {}
            markets = s.get("markets_succeeded") or s.get("markets_attempted") or 0
            markets_with_orders = br.get("markets_with_orders") or 0
            active_pct = (markets_with_orders / markets * 100.0) if markets else None

            rows.append(
                {
                    "path": path,
                    "markets": markets,
                    "active_pct": active_pct,
                    "pnl": br.get("total_pnl_usdc"),
                    "compounded_return_pct": br.get("compounded_return_pct"),
                    "max_drawdown_pct": br.get("path_max_drawdown_pct"),
                    "sharpe": br.get("sharpe_ratio"),
                    "fills": br.get("total_orders_filled"),
                    "log_loss": ((br.get("model_fill_quality") or {}).get("all") or {}).get("log_loss"),
                    "late_favourite": fill_tag_pnl(br, "br2_late_favourite_load"),
                    "late_confirm": fill_tag_pnl(br, "br2_late_confirm"),
                    "high_skew": fill_tag_pnl(br, "br2_high_skew_load"),
                    "convex_tail": fill_tag_pnl(br, "br2_convex_tail"),
                    "tail_clip": cfg.get("br2_tail_clip_frac"),
                    "tail_max_ask": cfg.get("br2_tail_max_ask"),
                    "profile": manifest.get("profile_path"),
                    "git_sha": (manifest.get("git_sha") or "")[:8],
                }
            )
        except Exception as exc:
            print(f"Error loading {path}: {exc}", file=sys.stderr)

    # Sort by compounded return desc
    rows.sort(key=lambda r: (r["compounded_return_pct"] or -999), reverse=True)

    print(
        f"{'Path':<60} {'Mkts':>7} {'Act%':>7} {'PnL':>10} {'Ret%':>9} {'DD%':>8} "
        f"{'Fills':>7} {'LL':>7} {'Fav':>9} {'Conf':>9} {'Skew':>9} {'Tail':>9} "
        f"{'TailCfg':>9} {'SHA'}"
    )
    print("-" * 185)
    for r in rows:
        tail_cfg = ""
        if r["tail_clip"] is not None:
            tail_cfg = f"{fmt_num(r['tail_clip'], 2)}/{fmt_num(r['tail_max_ask'], 2)}"
        print(
            f"{short_path(r['path']):<60} {r['markets']:>7} "
            f"{fmt_num(r['active_pct']):>7} "
            f"{fmt_num(r['pnl']):>10} "
            f"{fmt_num(r['compounded_return_pct']):>9} "
            f"{fmt_num(r['max_drawdown_pct']):>8} "
            f"{fmt_num(r['fills'], 0):>7} "
            f"{fmt_num(r['log_loss'], 4):>7} "
            f"{fmt_num(r['late_favourite']):>9} "
            f"{fmt_num(r['late_confirm']):>9} "
            f"{fmt_num(r['high_skew']):>9} "
            f"{fmt_num(r['convex_tail']):>9} "
            f"{tail_cfg:>9} "
            f"{r['git_sha']}"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
