#!/usr/bin/env python3
"""Overnight LIVE monitor: positions, wins, P&L; scale to $50 on night win streak.

Runs on Dublin via cron. Only restarts shadow_exec_tail (not shadow-final).

Scale-up when ALL true:
  - UTC hour in [0, 11] (historical strong window)
  - >= MIN_STREAK consecutive resolved LIVE wins (chronological)
  - night resolved gross >= MIN_NIGHT_GROSS_USD @ current clip
  - wallet >= MIN_WALLET_USD
  - clip still at BASE_CLIP_USD (not already scaled)

Usage:
  python3 scripts/ops/shadow_night_monitor.py
  python3 scripts/ops/shadow_night_monitor.py --dry-run
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

BASE_CLIP = float(os.environ.get("NIGHT_BASE_CLIP_USD", "25"))
TARGET_CLIP = float(os.environ.get("NIGHT_TARGET_CLIP_USD", "50"))
MIN_STREAK = int(os.environ.get("NIGHT_MIN_WIN_STREAK", "4"))
MIN_NIGHT_GROSS = float(os.environ.get("NIGHT_MIN_GROSS_USD", "40"))
MIN_WALLET = float(os.environ.get("NIGHT_MIN_WALLET_USD", "1350"))
NIGHT_HOURS = tuple(int(x) for x in os.environ.get("NIGHT_UTC_HOURS", "0-11").split("-"))

LIVE_ENTER_RE = re.compile(
    r"LIVE ENTER (UP|DOWN) (\S+).*touch=([\d.]+).*clip=(\d+)"
)
TS_RE = re.compile(r"(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
REDEEM_RE = re.compile(r"redeem OK slug=(\S+)")


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def in_night_window(now: datetime) -> bool:
    lo, hi = NIGHT_HOURS[0], NIGHT_HOURS[-1] if len(NIGHT_HOURS) > 1 else NIGHT_HOURS[0]
    return lo <= now.hour <= hi


def load_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def build_res_index(shadow_dir: Path) -> dict[tuple[str, str, int], dict]:
    import glob

    entries, resolutions = [], []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for ev in load_jsonl(Path(fp)):
            if ev.get("type") == "would_enter":
                entries.append(ev)
            elif ev.get("type") == "resolution":
                resolutions.append(ev)
    by_ent: dict[tuple[str, str], list] = defaultdict(list)
    for e in entries:
        by_ent[(e["slug"], e["side"])].append(e)
    for ents in by_ent.values():
        ents.sort(key=lambda x: x["ts_utc"])
    by_res: dict[tuple[str, str], list] = defaultdict(list)
    for r in resolutions:
        by_res[(r["slug"], r["side"])].append(r)
    for ress in by_res.values():
        ress.sort(key=lambda x: x["ts_utc"])
    out: dict[tuple[str, str, int], dict] = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = res
    return out


def live_legs(live_log: Path, session_date: str) -> list[dict]:
    legs: list[dict] = []
    seen: set[tuple[str, int]] = set()
    with live_log.open(encoding="utf-8") as f:
        for line in f:
            ts_m = TS_RE.search(line)
            if not ts_m or session_date not in ts_m.group(1):
                continue
            if "LIVE ENTER" not in line:
                continue
            m = LIVE_ENTER_RE.search(line)
            if not m:
                continue
            slug = m.group(2)
            clip = int(m.group(4))
            key = (slug, clip)
            if key in seen:
                continue
            seen.add(key)
            legs.append(
                {
                    "ts": ts_m.group(1),
                    "side": m.group(1).lower(),
                    "slug": slug,
                    "touch": float(m.group(3)),
                    "clip": clip,
                }
            )
    return sorted(legs, key=lambda x: x["ts"])


def current_clip(env_path: Path) -> float:
    if not env_path.exists():
        return BASE_CLIP
    for line in env_path.read_text().splitlines():
        if line.startswith("PM_SHADOW_CLIP_USD="):
            return float(line.split("=", 1)[1].strip())
    return BASE_CLIP


def wallet_cash(balance_bin: Path, wallet_env: Path) -> float | None:
    if not balance_bin.is_file():
        return None
    env = os.environ.copy()
    if wallet_env.exists():
        for line in wallet_env.read_text().splitlines():
            line = line.strip()
            if line.startswith("export "):
                line = line[7:]
            if "=" in line and not line.startswith("#"):
                k, v = line.split("=", 1)
                env[k] = v.strip('"').strip("'")
    try:
        out = subprocess.check_output([str(balance_bin)], env=env, text=True, timeout=30)
        return float(out.strip().splitlines()[-1])
    except Exception:
        return None


def pnl_leg(leg: dict, res: dict, clip: float) -> tuple[float, bool]:
    touch = leg["touch"]
    won = bool(res.get("won"))
    gross = clip / touch - clip if won else -clip
    return gross, won


def consecutive_wins(resolved: list[tuple[dict, dict, float, bool]]) -> int:
    streak = 0
    for *_rest, won in reversed(resolved):
        if won:
            streak += 1
        else:
            break
    return streak


def consecutive_losses(resolved: list[tuple[dict, dict, float, bool]]) -> int:
    streak = 0
    for *_rest, won in reversed(resolved):
        if not won:
            streak += 1
        else:
            break
    return streak


def scale_state_path(data_dir: Path) -> Path:
    return data_dir / "night_scale_state.json"


def already_scaled_today(state_path: Path, today: str) -> bool:
    if not state_path.exists():
        return False
    raw = json.loads(state_path.read_text())
    return raw.get("scaled_date") == today and raw.get("clip_usd") == TARGET_CLIP


def restart_exec(
    restart_sh: Path,
    *,
    dry_run: bool,
) -> None:
    if dry_run:
        print(f"DRY-RUN: would run {restart_sh}")
        return
    subprocess.check_call([str(restart_sh)])


def update_env_clip(env_path: Path, clip: float, *, dry_run: bool) -> None:
    text = env_path.read_text()
    repl = {
        "PM_SHADOW_CLIP_USD": clip,
        "PM_SHADOW_MAX_ORDER_NOTIONAL_USD": clip,
        "PM_SHADOW_MAX_MARKET_NOTIONAL_USD": clip * 2,
    }
    lines = []
    for line in text.splitlines():
        key = line.split("=", 1)[0] if "=" in line else None
        if key in repl:
            lines.append(f"{key}={repl[key]:g}")
        else:
            lines.append(line)
    new_text = "\n".join(lines) + "\n"
    if dry_run:
        print(f"DRY-RUN env -> clip ${clip:g}")
        return
    env_path.write_text(new_text)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--data-dir", default="/home/ubuntu/data/pm-alpha")
    ap.add_argument("--shadow-dir", default="/home/ubuntu/data/pm-alpha/shadow-final")
    ap.add_argument("--live-log", default="/home/ubuntu/data/pm-alpha/shadow_exec_tail.log")
    ap.add_argument(
        "--shadow-env",
        default="/home/ubuntu/.config/polymarket-exec/shadow_exec_tail.env",
    )
    ap.add_argument(
        "--wallet-env",
        default="/home/ubuntu/.config/polymarket-exec/wallet.env",
    )
    ap.add_argument(
        "--balance-bin",
        default="/home/ubuntu/deploy-main/polymarket-agent/target/release/balance_once",
    )
    ap.add_argument(
        "--restart-sh",
        default="/home/ubuntu/.local/bin/shadow-restart-exec.sh",
    )
    args = ap.parse_args()

    now = utc_now()
    today = now.strftime("%Y-%m-%d")
    data_dir = Path(args.data_dir)
    log_path = data_dir / "night_monitor.log"

    clip = current_clip(Path(args.shadow_env))
    cash = wallet_cash(Path(args.balance_bin), Path(args.wallet_env))
    res_idx = build_res_index(Path(args.shadow_dir))
    legs = live_legs(Path(args.live_log), today)

    night_legs = [l for l in legs if 0 <= int(l["ts"][11:13]) <= 11]
    resolved: list[tuple[dict, dict, float, bool]] = []
    open_legs: list[dict] = []
    night_gross = 0.0
    wins = losses = 0

    for leg in legs:
        key = (leg["slug"], leg["side"], leg["clip"])
        res = res_idx.get(key)
        if not res:
            open_legs.append(leg)
            continue
        g, won = pnl_leg(leg, res, clip)
        resolved.append((leg, res, g, won))
        if won:
            wins += 1
        else:
            losses += 1
        if 0 <= int(leg["ts"][11:13]) <= 11:
            night_gross += g

    streak = consecutive_wins(resolved)
    loss_streak = consecutive_losses(resolved)
    night_resolved = [x for x in resolved if 0 <= int(x[0]["ts"][11:13]) <= 11]
    night_w = sum(1 for *_, w in night_resolved if w)
    night_l = len(night_resolved) - night_w

    lines = [
        f"[{now.strftime('%Y-%m-%d %H:%M:%S')} UTC] clip=${clip:g} wallet=${cash if cash else '?'}"
        f" night_window={in_night_window(now)}",
        f"  today legs={len(legs)} resolved={wins+losses} ({wins}W/{losses}L)"
        f" open={len(open_legs)} win_streak={streak} loss_streak={loss_streak}",
        f"  night 00-11: resolved={len(night_resolved)} ({night_w}W/{night_l}L)"
        f" gross=${night_gross:+.2f}",
    ]
    if open_legs:
        lines.append(f"  open: {open_legs[-1]['slug'][-10:]} {open_legs[-1]['side']} c{open_legs[-1]['clip']}")

    scale_reason = None
    state_path = scale_state_path(data_dir)
    if clip >= TARGET_CLIP or already_scaled_today(state_path, today):
        lines.append(f"  scale: already at ${clip:g} (no action)")
    elif not in_night_window(now):
        lines.append("  scale: waiting for night window 00-11 UTC")
    elif cash is not None and cash < MIN_WALLET:
        lines.append(f"  scale: wallet ${cash:.0f} < floor ${MIN_WALLET:.0f}")
    elif loss_streak >= 2:
        lines.append(f"  scale: loss2 active (loss_streak={loss_streak})")
    elif streak < MIN_STREAK:
        lines.append(f"  scale: win_streak {streak} < {MIN_STREAK}")
    elif night_gross < MIN_NIGHT_GROSS:
        lines.append(f"  scale: night gross ${night_gross:.2f} < ${MIN_NIGHT_GROSS:.0f}")
    else:
        scale_reason = (
            f"streak={streak} night_gross=${night_gross:+.2f} wallet=${cash:.0f}"
        )
        lines.append(f"  scale: TRIGGER -> ${TARGET_CLIP:g} ({scale_reason})")
        update_env_clip(Path(args.shadow_env), TARGET_CLIP, dry_run=args.dry_run)
        restart_exec(Path(args.restart_sh), dry_run=args.dry_run)
        if not args.dry_run:
            state_path.write_text(
                json.dumps(
                    {
                        "scaled_date": today,
                        "scaled_at_utc": now.isoformat(),
                        "clip_usd": TARGET_CLIP,
                        "reason": scale_reason,
                    }
                )
            )

    msg = "\n".join(lines)
    print(msg)
    with log_path.open("a", encoding="utf-8") as f:
        f.write(msg + "\n")

    # optional telegram
    tg_env = Path.home() / ".config/polymarket-watchdog/telegram.env"
    if scale_reason and tg_env.exists() and not args.dry_run:
        try:
            for line in tg_env.read_text().splitlines():
                if "=" in line and not line.startswith("#"):
                    k, v = line.split("=", 1)
                    os.environ[k.strip()] = v.strip().strip('"')
            token = os.environ.get("TG_TOKEN")
            chat = os.environ.get("TG_CHAT_ID")
            if token and chat:
                import urllib.parse
                import urllib.request

                text = f"shadow night SCALE $25->$50\n{scale_reason}"
                url = (
                    f"https://api.telegram.org/bot{token}/sendMessage?"
                    + urllib.parse.urlencode({"chat_id": chat, "text": text})
                )
                urllib.request.urlopen(url, timeout=10)
        except Exception:
            pass

    return 0


if __name__ == "__main__":
    raise SystemExit(main())