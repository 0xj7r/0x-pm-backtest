#!/usr/bin/env python3
"""Ingest pm-alpha shadow JSONL logs into SQLite (idempotent, offset-tracked)
and keep simple rollups queryable. Runs on the deployment box via timer.

Usage: python3 shadow_ingest.py <log_dir> <db_path>
"""
import glob
import json
import sqlite3
import sys

DDL = """
CREATE TABLE IF NOT EXISTS ingest_state (file TEXT PRIMARY KEY, lines INTEGER);
CREATE TABLE IF NOT EXISTS entries (
  ts_utc TEXT, slug TEXT, side TEXT, p_exo REAL, touch_price REAL,
  touch_size REAL, edge REAL, strike REAL, strike_source TEXT,
  PRIMARY KEY (slug, side, ts_utc)
);
CREATE TABLE IF NOT EXISTS probes (
  ts_utc TEXT, slug TEXT, side TEXT, entry_touch_price REAL,
  still_quoted INTEGER, current_touch_price REAL, current_touch_size REAL,
  remaining_size REAL, PRIMARY KEY (slug, side, ts_utc)
);
CREATE TABLE IF NOT EXISTS exits (
  ts_utc TEXT, slug TEXT, side TEXT, exit_touch_price REAL,
  mark_pnl_per_share REAL, PRIMARY KEY (slug, side, ts_utc)
);
CREATE TABLE IF NOT EXISTS summaries (
  ts_utc TEXT PRIMARY KEY, n_active_markets INTEGER, n_entries_total INTEGER,
  probe_still_quoted_rate REAL, mean_mark_pnl_per_share REAL,
  binance_feed_age_ms INTEGER, book_feed_age_ms INTEGER,
  median_binance_receipt_minus_exchange_ms REAL,
  median_book_receipt_minus_exchange_ms REAL
);
"""


def f(r, k):
    v = r.get(k)
    return v if not isinstance(v, dict) else None


def main():
    log_dir, db_path = sys.argv[1], sys.argv[2]
    db = sqlite3.connect(db_path)
    db.executescript(DDL)
    for path in sorted(glob.glob(f"{log_dir}/*.jsonl")):
        row = db.execute("SELECT lines FROM ingest_state WHERE file=?", (path,)).fetchone()
        start = row[0] if row else 0
        n = start
        with open(path) as fh:
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
                    db.execute(
                        "INSERT OR IGNORE INTO entries VALUES (?,?,?,?,?,?,?,?,?)",
                        (r.get("ts_utc"), r.get("slug"), r.get("side"), f(r, "p_exo"),
                         (r.get("touch") or {}).get("price") if isinstance(r.get("touch"), dict) else f(r, "touch_price"),
                         (r.get("touch") or {}).get("size") if isinstance(r.get("touch"), dict) else f(r, "touch_size"),
                         f(r, "edge"), f(r, "strike"), r.get("strike_source")))
                elif t == "quote_probe":
                    ct = r.get("current_touch") or {}
                    db.execute(
                        "INSERT OR IGNORE INTO probes VALUES (?,?,?,?,?,?,?,?)",
                        (r.get("ts_utc"), r.get("slug"), r.get("side"), f(r, "entry_touch_price"),
                         1 if r.get("still_quoted") else 0,
                         ct.get("price") if isinstance(ct, dict) else None,
                         ct.get("size") if isinstance(ct, dict) else None,
                         f(r, "remaining_size")))
                elif t == "would_exit":
                    db.execute(
                        "INSERT OR IGNORE INTO exits VALUES (?,?,?,?,?)",
                        (r.get("ts_utc"), r.get("slug"), r.get("side"),
                         f(r, "exit_touch_price") or ((r.get("exit_touch") or {}).get("price") if isinstance(r.get("exit_touch"), dict) else None),
                         f(r, "mark_pnl_per_share")))
                elif t == "summary":
                    db.execute(
                        "INSERT OR IGNORE INTO summaries VALUES (?,?,?,?,?,?,?,?,?)",
                        (r.get("ts_utc"), f(r, "n_active_markets"), f(r, "n_entries_total"),
                         f(r, "probe_still_quoted_rate"), f(r, "mean_mark_pnl_per_share"),
                         f(r, "binance_feed_age_ms"), f(r, "book_feed_age_ms"),
                         f(r, "median_binance_receipt_minus_exchange_ms"),
                         f(r, "median_book_receipt_minus_exchange_ms")))
        db.execute(
            "INSERT INTO ingest_state VALUES (?,?) ON CONFLICT(file) DO UPDATE SET lines=excluded.lines",
            (path, n))
        db.commit()
    e, p, q, x = [db.execute(s).fetchone()[0] for s in (
        "SELECT COUNT(*) FROM entries", "SELECT COUNT(*) FROM probes",
        "SELECT COALESCE(AVG(still_quoted),0) FROM probes", "SELECT COUNT(*) FROM exits")]
    print(f"db: {e} entries, {p} probes (still-quoted {q:.0%}), {x} exits")


if __name__ == "__main__":
    main()
