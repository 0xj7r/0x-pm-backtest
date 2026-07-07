#!/usr/bin/env python3
"""Consensus sidecar: emit would_enter lines only when BOTH decision twins agree.

Tails the newest shadow-*.jsonl in two twin directories (identical config,
independent feeds). When both emit a first-clip would_enter for the same
market with the SAME side within --window-s, the A-twin line is appended to
the consensus output file (verbatim, so shadow_exec_tail can consume it
unchanged). Side disagreements and singletons are logged and dropped: a feed
artifact on one connection cannot create a trade.

Non-entry event types are NOT forwarded; the executor only acts on
would_enter, and redemption metadata rides on that line.

Usage (Dublin):
  python3 scripts/ops/consensus_tail.py \
    --a-dir ~/data/pm-alpha/shadow-final \
    --b-dir ~/data/pm-alpha/shadow-final-b \
    --out-dir ~/data/pm-alpha/shadow-consensus
Then point shadow_exec_tail's PM_SHADOW_JSONL_PATH at the out dir.

NOT deployed mid-soak: the July soak measures the ungated single-stream
baseline. Deploy decision comes with the gate verdict (docs/
stability-gate-preregistration-2026-07.md).
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import time
from pathlib import Path


def newest(dir_: Path) -> Path | None:
    files = sorted(glob.glob(str(dir_ / "shadow-*.jsonl")), key=os.path.getmtime)
    return Path(files[-1]) if files else None


class Tail:
    """Follow the newest shadow JSONL in a directory, surviving rotations."""

    def __init__(self, dir_: Path, tail_from_end: bool = True):
        self.dir = dir_
        self.path: Path | None = None
        self.pos = 0
        # Skip pre-existing backlog on startup so the two twins are matched
        # from the same instant forward; without this, mismatched file history
        # depth (one twin restarted more recently) manufactures phantom
        # "expired" singletons at boot. Genuine rotations still read from 0.
        self._skip_backlog = tail_from_end

    def poll(self) -> list[dict]:
        latest = newest(self.dir)
        if latest is None:
            return []
        if latest != self.path:
            self.path = latest
            if self._skip_backlog:
                self.pos = latest.stat().st_size
                self._skip_backlog = False
            else:
                self.pos = 0
        out = []
        with open(self.path, "rb") as f:
            f.seek(self.pos)
            for bline in f:
                if not bline.endswith(b"\n"):
                    break  # partial write; re-read next poll
                self.pos += len(bline)
                if b"would_enter" not in bline:
                    continue
                try:
                    ev = json.loads(bline)
                except json.JSONDecodeError:
                    continue
                if ev.get("type") != "would_enter":
                    continue
                ev["_raw"] = bline.decode(errors="replace")
                out.append(ev)
        return out


def key(ev: dict) -> tuple[str, int]:
    return (ev["slug"], int(ev.get("clip", 1)))


def run(a_dir: Path, b_dir: Path, out_dir: Path, window_s: float, poll_s: float) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / "shadow-consensus.jsonl"
    stats_path = out_dir / "consensus_stats.jsonl"
    ta, tb = Tail(a_dir), Tail(b_dir)
    pend_a: dict[tuple, dict] = {}
    pend_b: dict[tuple, dict] = {}
    agreed = disagreed = expired = 0
    last_stats = time.time()

    def log_stats(force: bool = False):
        nonlocal last_stats
        now = time.time()
        if force or now - last_stats >= 300:
            with open(stats_path, "a") as f:
                f.write(json.dumps({
                    "ts": int(now), "agreed": agreed,
                    "disagreed": disagreed, "expired": expired,
                }) + "\n")
            last_stats = now

    while True:
        now = time.time()
        for ev in ta.poll():
            pend_a.setdefault(key(ev), {**ev, "_seen": now})
        for ev in tb.poll():
            pend_b.setdefault(key(ev), {**ev, "_seen": now})

        for k in sorted(set(pend_a) & set(pend_b)):
            a, b = pend_a.pop(k), pend_b.pop(k)
            if a["side"] == b["side"]:
                agreed += 1
                with open(out_path, "a") as f:
                    f.write(a["_raw"])
            else:
                disagreed += 1
                print(f"DISAGREE {k[0]} clip{k[1]}: A={a['side']} B={b['side']}", flush=True)

        for pend in (pend_a, pend_b):
            for k in [k for k, v in pend.items() if now - v["_seen"] > window_s]:
                pend.pop(k)
                expired += 1
        log_stats()
        time.sleep(poll_s)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--a-dir", default="/home/ubuntu/data/pm-alpha/shadow-final")
    ap.add_argument("--b-dir", default="/home/ubuntu/data/pm-alpha/shadow-final-b")
    ap.add_argument("--out-dir", default="/home/ubuntu/data/pm-alpha/shadow-consensus")
    ap.add_argument("--window-s", type=float, default=45.0,
                    help="max seconds to wait for the twin's matching entry")
    ap.add_argument("--poll-s", type=float, default=0.5)
    args = ap.parse_args()
    run(Path(args.a_dir).expanduser(), Path(args.b_dir).expanduser(),
        Path(args.out_dir).expanduser(), args.window_s, args.poll_s)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
