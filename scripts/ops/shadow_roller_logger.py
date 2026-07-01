#!/usr/bin/env python3
"""Shadow BTC 5m complete-set roller — log-only soak for split/dump/redeem timing.

Mirrors wallet 0x4d64518a… roller cadence without submitting CTF or CLOB orders.
Writes JSONL to --out-dir for parity checks against live wallet activity.

Events:
  roller_would_split   — T+split_offset_s after window open
  roller_would_dump    — T-dump_before_close_s when loser best_bid <= penny_max
  roller_would_redeem  — T+redeem_after_close_s after window close

Usage:
  python3 scripts/shadow_roller_logger.py \\
    --out-dir ~/data/pm-alpha/shadow-roller \\
    --clip-usd 1111 --dump-clip-shares 200
"""

from __future__ import annotations

import argparse
import json
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

GAMMA = "https://gamma-api.polymarket.com/markets"
CLOB_BOOK = "https://clob.polymarket.com/book"
WINDOW_S = 300
ASSET = "btc"


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def http_json(url: str, params: dict | None = None, timeout: float = 15.0) -> object:
    if params:
        url = f"{url}?{urllib.parse.urlencode(params)}"
    req = urllib.request.Request(url, headers={"User-Agent": "pm-roller-shadow/1.0"})
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.load(resp)


def current_window_open(now: int | None = None) -> int:
    t = now or int(time.time())
    return t - (t % WINDOW_S)


def slug_for_open(open_ts: int) -> str:
    return f"{ASSET}-updown-5m-{open_ts}"


@dataclass
class MarketMeta:
    slug: str
    open_ts: int
    close_ts: int
    condition_id: str | None
    up_token: str
    down_token: str
    title: str


def parse_gamma(item: dict, slug: str) -> MarketMeta | None:
    if item.get("slug") != slug:
        return None
    open_ts = int(slug.rsplit("-", 1)[-1])
    tokens = item.get("clobTokenIds")
    outcomes = item.get("outcomes")
    if isinstance(tokens, str):
        tokens = json.loads(tokens)
    if isinstance(outcomes, str):
        outcomes = json.loads(outcomes)
    if not tokens or len(tokens) < 2:
        return None
    up_tok = down_tok = None
    if outcomes and len(outcomes) == len(tokens):
        for tok, out in zip(tokens, outcomes):
            ol = (out or "").lower()
            if ol == "up":
                up_tok = tok
            elif ol == "down":
                down_tok = tok
    if not up_tok or not down_tok:
        up_tok, down_tok = tokens[0], tokens[1]
    cid = item.get("conditionId") or item.get("condition_id")
    return MarketMeta(
        slug=slug,
        open_ts=open_ts,
        close_ts=open_ts + WINDOW_S,
        condition_id=cid,
        up_token=up_tok,
        down_token=down_tok,
        title=item.get("question") or item.get("title") or slug,
    )


def fetch_market(slug: str) -> MarketMeta | None:
    try:
        items = http_json(GAMMA, {"slug": slug})
    except Exception:
        return None
    if not isinstance(items, list):
        return None
    for item in items:
        meta = parse_gamma(item, slug)
        if meta:
            return meta
    return None


def best_bid(token_id: str) -> float | None:
    try:
        book = http_json(CLOB_BOOK, {"token_id": token_id})
    except Exception:
        return None
    bids = book.get("bids") or []
    if not bids:
        return None
    try:
        return max(float(b["price"]) for b in bids if b.get("price") is not None)
    except (TypeError, ValueError):
        return None


class RollerShadow:
    def __init__(
        self,
        out_path: Path,
        clip_usd: float,
        dump_clip: float,
        split_offset_s: int,
        dump_before_close_s: int,
        redeem_after_close_s: int,
        penny_max: float,
    ) -> None:
        self.out_path = out_path
        self.clip_usd = clip_usd
        self.dump_clip = dump_clip
        self.split_offset_s = split_offset_s
        self.dump_before_close_s = dump_before_close_s
        self.redeem_after_close_s = redeem_after_close_s
        self.penny_max = penny_max
        self.split_done: set[str] = set()
        self.dump_done: set[str] = set()
        self.redeem_done: set[str] = set()
        self.meta_cache: dict[str, MarketMeta] = {}

    def log(self, event: dict) -> None:
        event.setdefault("ts_utc", utc_now())
        line = json.dumps(event, separators=(",", ":"))
        with self.out_path.open("a", encoding="utf-8") as f:
            f.write(line + "\n")
        print(line, flush=True)

    def meta(self, slug: str) -> MarketMeta | None:
        if slug in self.meta_cache:
            return self.meta_cache[slug]
        m = fetch_market(slug)
        if m:
            self.meta_cache[slug] = m
        return m

    def tick(self, now: int) -> None:
        open_ts = current_window_open(now)
        for offset in (0, WINDOW_S, -WINDOW_S):
            slug = slug_for_open(open_ts + offset)
            m = self.meta(slug)
            if not m:
                continue
            age = now - m.open_ts
            to_close = m.close_ts - now

            if (
                slug not in self.split_done
                and age >= self.split_offset_s
                and age < self.split_offset_s + 30
            ):
                self.split_done.add(slug)
                self.log({
                    "type": "roller_would_split",
                    "slug": slug,
                    "title": m.title,
                    "condition_id": m.condition_id,
                    "clip_usd": self.clip_usd,
                    "shares": self.clip_usd,
                    "secs_from_open": age,
                })

            if (
                slug not in self.dump_done
                and 0 < to_close <= self.dump_before_close_s
            ):
                up_bid = best_bid(m.up_token)
                down_bid = best_bid(m.down_token)
                loser = None
                loser_bid = None
                if up_bid is not None and up_bid <= self.penny_max:
                    loser, loser_bid = "up", up_bid
                elif down_bid is not None and down_bid <= self.penny_max:
                    loser, loser_bid = "down", down_bid
                if loser:
                    self.dump_done.add(slug)
                    est_usd = self.dump_clip * loser_bid
                    self.log({
                        "type": "roller_would_dump",
                        "slug": slug,
                        "side": loser,
                        "shares": self.dump_clip,
                        "limit_price": loser_bid,
                        "est_usd": round(est_usd, 4),
                        "up_bid": up_bid,
                        "down_bid": down_bid,
                        "secs_to_close": to_close,
                    })

            since_close = now - m.close_ts
            if (
                slug not in self.redeem_done
                and since_close >= self.redeem_after_close_s
                and since_close < self.redeem_after_close_s + 600
            ):
                self.redeem_done.add(slug)
                self.log({
                    "type": "roller_would_redeem",
                    "slug": slug,
                    "condition_id": m.condition_id,
                    "shares": self.clip_usd,
                    "est_usd": self.clip_usd,
                    "secs_after_close": since_close,
                })

        # prune old slugs
        if len(self.split_done) > 500:
            self.split_done = set(sorted(self.split_done)[-300:])
            self.dump_done = set(sorted(self.dump_done)[-300:])
            self.redeem_done = set(sorted(self.redeem_done)[-300:])


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out-dir", required=True)
    ap.add_argument("--clip-usd", type=float, default=1111.0)
    ap.add_argument("--dump-clip-shares", type=float, default=200.0)
    ap.add_argument("--split-offset-s", type=int, default=10)
    ap.add_argument("--dump-before-close-s", type=int, default=15)
    ap.add_argument("--redeem-after-close-s", type=int, default=30)
    ap.add_argument("--penny-max", type=float, default=0.02)
    ap.add_argument("--poll-s", type=float, default=1.0)
    args = ap.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    day = datetime.now(timezone.utc).strftime("%Y%m%d")
    out_path = out_dir / f"roller-{day}.jsonl"

    roller = RollerShadow(
        out_path=out_path,
        clip_usd=args.clip_usd,
        dump_clip=args.dump_clip_shares,
        split_offset_s=args.split_offset_s,
        dump_before_close_s=args.dump_before_close_s,
        redeem_after_close_s=args.redeem_after_close_s,
        penny_max=args.penny_max,
    )

    print(f"# shadow roller logger → {out_path}")
    print(
        f"# clip=${args.clip_usd:.0f} dump={args.dump_clip_shares:.0f}sh "
        f"split+{args.split_offset_s}s dump-{args.dump_before_close_s}s redeem+{args.redeem_after_close_s}s"
    )

    while True:
        try:
            roller.tick(int(time.time()))
        except KeyboardInterrupt:
            print("\n# stopped")
            return 0
        except Exception as exc:
            print(json.dumps({"type": "roller_error", "error": str(exc), "ts_utc": utc_now()}))
        time.sleep(args.poll_s)


if __name__ == "__main__":
    raise SystemExit(main())