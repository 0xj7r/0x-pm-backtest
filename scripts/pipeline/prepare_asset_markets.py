#!/usr/bin/env python3
"""
Prepare per-asset markets JSONL lists (full + small train slice) from the master
markets-full.parquet (local or S3) for cross-market diversification runs (ETH-5m,
SOL-5m, etc.).

This is the missing piece that caused the ETH run 56874 to fail with
"markets-train.jsonl: No such file".

It replicates (in Python, for ease of S3 + one-off use) the filtering logic of
`pm-app discover-markets-parquet --slug-prefix ... --start-date ... --end-date ...`
plus the small head for meta-calibrator training.

Usage (with AWS creds):
    python scripts/prepare_asset_markets.py \
        --master-parquet s3://pm-research-backtest-prod/artifacts/markets-full.parquet \
        --asset eth \
        --start-date 2026-02-18 --end-date 2026-05-20 \
        --train-n 4500 \
        --upload-bucket pm-research-backtest-prod \
        --upload-prefix markets/eth-5m-feb-may-2026

Then launch EC2 with:
    --markets-key markets/eth-5m-feb-may-2026/markets.jsonl \
    --train-markets-key markets/eth-5m-feb-may-2026/markets-train.jsonl \
    --slug-prefixes "eth-updown-5m-" \
    --spot-symbol ETHUSDT \
    ...

The script also works fully locally if you have the parquet.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import pyarrow.parquet as pq

try:
    import s3fs  # type: ignore
except Exception:
    s3fs = None  # lazy — only needed for direct s3 parquet reads


def parse_close_ts(slug: str) -> int | None:
    """Extract the unix seconds from a slug like eth-updown-5m-1772444100."""
    try:
        parts = slug.rsplit("-", 1)
        return int(parts[-1])
    except Exception:
        return None


def slug_matches_prefixes(slug: str, prefixes: str) -> bool:
    prefs = [p.strip() for p in prefixes.split(",") if p.strip()]
    s = slug.lower()
    return any(s.startswith(p.lower()) for p in prefs)


def canonical_up_asset_for_row(
    outcome_0: str | None,
    asset_id_0: str | None,
    outcome_1: str | None,
    asset_id_1: str | None,
) -> tuple[int, str] | None:
    """Pick the UP/YES leg (the one the late-favourite strategy loads)."""
    if outcome_0 and outcome_0.lower() in ("up", "yes") and asset_id_0:
        return (0, asset_id_0)
    if outcome_1 and outcome_1.lower() in ("up", "yes") and asset_id_1:
        return (1, asset_id_1)
    # Fallback: first non-empty
    if asset_id_0:
        return (0, asset_id_0)
    if asset_id_1:
        return (1, asset_id_1)
    return None


def discover_from_parquet(
    parquet_path: str,
    slug_prefixes: str,
    start_date: str,
    end_date: str,
    require_book_s3: bool = False,  # NOTE: full S3 book filter is heavy; usually done in Rust
) -> list[dict[str, Any]]:
    """Return list of MarketHandle dicts (same shape the Rust code emits)."""
    if parquet_path.startswith("s3://"):
        if s3fs is None:
            raise RuntimeError("s3fs not installed; pip install s3fs or use a local parquet copy")
        fs = s3fs.S3FileSystem(anon=False)
        with fs.open(parquet_path, "rb") as f:
            pf = pq.ParquetFile(f)
            batches = list(pf.iter_batches(batch_size=8192))
    else:
        pf = pq.ParquetFile(parquet_path)
        batches = list(pf.iter_batches(batch_size=8192))

    start_dt = datetime.fromisoformat(start_date).date()
    end_dt = datetime.fromisoformat(end_date).date()

    out: list[dict[str, Any]] = []
    seen_slugs: set[str] = set()

    for batch in batches:
        slugs = batch.column("slug").to_pylist()
        statuses = batch.column("status").to_pylist()
        outcome_0 = batch.column("outcome_0").to_pylist() if "outcome_0" in batch.schema.names else [None] * len(slugs)
        outcome_1 = batch.column("outcome_1").to_pylist() if "outcome_1" in batch.schema.names else [None] * len(slugs)
        asset_id_0 = batch.column("asset_id_0").to_pylist() if "asset_id_0" in batch.schema.names else [None] * len(slugs)
        asset_id_1 = batch.column("asset_id_1").to_pylist() if "asset_id_1" in batch.schema.names else [None] * len(slugs)
        end_date_us = batch.column("end_date_us").to_pylist()

        for i in range(len(slugs)):
            slug = str(slugs[i]) if slugs[i] is not None else ""
            if not slug or not slug_matches_prefixes(slug, slug_prefixes):
                continue
            if str(statuses[i]).lower() != "resolved":
                continue
            close_ts = parse_close_ts(slug)
            if close_ts is None:
                continue
            dt = datetime.fromtimestamp(close_ts, tz=timezone.utc).date()
            if dt < start_dt or dt > end_dt:
                continue

            sel = canonical_up_asset_for_row(
                outcome_0[i], asset_id_0[i], outcome_1[i], asset_id_1[i]
            )
            if sel is None:
                continue
            _, asset_id = sel

            date_str = dt.isoformat()
            if slug in seen_slugs:
                continue
            seen_slugs.add(slug)

            out.append(
                {
                    "asset_id": asset_id,
                    "slug": slug,
                    "close_ts": close_ts,
                    "outcome": "Up",  # canonical for our strategies
                    "date": date_str,
                }
            )

    out.sort(key=lambda m: (m["close_ts"], m["asset_id"]))
    print(f"Discovered {len(out)} markets for prefix={slug_prefixes} {start_date}..{end_date}", file=sys.stderr)
    return out


def main() -> int:
    p = argparse.ArgumentParser()
    p.add_argument("--master-parquet", help="Local path or s3://.../markets-full.parquet (required unless --from-full-jsonl)")
    p.add_argument("--from-full-jsonl", help="Path to an already-discovered full markets.jsonl (fast path, no parquet needed)")
    p.add_argument("--asset", required=True, choices=["eth", "sol", "xrp", "bnb", "doge", "hype", "btc"], help="Short asset name for slug prefix")
    p.add_argument("--start-date", required=True)
    p.add_argument("--end-date", required=True)
    p.add_argument("--train-n", type=int, default=4500, help="How many markets for the small meta-calibrator train slice (head)")
    p.add_argument("--upload-bucket", default=None)
    p.add_argument("--upload-prefix", default=None, help="e.g. markets/eth-5m-feb-may-2026")
    p.add_argument("--out-dir", default=".", help="Local output dir for the two jsonl files")
    p.add_argument("--slug-prefix-override", default=None)
    args = p.parse_args()

    if not args.master_parquet and not args.from_full_jsonl:
        p.error("one of --master-parquet or --from-full-jsonl is required")

    prefix_map = {
        "eth": "eth-updown-5m-",
        "sol": "sol-updown-5m-",
        "xrp": "xrp-updown-5m-",
        "bnb": "bnb-updown-5m-",
        "doge": "doge-updown-5m-",
        "hype": "hype-updown-5m-",
        "btc": "btc-updown-5m-",
    }
    slug_prefix = args.slug_prefix_override or prefix_map[args.asset]

    if args.from_full_jsonl:
        # Fast path: user already ran discovery once (e.g. the big BTC list) and wants only the ETH slice.
        with open(args.from_full_jsonl) as f:
            all_markets = [json.loads(line) for line in f if line.strip()]
        markets = [
            m for m in all_markets
            if slug_matches_prefixes(m["slug"], slug_prefix)
            and m.get("date", "") >= args.start_date
            and m.get("date", "") <= args.end_date
        ]
        markets.sort(key=lambda m: (m["close_ts"], m["asset_id"]))
        print(f"Filtered {len(markets)} markets from full jsonl for {slug_prefix} {args.start_date}..{args.end_date}", file=sys.stderr)
    else:
        markets = discover_from_parquet(args.master_parquet, slug_prefix, args.start_date, args.end_date)

    if not markets:
        print("ERROR: zero markets discovered — check dates, prefix, and input data", file=sys.stderr)
        return 2

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    full_path = out_dir / "markets.jsonl"
    with full_path.open("w") as f:
        for m in markets:
            f.write(json.dumps(m) + "\n")

    train_n = min(args.train_n, len(markets))
    train_path = out_dir / "markets-train.jsonl"
    with train_path.open("w") as f:
        for m in markets[:train_n]:
            f.write(json.dumps(m) + "\n")

    print(f"Wrote {full_path} ({len(markets)} markets)")
    print(f"Wrote {train_path} (first {train_n} for meta training)")

    if args.upload_bucket and args.upload_prefix:
        s3_full = f"s3://{args.upload_bucket}/{args.upload_prefix}/markets.jsonl"
        s3_train = f"s3://{args.upload_bucket}/{args.upload_prefix}/markets-train.jsonl"
        print(f"Uploading to {s3_full} and {s3_train} ...", file=sys.stderr)
        subprocess.check_call(["aws", "s3", "cp", str(full_path), s3_full])
        subprocess.check_call(["aws", "s3", "cp", str(train_path), s3_train])
        print("Upload complete. Use these in the EC2 launcher:")
        print(f"  --markets-key {args.upload_prefix}/markets.jsonl")
        print(f"  --train-markets-key {args.upload_prefix}/markets-train.jsonl")
        print(f"  --slug-prefixes {slug_prefix}")
        print(f"  --spot-symbol {args.asset.upper()}USDT   (or the correct spot pair)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
