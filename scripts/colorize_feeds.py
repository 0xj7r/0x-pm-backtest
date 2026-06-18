#!/usr/bin/env python3
"""Colorize shadow-final WS feed telemetry for tmux feed panes.

Reads interleaved JSONL (summary, venue_lead_lag) and engine log lines from
stdin. Filter with argv[1]: summary | binance | perp | book | kraken | coinbase
"""
from __future__ import annotations

import json
import re
import sys

MODE = (sys.argv[1] if len(sys.argv) > 1 else "summary").lower()

R = "\033[0m"
DIM = "\033[90m"
OK = "\033[1;32m"
WARN = "\033[1;33m"
BAD = "\033[1;31m"
INFO = "\033[1;36m"

ACCENTS = {
    "summary": "\033[1;36m",
    "binance": "\033[1;33m",
    "perp": "\033[1;35m",
    "book": "\033[1;34m",
    "kraken": "\033[1;32m",
    "coinbase": "\033[1;37m",
}
accent = ACCENTS.get(MODE, "\033[37m")
LABEL = MODE[:4].upper()

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
TS_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")


def age_color(ms: int | None) -> str:
    if ms is None:
        return f"{DIM}n/a{R}"
    if ms < 500:
        return f"{OK}{ms:4d}ms{R}"
    if ms < 2000:
        return f"{WARN}{ms:4d}ms{R}"
    return f"{BAD}{ms:4d}ms STALE{R}"


def head(ts: str) -> str:
    return f"{accent}{ts} {LABEL:<4}{R}"


def parse_json(line: str) -> dict | None:
    line = line.strip()
    if not line.startswith("{"):
        return None
    try:
        return json.loads(line)
    except json.JSONDecodeError:
        return None


def log_ts(line: str) -> str:
    m = TS_RE.search(line)
    return m.group(1)[11:19] if m else "??:??:??"


def matches_log(mode: str, plain: str) -> bool:
    low = plain.lower()
    if mode == "binance":
        return any(
            k in low
            for k in (
                "binance spot",
                "spot klines",
                "spot buffer",
                "malformed binance",
            )
        )
    if mode == "perp":
        return any(
            k in low
            for k in (
                "binance futures",
                "binance perp",
                "futures ws",
                "perp klines",
                "perp buffer",
                "malformed futures",
            )
        )
    if mode == "book":
        return any(
            k in low
            for k in (
                "polymarket book",
                "book subscription",
                "book feed",
                "malformed book",
            )
        )
    if mode == "kraken":
        return "kraken" in low
    if mode == "coinbase":
        return "coinbase" in low
    return False


def format_log(mode: str, plain: str) -> str | None:
    if not matches_log(mode, plain):
        return None
    ts = log_ts(plain)
    h = head(ts)
    low = plain.lower()
    if "connected" in low:
        return f"{h} {OK}CONNECTED{R} {plain.split('connected')[-1].strip()[:60]}"
    if "resubscrib" in low:
        return f"{h} {WARN}RESUB{R} {plain[-80:]}"
    if any(k in low for k in ("failed", "reconnecting", "stale", "closed", "ended")):
        return f"{h} {BAD}WARN{R} {plain[-100:]}"
    if "malformed" in low or "skipping" in low:
        return f"{h} {DIM}skip {plain[-90:]}{R}"
    if "pre-warm" in low or "klines" in low:
        return f"{h} {INFO}warm {plain[-90:]}{R}"
    return f"{h} {plain[-110:]}"


def format_summary(o: dict) -> str:
    ts = o.get("ts_utc", "")[11:19]
    h = head(ts)
    b_age = o.get("binance_feed_age_ms")
    bk_age = o.get("book_feed_age_ms")
    b_lat = o.get("median_binance_receipt_minus_exchange_ms")
    bk_lat = o.get("median_book_receipt_minus_exchange_ms")
    return (
        f"{h} spot={age_color(b_age)} book={age_color(bk_age)} "
        f"lat_bn={b_lat if b_lat is not None else 'n/a'}ms "
        f"lat_book={bk_lat if bk_lat is not None else 'n/a'}ms "
        f"active={o.get('n_active_markets')} entries={o.get('n_entries_total')}"
    )


def format_venue(mode: str, o: dict) -> str | None:
    venue = str(o.get("venue", "")).lower()
    if venue != mode:
        return None
    ts = o.get("ts_utc", "")[11:19]
    h = head(ts)
    lead = o.get("best_lead_ms")
    corr = o.get("corr")
    n = o.get("n_samples")
    med = o.get("median_receipt_minus_exchange_ms")
    lead_c = OK if isinstance(lead, int) and lead > 0 else WARN if lead == 0 else DIM
    return (
        f"{h} {lead_c}lead={lead}ms{R} corr={corr:.3f} n={n} "
        f"med_rx={med if med is not None else 'n/a'}ms"
    )


def format_json(mode: str, o: dict) -> str | None:
    t = o.get("type", "")
    if mode == "summary" and t == "summary":
        return format_summary(o)
    if mode in ("kraken", "coinbase") and t == "venue_lead_lag":
        return format_venue(mode, o)
    if mode == "binance" and t == "summary":
        ts = o.get("ts_utc", "")[11:19]
        return (
            f"{head(ts)} spot_age={age_color(o.get('binance_feed_age_ms'))} "
            f"lat={o.get('median_binance_receipt_minus_exchange_ms', 'n/a')}ms"
        )
    if mode == "book" and t == "summary":
        ts = o.get("ts_utc", "")[11:19]
        return (
            f"{head(ts)} book_age={age_color(o.get('book_feed_age_ms'))} "
            f"lat={o.get('median_book_receipt_minus_exchange_ms', 'n/a')}ms"
        )
    if mode == "perp" and t == "summary":
        return None
    return None


for raw in sys.stdin:
    raw = raw.rstrip("\n")
    if not raw:
        continue
    o = parse_json(raw)
    if o is not None:
        out = format_json(MODE, o)
        if out:
            print(out)
        continue
    plain = ANSI_RE.sub("", raw).strip()
    if MODE == "summary":
        continue
    out = format_log(MODE, plain)
    if out:
        print(out)
sys.stdout.flush()