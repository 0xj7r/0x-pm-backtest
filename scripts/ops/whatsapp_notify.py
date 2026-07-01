#!/usr/bin/env python3
"""Send a message via the local whatsapp-bridge (same API as stock-agent).

Requires bridge running on this machine (LaunchAgent com.whatsapp.bridge).
Env file: ~/.config/polymarket-watchdog/whatsapp.env

  export WHATSAPP_BRIDGE_URL=http://127.0.0.1:8080
  export WHATSAPP_BRIDGE_API_KEY=...
  export WHATSAPP_RECIPIENT=447703833707@s.whatsapp.net
"""
from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request
from pathlib import Path


def load_whatsapp_env(path: Path | None = None) -> None:
    env_path = path or Path.home() / ".config/polymarket-watchdog/whatsapp.env"
    if not env_path.is_file():
        return
    for line in env_path.read_text().splitlines():
        if line.startswith("export "):
            line = line[7:]
        if "=" in line and not line.strip().startswith("#"):
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def wa_send(message: str) -> bool:
    url = os.environ.get("WHATSAPP_BRIDGE_URL", "http://127.0.0.1:8080").rstrip("/")
    key = os.environ.get("WHATSAPP_BRIDGE_API_KEY", "")
    recipient = os.environ.get("WHATSAPP_RECIPIENT", "")
    if not key or not recipient:
        return False
    body = json.dumps({"recipient": recipient, "message": message}).encode()
    req = urllib.request.Request(
        f"{url}/api/send",
        data=body,
        headers={"Content-Type": "application/json", "X-API-Key": key},
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as resp:
            return 200 <= resp.status < 300
    except (urllib.error.URLError, TimeoutError):
        return False


def main() -> int:
    load_whatsapp_env()
    if len(sys.argv) < 2:
        print("usage: whatsapp_notify.py <message>", file=sys.stderr)
        return 2
    msg = " ".join(sys.argv[1:])
    if wa_send(msg):
        print("sent")
        return 0
    print("send failed (bridge down or env missing)", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())