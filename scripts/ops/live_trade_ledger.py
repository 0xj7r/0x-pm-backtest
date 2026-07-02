#!/usr/bin/env python3
"""SQLite ledger for LIVE submits matched to shadow-final entries + resolutions.

Idempotent offset-tracked ingest from shadow-final JSONL and shadow_exec_tail.log.
Matched legs carry gross_pnl, fee (curve), and net_pnl.

Usage (Dublin):
  python3 scripts/ops/live_trade_ledger.py ingest
  python3 scripts/ops/live_trade_ledger.py daily-report
  python3 scripts/ops/live_trade_ledger.py query --date 2026-06-17

From Mac:
  ./scripts/ops/live_trade_ledger.sh ingest
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import sqlite3
from collections import defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
ISO_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")
TS_RE = re.compile(r"(?P<ts>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
SUBMITTED_RE = re.compile(
    r"shadow SUBMITTED.*slug=(?P<slug>\S+).*accepted=(?P<acc>true|false)"
)
REDEEM_RE = re.compile(r"redeem OK slug=(?P<slug>\S+)")

DDL = """
CREATE TABLE IF NOT EXISTS ingest_state (
  source TEXT PRIMARY KEY,
  offset_lines INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS live_submits (
  ts_utc TEXT NOT NULL,
  slug TEXT NOT NULL,
  accepted INTEGER NOT NULL,
  PRIMARY KEY (slug, ts_utc)
);
CREATE TABLE IF NOT EXISTS shadow_entries (
  ts_utc TEXT NOT NULL,
  slug TEXT NOT NULL,
  side TEXT NOT NULL,
  clip INTEGER NOT NULL,
  touch_price REAL,
  p_exo REAL,
  edge REAL,
  PRIMARY KEY (slug, side, ts_utc)
);
CREATE TABLE IF NOT EXISTS resolutions (
  ts_utc TEXT NOT NULL,
  slug TEXT NOT NULL,
  side TEXT NOT NULL,
  won INTEGER NOT NULL,
  settle_pnl_per_share REAL,
  PRIMARY KEY (slug, side, ts_utc)
);
CREATE TABLE IF NOT EXISTS legs (
  submit_ts_utc TEXT NOT NULL,
  entry_ts_utc TEXT,
  resolution_ts_utc TEXT,
  slug TEXT NOT NULL,
  side TEXT,
  clip INTEGER NOT NULL,
  touch_price REAL,
  won INTEGER,
  shares REAL,
  gross_pnl REAL,
  fee REAL,
  net_pnl REAL,
  status TEXT NOT NULL,
  PRIMARY KEY (slug, clip, submit_ts_utc)
);
"""


def default_db() -> Path:
    dublin = Path.home() / "data" / "pm-alpha" / "live_ledger.db"
    if dublin.parent.is_dir():
        return dublin
    local = Path(__file__).resolve().parents[2] / "data" / "runs" / "live_ledger.db"
    local.parent.mkdir(parents=True, exist_ok=True)
    return local


def default_shadow_dir() -> Path:
    return Path(os.path.expanduser("~/data/pm-alpha/shadow-final"))


def default_live_log() -> Path:
    return Path(os.path.expanduser("~/data/pm-alpha/shadow_exec_tail.log"))


def curve_fee(rate: float, price: float, shares: float) -> float:
    return rate * price * (1.0 - price) * shares


def leg_pnl(
    touch: float, won: bool, clip_usd: float, fee_curve_rate: float
) -> tuple[float, float, float, float]:
    if touch <= 0:
        return 0.0, 0.0, 0.0, 0.0
    shares = clip_usd / touch
    sps = (1.0 - touch) if won else (-touch)
    gross = sps * shares
    fee = curve_fee(fee_curve_rate, touch, shares)
    return shares, gross, fee, gross - fee


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def ingest_offset(db: sqlite3.Connection, source: str) -> int:
    row = db.execute(
        "SELECT offset_lines FROM ingest_state WHERE source=?", (source,)
    ).fetchone()
    return int(row[0]) if row else 0


def set_ingest_offset(db: sqlite3.Connection, source: str, offset: int) -> None:
    db.execute(
        "INSERT INTO ingest_state VALUES (?,?) "
        "ON CONFLICT(source) DO UPDATE SET offset_lines=excluded.offset_lines",
        (source, offset),
    )


def ingest_shadow_jsonl(db: sqlite3.Connection, shadow_dir: Path) -> int:
    n_new = 0
    for path in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        start = ingest_offset(db, path)
        n = start
        with open(path, encoding="utf-8") as fh:
            for i, line in enumerate(fh):
                if i < start:
                    continue
                n = i + 1
                line = line.strip()
                if not line:
                    continue
                try:
                    r = json.loads(line)
                except json.JSONDecodeError:
                    continue
                t = r.get("type")
                if t == "would_enter":
                    touch = r.get("touch_price")
                    if touch is None and isinstance(r.get("touch"), dict):
                        touch = r["touch"].get("price")
                    cur = db.execute(
                        "INSERT OR IGNORE INTO shadow_entries VALUES (?,?,?,?,?,?,?)",
                        (
                            r.get("ts_utc"),
                            r.get("slug"),
                            r.get("side"),
                            int(r.get("clip", 1)),
                            touch,
                            r.get("p_exo"),
                            r.get("edge"),
                        ),
                    )
                    n_new += cur.rowcount
                elif t == "resolution":
                    cur = db.execute(
                        "INSERT OR IGNORE INTO resolutions VALUES (?,?,?,?,?)",
                        (
                            r.get("ts_utc"),
                            r.get("slug"),
                            r.get("side"),
                            1 if r.get("won") else 0,
                            r.get("settle_pnl_per_share"),
                        ),
                    )
                    n_new += cur.rowcount
        set_ingest_offset(db, path, n)
    return n_new


def ingest_live_log(db: sqlite3.Connection, live_log: Path) -> int:
    if not live_log.is_file():
        return 0
    start = ingest_offset(db, str(live_log))
    n_new = 0
    n = start
    with live_log.open(encoding="utf-8", errors="replace") as fh:
        for i, raw in enumerate(fh):
            if i < start:
                continue
            n = i + 1
            line = strip_ansi(raw)
            iso = ISO_RE.search(line)
            ts: str | None = None
            if iso:
                ts = iso.group(1)
            else:
                ts_m = TS_RE.search(line)
                if ts_m:
                    ts = ts_m.group("ts") + "Z"
            if not ts:
                continue
            sm = SUBMITTED_RE.search(line)
            if sm:
                cur = db.execute(
                    "INSERT OR IGNORE INTO live_submits VALUES (?,?,?)",
                    (ts, sm.group("slug"), 1 if sm.group("acc") == "true" else 0),
                )
                n_new += cur.rowcount
    set_ingest_offset(db, str(live_log), n)
    return n_new


def index_resolutions(db: sqlite3.Connection) -> dict[tuple[str, str, int], dict]:
    by_ent: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for row in db.execute(
        "SELECT ts_utc, slug, side, clip FROM shadow_entries ORDER BY ts_utc"
    ):
        by_ent[(row[1], row[2])].append(
            {"ts_utc": row[0], "slug": row[1], "side": row[2], "clip": int(row[3])}
        )

    by_res: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for row in db.execute(
        "SELECT ts_utc, slug, side, won FROM resolutions ORDER BY ts_utc"
    ):
        by_res[(row[1], row[2])].append(
            {"ts_utc": row[0], "slug": row[1], "side": row[2], "won": bool(row[3])}
        )

    out: dict[tuple[str, str, int], dict] = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent["clip"]))] = {**ent, **res}
    return out


def load_redeemed(live_log: Path) -> set[str]:
    redeemed: set[str] = set()
    if not live_log.is_file():
        return redeemed
    for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
        rm = REDEEM_RE.search(strip_ansi(raw))
        if rm:
            redeemed.add(rm.group("slug"))
    return redeemed


def rebuild_legs(
    db: sqlite3.Connection,
    *,
    clip_usd: float,
    fee_curve_rate: float,
    live_log: Path,
) -> int:
    res_idx = index_resolutions(db)
    redeemed = load_redeemed(live_log)

    entries_by_slug: dict[str, list[dict]] = defaultdict(list)
    for row in db.execute(
        "SELECT ts_utc, slug, side, clip, touch_price, p_exo, edge "
        "FROM shadow_entries ORDER BY ts_utc"
    ):
        entries_by_slug[row[1]].append(
            {
                "ts_utc": row[0],
                "slug": row[1],
                "side": row[2],
                "clip": int(row[3]),
                "touch_price": row[4],
                "p_exo": row[5],
                "edge": row[6],
            }
        )

    db.execute("DELETE FROM legs")
    seen: set[tuple[str, int]] = set()
    n = 0
    for row in db.execute(
        "SELECT ts_utc, slug, accepted FROM live_submits "
        "WHERE accepted=1 ORDER BY ts_utc"
    ):
        submit_ts, slug, _ = row[0], row[1], row[2]
        fe = parse_ts(submit_ts).timestamp()
        best = None
        best_dt = 1e18
        for ent in entries_by_slug.get(slug, []):
            dt = abs(parse_ts(ent["ts_utc"]).timestamp() - fe)
            if dt <= 120 and dt < best_dt:
                best_dt = dt
                best = ent
        if not best:
            db.execute(
                "INSERT OR REPLACE INTO legs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    submit_ts,
                    None,
                    None,
                    slug,
                    None,
                    0,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    "unmatched",
                ),
            )
            n += 1
            continue

        clip = int(best["clip"])
        key = (slug, clip)
        if key in seen:
            continue
        seen.add(key)

        touch = float(best.get("touch_price") or 0)
        res = res_idx.get((slug, best["side"], clip))
        status = "open"
        won = resolution_ts = None
        shares = gross = fee = net = None
        if res and slug in redeemed:
            won = bool(res.get("won"))
            resolution_ts = res.get("ts_utc")
            shares, gross, fee, net = leg_pnl(touch, won, clip_usd, fee_curve_rate)
            status = "resolved"

        db.execute(
            "INSERT OR REPLACE INTO legs VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                submit_ts,
                best["ts_utc"],
                resolution_ts,
                slug,
                best["side"],
                clip,
                touch if touch > 0 else None,
                1 if won else (0 if won is False else None),
                shares,
                gross,
                fee,
                net,
                status,
            ),
        )
        n += 1
    return n


@dataclass
class DayRollup:
    date: str
    n_submitted: int
    n_resolved: int
    n_open: int
    wins: int
    gross_usd: float
    fee_usd: float
    net_usd: float


def rollup_by_submit_date(db: sqlite3.Connection) -> list[DayRollup]:
    rows = db.execute(
        "SELECT submit_ts_utc, status, won, gross_pnl, fee, net_pnl FROM legs"
    ).fetchall()
    by_day: dict[str, dict] = defaultdict(
        lambda: {
            "n_submitted": 0,
            "n_resolved": 0,
            "n_open": 0,
            "wins": 0,
            "gross": 0.0,
            "fee": 0.0,
            "net": 0.0,
        }
    )
    for submit_ts, status, won, gross, fee, net in rows:
        day = submit_ts[:10] if submit_ts else "unknown"
        d = by_day[day]
        d["n_submitted"] += 1
        if status == "resolved":
            d["n_resolved"] += 1
            if won:
                d["wins"] += 1
            d["gross"] += float(gross or 0)
            d["fee"] += float(fee or 0)
            d["net"] += float(net or 0)
        elif status == "open":
            d["n_open"] += 1
    out: list[DayRollup] = []
    for day in sorted(by_day):
        d = by_day[day]
        out.append(
            DayRollup(
                date=day,
                n_submitted=d["n_submitted"],
                n_resolved=d["n_resolved"],
                n_open=d["n_open"],
                wins=d["wins"],
                gross_usd=d["gross"],
                fee_usd=d["fee"],
                net_usd=d["net"],
            )
        )
    return out


def cmd_ingest(args: argparse.Namespace) -> int:
    db_path = Path(args.db)
    db_path.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(db_path)
    db.executescript(DDL)

    shadow_n = ingest_shadow_jsonl(db, Path(args.shadow_dir))
    live_n = ingest_live_log(db, Path(args.live_log))
    legs_n = rebuild_legs(
        db,
        clip_usd=args.clip_usd,
        fee_curve_rate=args.fee_curve_rate,
        live_log=Path(args.live_log),
    )
    db.commit()

    counts = {
        "live_submits": db.execute("SELECT COUNT(*) FROM live_submits").fetchone()[0],
        "shadow_entries": db.execute("SELECT COUNT(*) FROM shadow_entries").fetchone()[0],
        "resolutions": db.execute("SELECT COUNT(*) FROM resolutions").fetchone()[0],
        "legs": db.execute("SELECT COUNT(*) FROM legs").fetchone()[0],
    }
    print(
        f"ingest: +{shadow_n} shadow rows, +{live_n} submits, rebuilt {legs_n} legs -> {db_path}"
    )
    print(
        f"totals: submits={counts['live_submits']} entries={counts['shadow_entries']} "
        f"resolutions={counts['resolutions']} legs={counts['legs']}"
    )
    return 0


def fmt_day(r: DayRollup) -> str:
    hit = 100.0 * r.wins / r.n_resolved if r.n_resolved else 0.0
    upt = r.net_usd / r.n_resolved if r.n_resolved else 0.0
    return (
        f"{r.date}  submits={r.n_submitted} resolved={r.n_resolved} open={r.n_open} "
        f"hit={hit:.1f}% ({r.wins}W)  "
        f"GROSS=${r.gross_usd:+,.0f} fee=${r.fee_usd:,.0f} NET=${r.net_usd:+,.0f} "
        f"${upt:+.2f}/tr"
    )


def cmd_daily_report(args: argparse.Namespace) -> int:
    db = sqlite3.connect(args.db)
    rollups = rollup_by_submit_date(db)
    if not rollups:
        print("(no legs: run ingest first)")
        return 0
    print("LIVE trade ledger: daily rollup (by submit UTC date)")
    print("-" * 72)
    for r in rollups:
        print(fmt_day(r))
    resolved = sum(x.n_resolved for x in rollups)
    net = sum(x.net_usd for x in rollups)
    print("-" * 72)
    print(
        f"ALL  resolved={resolved} NET=${net:+,.0f} "
        f"${net / resolved if resolved else 0:+.2f}/tr"
    )
    return 0


def cmd_query(args: argparse.Namespace) -> int:
    db = sqlite3.connect(args.db)
    rows = db.execute(
        "SELECT submit_ts_utc, slug, side, clip, touch_price, status, won, "
        "gross_pnl, fee, net_pnl FROM legs WHERE submit_ts_utc LIKE ? "
        "ORDER BY submit_ts_utc",
        (f"{args.date}%",),
    ).fetchall()
    if not rows:
        print(f"(no legs for {args.date})")
        return 0
    print(f"LIVE legs for {args.date} (UTC submit date)")
    print("-" * 72)
    for submit_ts, slug, side, clip, touch, status, won, gross, fee, net in rows:
        tail = slug[-24:] if slug else "?"
        if status == "resolved":
            print(
                f"{submit_ts[11:19]} {side or '?':>4} {tail} clip={clip} "
                f"touch={touch:.3f} {'W' if won else 'L'} "
                f"gross=${gross:+.2f} fee=${fee:.2f} net=${net:+.2f}"
            )
        else:
            print(
                f"{submit_ts[11:19]} {side or '?':>4} {tail} clip={clip} "
                f"status={status}"
            )
    rollups = [r for r in rollup_by_submit_date(db) if r.date == args.date]
    if rollups:
        print("-" * 72)
        print(fmt_day(rollups[0]))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", default=str(default_db()))
    ap.add_argument("--shadow-dir", default=str(default_shadow_dir()))
    ap.add_argument("--live-log", default=str(default_live_log()))
    ap.add_argument("--clip-usd", type=float, default=50.0)
    ap.add_argument("--fee-curve-rate", type=float, default=0.07)
    sub = ap.add_subparsers(dest="cmd", required=True)

    p_ingest = sub.add_parser("ingest", help="Ingest JSONL + live log and rebuild legs")
    p_ingest.set_defaults(func=cmd_ingest)

    p_daily = sub.add_parser("daily-report", help="Per-day rollup of matched legs")
    p_daily.set_defaults(func=cmd_daily_report)

    p_query = sub.add_parser("query", help="Legs for one UTC date (YYYY-MM-DD)")
    p_query.add_argument("--date", required=True)
    p_query.set_defaults(func=cmd_query)

    args = ap.parse_args()
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())