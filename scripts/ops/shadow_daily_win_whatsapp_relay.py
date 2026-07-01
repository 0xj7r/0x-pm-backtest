#!/usr/bin/env python3
"""Mac-side relay: fetch green-day summary from Dublin, send via local WhatsApp bridge.

Dublin cron cannot reach localhost:8080 on this Mac. This script SSHes to the
shadow box, runs shadow_daily_win_summary.py --json --notify none, and posts
the message through the same bridge stock-agent uses.

Usage:
  python3 scripts/ops/shadow_daily_win_whatsapp_relay.py
  python3 scripts/ops/shadow_daily_win_whatsapp_relay.py --day 2026-06-16 --dry-run
"""
from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from whatsapp_notify import load_whatsapp_env, wa_send  # noqa: E402

SSH_KEY = os.environ.get("SHADOW_SSH_KEY", os.path.expanduser("~/.ssh/whale_pair_dublin_ed25519.pem"))
SSH_HOST = os.environ.get("SHADOW_SSH_HOST", "")
if not SSH_HOST:
    raise SystemExit("SHADOW_SSH_HOST is not set; set SHADOW_SSH_HOST=ubuntu@<current-ip>")
REMOTE_SCRIPT = os.environ.get("SHADOW_DAILY_SCRIPT", "~/scripts/shadow_daily_win_summary.py")


def fetch_summary(day: str | None, tz: str) -> dict:
    remote = REMOTE_SCRIPT
    cmd = f"python3 {remote} --json --notify none"
    if day:
        cmd += f" --day {day}"
    cmd += f" --tz {tz}"
    ssh = [
        "ssh",
        "-i",
        SSH_KEY,
        "-o",
        "ConnectTimeout=15",
        SSH_HOST,
        cmd,
    ]
    r = subprocess.run(ssh, capture_output=True, text=True, timeout=120)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip() or r.stdout.strip() or "ssh failed")
    line = r.stdout.strip().splitlines()[-1]
    return json.loads(line)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--day", help="YYYY-MM-DD (default: yesterday on remote)")
    ap.add_argument("--tz", default="Europe/Dublin")
    ap.add_argument("--whatsapp-env", default=os.path.expanduser("~/.config/polymarket-watchdog/whatsapp.env"))
    ap.add_argument("--dry-run", action="store_true")
    args = ap.parse_args()

    load_whatsapp_env(Path(args.whatsapp_env))

    try:
        payload = fetch_summary(args.day, args.tz)
    except (RuntimeError, json.JSONDecodeError, subprocess.TimeoutExpired) as e:
        print(f"relay fetch failed: {e}", file=sys.stderr)
        return 1

    if not payload.get("send"):
        print(payload.get("reason", "no message"))
        return 0

    msg = payload["message"]
    print(msg)
    if args.dry_run:
        print("(dry-run — not sent)")
        return 0

    if wa_send(msg):
        print("WhatsApp sent")
        return 0
    print("WhatsApp send failed (is bridge running?)", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())