#!/usr/bin/env python3
"""Build an unlabeled markets.jsonl manifest from local book_snapshot_25 parquet.

One JSON object per (deduped) asset_id with fields: asset_id, slug, close_ts, outcome, date.
slug is read from the parquet; the integer suffix of the slug (btc-updown-5m-<open_ts>)
is the market OPEN epoch, so close_ts = open_ts + MARKET_DURATION_SECS (a 5-minute
market closes 300s after it opens). date comes from the partition path.

The local book cache does not contain settlement labels, so outcome is set to
"Unknown". Do not run these manifests with walk-forward --use-outcome-label;
use discover-markets-parquet against the labeled S3 markets parquet instead.
"""
import glob
import json
import os
import re
import sys

import pyarrow.parquet as pq

REPO_ROOT = os.environ.get("PM_REPO_ROOT", "/Users/jackreid/go/polymarket-backtest")
CACHE_ROOT = os.path.join(
    REPO_ROOT,
    "data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25",
)
SLUG_RE = re.compile(r"btc-updown-5m-(\d+)")
MARKET_DURATION_SECS = 300


def main(out_path: str) -> None:
    date_dirs = sorted(glob.glob(os.path.join(CACHE_ROOT, "date=*")))
    seen: dict[str, dict] = {}
    n_files = 0
    n_skipped = 0
    for dd in date_dirs:
        date = dd.split("date=")[-1]
        for pf in glob.glob(os.path.join(dd, "asset_id=*", "*.parquet")):
            n_files += 1
            if n_files % 500 == 0:
                print(
                    f"progress files={n_files} unique={len(seen)}",
                    file=sys.stderr,
                    flush=True,
                )
            asset_id = pf.split("asset_id=")[1].split(os.sep)[0]
            if asset_id in seen:
                continue
            try:
                pfh = pq.ParquetFile(pf)
                batch = next(
                    pfh.iter_batches(batch_size=1, columns=["slug"])
                )
                slug = batch.column("slug")[0].as_py()
            except StopIteration:
                n_skipped += 1
                continue
            except Exception as exc:  # noqa: BLE001
                print(f"WARN read {pf}: {exc}", file=sys.stderr)
                n_skipped += 1
                continue
            if not slug:
                n_skipped += 1
                continue
            m = SLUG_RE.search(slug)
            if not m:
                n_skipped += 1
                continue
            close_ts = int(m.group(1)) + MARKET_DURATION_SECS
            seen[asset_id] = {
                "asset_id": asset_id,
                "slug": slug,
                "close_ts": close_ts,
                "outcome": "Unknown",
                "date": date,
            }

    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    rows = sorted(seen.values(), key=lambda r: (r["close_ts"], r["asset_id"]))
    with open(out_path, "w") as fh:
        for r in rows:
            fh.write(json.dumps(r) + "\n")
    print(
        f"scanned files={n_files} unique_assets={len(rows)} skipped={n_skipped} "
        f"-> {out_path}"
    )


if __name__ == "__main__":
    out = (
        sys.argv[1]
        if len(sys.argv) > 1
        else "data/runs/volgate/markets-local-unlabeled.jsonl"
    )
    main(out)
