#!/usr/bin/env python3
"""Find filled LIVE slugs without matching redeem OK in shadow_exec_tail.log."""
from __future__ import annotations

import argparse
import re
import sys
from collections import defaultdict
from datetime import datetime, timezone

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
# LIVE ENTER is logged before submit; only SUBMITTED rows imply ledger + redeem duty.
SUBMITTED_RE = re.compile(r"shadow SUBMITTED.*slug=(?P<slug>\S+)")
REDEEM_RE = re.compile(r"redeem OK slug=(?P<slug>\S+)")
ISO_RE = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")


def strip_ansi(line: str) -> str:
    return ANSI_RE.sub("", line)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--live-log", required=True)
    ap.add_argument("--since-hours", type=float, default=0)
    args = ap.parse_args()

    cutoff = 0.0
    if args.since_hours > 0:
        cutoff = datetime.now(timezone.utc).timestamp() - args.since_hours * 3600

    fills: dict[str, list[str]] = defaultdict(list)
    redeemed: set[str] = set()

    with open(args.live_log, encoding="utf-8") as f:
        for raw in f:
            line = strip_ansi(raw)
            iso = ISO_RE.search(line)
            epoch = 0.0
            if iso:
                epoch = datetime.fromisoformat(
                    iso.group(1).replace("Z", "+00:00")
                ).timestamp()
            if cutoff and epoch and epoch < cutoff:
                continue
            m = SUBMITTED_RE.search(line)
            if m:
                fills[m.group("slug")].append(line.strip()[:120])
            rm = REDEEM_RE.search(line)
            if rm:
                redeemed.add(rm.group("slug"))

    open_slugs = sorted(set(fills) - redeemed)
    print(f"SUBMITTED slugs:  {len(fills)}")
    print(f"redeemed slugs:   {len(redeemed)}")
    print(f"unredeemed:       {len(open_slugs)}")
    for slug in open_slugs[:40]:
        print(f"  OPEN {slug} ({len(fills[slug])} fill(s))")
    if len(open_slugs) > 40:
        print(f"  ... +{len(open_slugs) - 40} more")
    return 1 if open_slugs else 0


if __name__ == "__main__":
    raise SystemExit(main())