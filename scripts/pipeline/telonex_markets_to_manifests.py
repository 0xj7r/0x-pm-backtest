#!/usr/bin/env python3
"""Convert the free Telonex markets metadata parquet (canonical: true
resolutions via result_id, both asset ids) into MarketHandle manifests for
the updown families. Replaces availability-API crawling and label inference.

Usage: python3 scripts/pipeline/telonex_markets_to_manifests.py /tmp/telonex_markets.parquet data/manifests/canonical
Outputs per family: {fam}_up.jsonl (canonical Up asset, outcome = true winner)
and one down_all.jsonl (Down assets for the real-NO pairing map).
"""
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

import pyarrow.parquet as pq

FAMS = ("-updown-5m-", "-updown-15m-", "-updown-1h-", "-updown-4h-")


def main():
    src, out_dir = sys.argv[1], Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)
    pf = pq.ParquetFile(src)
    cols = ["slug", "outcome_0", "outcome_1", "asset_id_0", "asset_id_1",
            "status", "result_id", "book_snapshot_25_from"]
    up_files, down = {}, []
    n_seen = n_unresolved = 0
    for rg in range(pf.metadata.num_row_groups):
        t = pf.read_row_group(rg, columns=cols)
        for r in t.to_pylist():
            slug = r["slug"] or ""
            if not any(f in slug for f in FAMS):
                continue
            n_seen += 1
            if r["status"] != "resolved" or r["result_id"] not in ("0", "1"):
                n_unresolved += 1
                continue
            try:
                open_ts = int(slug.rsplit("-", 1)[1])
            except ValueError:
                continue
            date = datetime.fromtimestamp(open_ts, timezone.utc).date().isoformat()
            winner = r[f"outcome_{r['result_id']}"]
            fam = "-".join(slug.split("-")[:3])
            up_idx = "0" if r["outcome_0"] == "Up" else ("1" if r["outcome_1"] == "Up" else None)
            if up_idx is None:
                continue
            down_idx = "1" if up_idx == "0" else "0"
            row_up = {"asset_id": r[f"asset_id_{up_idx}"], "slug": slug,
                      "close_ts": open_ts, "outcome": winner, "date": date}
            row_down = {"asset_id": r[f"asset_id_{down_idx}"], "slug": slug,
                        "close_ts": open_ts, "outcome": winner, "date": date}
            if fam not in up_files:
                up_files[fam] = open(out_dir / f"{fam}_up.jsonl", "w")
            up_files[fam].write(json.dumps(row_up) + "\n")
            down.append(row_down)
    with open(out_dir / "down_all.jsonl", "w") as f:
        for r in down:
            f.write(json.dumps(r) + "\n")
    for fam, fh in up_files.items():
        fh.close()
    print(f"updown rows seen={n_seen} unresolved-skipped={n_unresolved} down_map={len(down)}")
    for p in sorted(out_dir.glob("*_up.jsonl")):
        print(p.name, sum(1 for _ in open(p)))


if __name__ == "__main__":
    main()
