#!/usr/bin/env python3
"""Backfill OFFICIAL Polymarket open/close prices per updown window from
polymarket.com/api/crypto/crypto-price. Kills the Binance-open strike proxy
and gives an independent resolution check (closePrice vs openPrice).

Usage: python3 scripts/polymarket_strikes_fetch.py <manifest.jsonl> <out.jsonl> [slug_prefix]
Manifest rows: MarketHandle JSON (asset_id, slug, close_ts, outcome, date).
Output rows: {"slug", "open_ts", "open_price", "close_price", "completed"}.
Resumable: existing out rows are skipped. ~8 req/s with retry on 429.
"""
import json
import sys
import time
from urllib.request import urlopen, Request

VARIANT = {"5m": "fiveminute", "15m": "fifteen", "1h": "hourly", "4h": "fourhour"}
DUR = {"5m": 300, "15m": 900, "1h": 3600, "4h": 14400}


def window_kind(slug):
    for k in DUR:
        if f"-updown-{k}-" in slug:
            return k
    return None


def fetch(symbol, kind, open_ts):
    url = (f"https://polymarket.com/api/crypto/crypto-price?symbol={symbol}"
           f"&eventStartTime={open_ts}&variant={VARIANT[kind]}&endDate={open_ts + DUR[kind]}")
    for attempt in range(6):
        try:
            req = Request(url, headers={"User-Agent": "pm-backtest-research/1.0"})
            with urlopen(req, timeout=20) as r:
                return json.load(r)
        except Exception:
            time.sleep(1.5 * (attempt + 1))
    return None


def main():
    manifest, out_path = sys.argv[1], sys.argv[2]
    prefix = sys.argv[3] if len(sys.argv) > 3 else ""
    done = set()
    try:
        for line in open(out_path):
            done.add(json.loads(line)["slug"])
    except FileNotFoundError:
        pass

    slugs = {}
    for line in open(manifest):
        line = line.strip()
        if not line:
            continue
        r = json.loads(line)
        slug = r["slug"]
        if not slug.startswith(prefix) or slug in done or slug in slugs:
            continue
        kind = window_kind(slug)
        if kind is None:
            continue
        try:
            open_ts = int(slug.rsplit("-", 1)[1])
        except ValueError:
            continue
        slugs[slug] = (slug.split("-")[0].upper(), kind, open_ts)

    print(f"{len(slugs)} windows to fetch ({len(done)} already done)")
    n_ok = n_fail = 0
    with open(out_path, "a") as f:
        for i, (slug, (symbol, kind, open_ts)) in enumerate(sorted(slugs.items())):
            d = fetch(symbol, kind, open_ts)
            if d and d.get("openPrice"):
                f.write(json.dumps({
                    "slug": slug, "open_ts": open_ts,
                    "open_price": d["openPrice"],
                    "close_price": d.get("closePrice"),
                    "completed": d.get("completed"),
                }) + "\n")
                n_ok += 1
            else:
                n_fail += 1
            if i % 500 == 0:
                f.flush()
                print(f"  {i}/{len(slugs)} ok={n_ok} fail={n_fail}")
            time.sleep(0.12)
    print(f"done: ok={n_ok} fail={n_fail}")


if __name__ == "__main__":
    main()
