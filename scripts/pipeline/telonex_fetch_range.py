#!/usr/bin/env python3
"""Download Telonex book/trade parquets via REST API into the local cache layout.

S3 mirror (pm-research-data-prod) stops at 2026-06-07; post-gap days must be
fetched directly from api.telonex.io. Layout matches prep-cache / walk-forward:

  data/cache/raw/telonex/exchange=polymarket/channel={ch}/date={d}/asset_id={aid}/

Usage:
  python3 scripts/pipeline/telonex_fetch_range.py 2026-06-08 2026-06-17
  python3 scripts/pipeline/telonex_fetch_range.py 2026-06-08 2026-06-17 --slug-prefix btc-updown-5m-
  TELONEX_API_KEY=tlx_... python3 scripts/pipeline/telonex_fetch_range.py ...
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import date, datetime, timedelta, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CACHE = REPO / "data/cache/raw/telonex/exchange=polymarket"
API = "https://api.telonex.io/v1/downloads/polymarket"
CHANNELS = ("book_snapshot_25", "trades")
MARKETS_URL = "https://api.telonex.io/v1/datasets/polymarket/markets"


def load_api_key() -> str:
    key = os.environ.get("TELONEX_API_KEY", "").strip()
    if key:
        return key
    env_local = REPO / ".env.local"
    if env_local.exists():
        for line in env_local.read_text().splitlines():
            if line.startswith("TELONEX_API_KEY="):
                return line.split("=", 1)[1].strip()
    sys.exit("TELONEX_API_KEY not set (env or .env.local)")


def daterange(start: date, end: date):
    d = start
    while d <= end:
        yield d.isoformat()
        d += timedelta(days=1)


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


_NO_REDIRECT = urllib.request.build_opener(_NoRedirect())


def _fetch_presigned(url: str, timeout: int = 120) -> bytes | None:
    req = urllib.request.Request(url)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise


def _download_once(api_key: str, channel: str, day: str, asset_id: str) -> bytes | str:
    url = f"{API}/{channel}/{day}?asset_id={asset_id}"
    req = urllib.request.Request(url, headers={"Authorization": f"Bearer {api_key}"})
    try:
        with _NO_REDIRECT.open(req, timeout=60) as resp:
            return resp.read()
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return "missing"
        if e.code in (301, 302, 303, 307, 308):
            loc = e.headers.get("Location")
            if not loc:
                return "no_location"
            try:
                data = _fetch_presigned(loc)
            except Exception as err:
                return f"err:{err}"
            if data is None:
                return "missing"
            return data
        if e.code == 429:
            return "rate_limit"
        return f"http{e.code}"
    except Exception as e:
        return f"err:{e}"


def download_parquet(api_key: str, channel: str, day: str, asset_id: str, out: Path) -> str:
    if out.exists() and out.stat().st_size > 0:
        return "skip"
    out.parent.mkdir(parents=True, exist_ok=True)
    data: bytes | str | None = None
    for attempt in range(5):
        data = _download_once(api_key, channel, day, asset_id)
        if isinstance(data, bytes):
            break
        if data == "missing":
            return "missing"
        if data == "rate_limit" or (isinstance(data, str) and data.startswith("err:")):
            time.sleep(1.5 * (attempt + 1))
            continue
        if isinstance(data, str) and data.startswith("http"):
            time.sleep(0.5 * (attempt + 1))
            continue
        break
    if not isinstance(data, bytes):
        return data if isinstance(data, str) else "fail"
    if len(data) < 64:
        return "empty"
    tmp = out.with_suffix(".parquet.part")
    tmp.write_bytes(data)
    tmp.rename(out)
    return "ok"


def iter_market_pairs(
    markets_parquet: Path,
    slug_prefix: str,
    start: date,
    end: date,
    up_only: bool = False,
):
    import pyarrow.parquet as pq

    cols = ["slug", "outcome_0", "outcome_1", "asset_id_0", "asset_id_1"]
    pf = pq.ParquetFile(markets_parquet)
    for rg in range(pf.metadata.num_row_groups):
        t = pf.read_row_group(rg, columns=cols)
        for r in t.to_pylist():
            slug = r["slug"] or ""
            if not slug.startswith(slug_prefix):
                continue
            try:
                open_ts = int(slug.rsplit("-", 1)[1])
            except ValueError:
                continue
            mkt_day = datetime.fromtimestamp(open_ts, timezone.utc).date()
            if mkt_day < start or mkt_day > end:
                continue
            day = mkt_day.isoformat()
            if up_only:
                up_idx = (
                    0
                    if r["outcome_0"] == "Up"
                    else (1 if r["outcome_1"] == "Up" else None)
                )
                if up_idx is None:
                    continue
                aid = r[f"asset_id_{up_idx}"]
                if aid:
                    yield day, str(aid)
            else:
                for idx in (0, 1):
                    aid = r[f"asset_id_{idx}"]
                    if aid:
                        yield day, str(aid)


def build_tasks(
    markets_parquet: Path,
    slug_prefix: str,
    start: date,
    end: date,
    up_only: bool = False,
) -> list[tuple[str, str, str, Path]]:
    seen: set[tuple[str, str, str]] = set()
    tasks: list[tuple[str, str, str, Path]] = []
    for day, aid in iter_market_pairs(
        markets_parquet, slug_prefix, start, end, up_only=up_only
    ):
        for ch in CHANNELS:
            key = (ch, day, aid)
            if key in seen:
                continue
            seen.add(key)
            fname = f"{aid}_{day}_{ch}.parquet"
            out = CACHE / f"channel={ch}" / f"date={day}" / f"asset_id={aid}" / fname
            tasks.append((ch, day, aid, out))
    return tasks


def main() -> None:
    ap = argparse.ArgumentParser(description="Fetch Telonex parquets for a date range")
    ap.add_argument("start_date")
    ap.add_argument("end_date")
    ap.add_argument("--slug-prefix", default="btc-updown-5m-")
    ap.add_argument(
        "--markets-parquet",
        default=str(REPO / "data/cache/telonex_markets.parquet"),
        help="local markets metadata parquet (downloaded if missing)",
    )
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument(
        "--up-only",
        action="store_true",
        help="fetch Up outcome assets only (enough for btc-updown-5m_up manifest)",
    )
    args = ap.parse_args()

    start = date.fromisoformat(args.start_date)
    end = date.fromisoformat(args.end_date)
    if end < start:
        sys.exit("--end-date must be >= --start-date")

    api_key = load_api_key()
    mp = Path(args.markets_parquet)
    if not mp.exists():
        print(f"downloading markets parquet -> {mp}")
        mp.parent.mkdir(parents=True, exist_ok=True)
        urllib.request.urlretrieve(MARKETS_URL, mp)

    tasks = build_tasks(mp, args.slug_prefix, start, end, up_only=args.up_only)
    mode = "up-only" if args.up_only else "both-legs"
    print(
        f"tasks: {len(tasks)} ({args.slug_prefix} {args.start_date}..{args.end_date} {mode})"
    )

    counts = {"ok": 0, "skip": 0, "missing": 0, "fail": 0}
    t0 = time.time()
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        futs = {
            pool.submit(download_parquet, api_key, ch, day, aid, out): (ch, day, aid)
            for ch, day, aid, out in tasks
        }
        for i, fut in enumerate(as_completed(futs), 1):
            status = fut.result()
            if status == "ok":
                counts["ok"] += 1
            elif status == "skip":
                counts["skip"] += 1
            elif status == "missing":
                counts["missing"] += 1
            else:
                counts["fail"] += 1
            if i % 200 == 0 or i == len(futs):
                elapsed = time.time() - t0
                print(
                    f"progress {i}/{len(futs)} "
                    f"ok={counts['ok']} skip={counts['skip']} "
                    f"missing={counts['missing']} fail={counts['fail']} "
                    f"({elapsed:.0f}s)",
                    flush=True,
                )

    print(json.dumps({"counts": counts, "elapsed_s": round(time.time() - t0, 1)}))


if __name__ == "__main__":
    main()