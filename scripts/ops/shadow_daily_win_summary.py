#!/usr/bin/env python3
"""Morning summary for yesterday; notify ONLY if LIVE resolved P&L > 0.

Designed for hands-off week: celebrate green days at ~08:00 local, silence on red.

The LIVE figure is LEDGER-ESTIMATED (rebuilt from executor logs + shadow
telemetry). It uses actual venue fills (avg_fill_price/filled_qty) when the
executor logged them, else falls back to intended clip notional at the
reference touch. On-chain cash flow is ground truth; pass --reconcile to
compare against scripts/ops/onchain_reconcile.py for the same day.

Usage (cron, Europe/Dublin 08:00):
  python3 scripts/ops/shadow_daily_win_summary.py
  python3 scripts/ops/shadow_daily_win_summary.py --day 2026-06-16 --dry-run
  python3 scripts/ops/shadow_daily_win_summary.py --json --notify none   # Mac relay input
"""
from __future__ import annotations

import argparse
import glob
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
TS_RE = re.compile(r"(?P<ts>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})")
SUBMITTED_RE = re.compile(
    r"shadow SUBMITTED.*slug=(?P<slug>\S+).*accepted=(?P<acc>true|false)"
    r"(?:.*avg_fill_price=(?P<fill>[\d.]+))?"
    r"(?:.*filled_qty=(?P<qty>[\d.]+))?"
)
REDEEM_RE = re.compile(r"redeem OK slug=(?P<slug>\S+)")


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def load_telegram_env(path: Path) -> None:
    if not path.is_file():
        return
    for line in path.read_text().splitlines():
        if line.startswith("export "):
            line = line[7:]
        if "=" in line and not line.strip().startswith("#"):
            k, v = line.split("=", 1)
            os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))


def tg_send(msg: str) -> bool:
    token = os.environ.get("TG_TOKEN", "")
    chat = os.environ.get("TG_CHAT_ID", "")
    if not token or not chat:
        return False
    r = subprocess.run(
        [
            "curl",
            "-s",
            f"https://api.telegram.org/bot{token}/sendMessage",
            "-d",
            f"chat_id={chat}",
            "--data-urlencode",
            f"text={msg}",
        ],
        capture_output=True,
        text=True,
        timeout=30,
    )
    return r.returncode == 0


def run_reconcile(day: str, ledger_pnl: float) -> None:
    """Compare the ledger-estimated day P&L against on-chain ground truth."""
    script = Path(__file__).resolve().parent / "onchain_reconcile.py"
    print(f"\n[reconcile] on-chain vs ledger-estimated for {day} (UTC day basis)")
    r = subprocess.run(
        [
            sys.executable,
            str(script),
            "--day",
            day,
            "--ledger-pnl",
            f"{ledger_pnl:.2f}",
        ],
        capture_output=True,
        text=True,
        timeout=300,
    )
    sys.stdout.write(r.stdout)
    if r.returncode != 0:
        sys.stderr.write(r.stderr)
        print(f"[reconcile] failed (rc={r.returncode})", file=sys.stderr)


def current_clip(env_path: Path, default: float = 50.0) -> float:
    if not env_path.is_file():
        return default
    for line in env_path.read_text().splitlines():
        if line.startswith("PM_SHADOW_CLIP_USD="):
            return float(line.split("=", 1)[1].strip())
    return default


def wallet_cash(balance_bin: Path, wallet_env: Path) -> float | None:
    if not balance_bin.is_file() or not wallet_env.is_file():
        return None
    cmd = f"set -a && source '{wallet_env}' && set +a && '{balance_bin}'"
    r = subprocess.run(["bash", "-lc", cmd], capture_output=True, text=True)
    try:
        return float((r.stdout or "").strip())
    except ValueError:
        return None


def day_window(day: str, tz: ZoneInfo) -> tuple[datetime, datetime]:
    """UTC [start, end) for calendar `day` in timezone `tz`."""
    local_start = datetime.strptime(day, "%Y-%m-%d").replace(tzinfo=tz)
    local_end = local_start + timedelta(days=1)
    return local_start.astimezone(timezone.utc), local_end.astimezone(timezone.utc)


def load_shadow_day(
    shadow_dir: Path, utc_start: datetime, utc_end: datetime
) -> tuple[list[dict], list[dict]]:
    entries: list[dict] = []
    resolutions: list[dict] = []
    files = sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl")))
    for fp in files:
        with open(fp, encoding="utf-8") as f:
            for line in f:
                try:
                    e = json.loads(line)
                except json.JSONDecodeError:
                    continue
                ts = e.get("ts_utc", "")
                if not ts:
                    continue
                t = parse_ts(ts)
                if not (utc_start <= t < utc_end):
                    continue
                if e.get("type") == "would_enter":
                    entries.append(e)
                elif e.get("type") == "resolution":
                    resolutions.append(e)
    return entries, resolutions


def index_resolutions(
    all_entries: list[dict], all_resolutions: list[dict]
) -> dict[tuple[str, str, int], dict]:
    by_ent: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for e in all_entries:
        by_ent[(e["slug"], e["side"])].append(e)
    for ents in by_ent.values():
        ents.sort(key=lambda x: x["ts_utc"])

    by_res: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for r in all_resolutions:
        by_res[(r["slug"], r["side"])].append(r)
    for ress in by_res.values():
        ress.sort(key=lambda x: x["ts_utc"])

    out: dict[tuple[str, str, int], dict] = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = res
    return out


def load_submitted_day(
    live_log: Path, utc_start: datetime, utc_end: datetime
) -> tuple[list[dict], set[str]]:
    """SUBMITTED accepted=true fills in [utc_start, utc_end).

    Redeems are collected over the ENTIRE log, not just the day window:
    a leg entered before midnight often redeems after it, and restricting
    redeems to the window left such legs permanently "open" (P&L 0).
    """
    fills: list[dict] = []
    redeemed: set[str] = set()
    if not live_log.is_file():
        return fills, redeemed
    for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
        line = strip_ansi(raw)
        rm = REDEEM_RE.search(line)
        if rm:
            redeemed.add(rm.group("slug"))
        ts_m = TS_RE.search(line)
        if not ts_m:
            continue
        t = parse_ts(ts_m.group("ts") + "Z")
        if not (utc_start <= t < utc_end):
            continue
        sm = SUBMITTED_RE.search(line)
        if sm and sm.group("acc") == "true":
            fills.append(
                {
                    "ts": ts_m.group("ts"),
                    "slug": sm.group("slug"),
                    "fill_price": float(sm.group("fill")) if sm.group("fill") else None,
                    "fill_qty": float(sm.group("qty")) if sm.group("qty") else None,
                }
            )
    return fills, redeemed


def match_fills_to_entries(
    fills: list[dict], entries: list[dict], window_s: float = 120.0
) -> list[dict]:
    by_slug: dict[str, list[dict]] = defaultdict(list)
    for e in entries:
        by_slug[e["slug"]].append(e)
    for ents in by_slug.values():
        ents.sort(key=lambda x: x["ts_utc"])

    legs: list[dict] = []
    seen: set[tuple[str, int]] = set()
    for fill in fills:
        slug = fill["slug"]
        fill_epoch = parse_ts(fill["ts"] + "Z").timestamp()
        best = None
        best_dt = 1e18
        for ent in by_slug.get(slug, []):
            ent_epoch = parse_ts(ent["ts_utc"]).timestamp()
            dt = abs(ent_epoch - fill_epoch)
            if dt <= window_s and dt < best_dt:
                best_dt = dt
                best = ent
        if not best:
            continue
        clip = int(best.get("clip", 1))
        key = (slug, clip)
        if key in seen:
            continue
        seen.add(key)
        legs.append(
            {
                "ts": fill["ts"],
                "slug": slug,
                "side": best["side"],
                "clip": clip,
                "touch": float(best.get("touch_price") or 0),
                "fill_price": fill.get("fill_price"),
                "fill_qty": fill.get("fill_qty"),
            }
        )
    return sorted(legs, key=lambda x: x["ts"])


def leg_pnl(leg: dict, res: dict, clip_usd: float) -> tuple[float, bool, bool]:
    """Settle P&L for one leg. Returns (pnl, won, actual_fill_used).

    Prefers the actual venue fill (avg_fill_price * filled_qty) when the
    executor logged it; falls back to the intended-notional estimate
    (clip_usd at the would_enter reference touch), which overstates
    notional on partial fills and ignores slippage.
    """
    won = bool(res.get("won"))
    price = leg.get("fill_price")
    qty = leg.get("fill_qty")
    if price and qty and price > 0:
        sps = (1.0 - price) if won else (-price)
        return sps * qty, won, True
    touch = leg["touch"]
    if touch <= 0:
        return 0.0, won, False
    sps = (1.0 - touch) if won else (-touch)
    return sps * (clip_usd / touch), won, False


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--day", help="YYYY-MM-DD in --tz (default: yesterday)")
    ap.add_argument("--tz", default="Europe/Dublin")
    ap.add_argument("--shadow-dir", default=os.path.expanduser("~/data/pm-alpha/shadow-final"))
    ap.add_argument("--live-log", default=os.path.expanduser("~/data/pm-alpha/shadow_exec_tail.log"))
    ap.add_argument(
        "--shadow-env",
        default=os.path.expanduser("~/.config/polymarket-exec/shadow_exec_tail.env"),
    )
    ap.add_argument(
        "--wallet-env",
        default=os.path.expanduser("~/.config/polymarket-exec/wallet.env"),
    )
    ap.add_argument(
        "--balance-bin",
        default=os.path.expanduser(
            "~/deploy-main/polymarket-agent/target/release/balance_once"
        ),
    )
    ap.add_argument(
        "--telegram-env",
        default=os.path.expanduser("~/.config/polymarket-watchdog/telegram.env"),
    )
    ap.add_argument("--ref-clip-usd", type=float, default=50.0)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument(
        "--reconcile",
        action="store_true",
        help="compare the ledger-estimated day P&L against on-chain cash flow "
        "(runs onchain_reconcile.py for the same day, UTC basis)",
    )
    ap.add_argument(
        "--notify",
        choices=("telegram", "none"),
        default="telegram",
        help="telegram=send on green day; none=compute+log only (Mac WhatsApp relay)",
    )
    ap.add_argument(
        "--json",
        action="store_true",
        help="emit one JSON line (send, message, day, live_pnl) for Mac relay",
    )
    ap.add_argument(
        "--log-dir",
        default=os.path.expanduser("~/data/pm-alpha/week_monitor"),
    )
    args = ap.parse_args()

    load_telegram_env(Path(args.telegram_env))
    tz = ZoneInfo(args.tz)
    now_local = datetime.now(tz)
    day = args.day or (now_local.date() - timedelta(days=1)).isoformat()
    utc_start, utc_end = day_window(day, tz)
    clip = current_clip(Path(args.shadow_env))

    shadow_dir = Path(args.shadow_dir)
    entries, resolutions = load_shadow_day(shadow_dir, utc_start, utc_end)

    # Full history for resolution index (legs may resolve after midnight)
    all_entries, all_resolutions = [], []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        with open(fp, encoding="utf-8") as f:
            for line in f:
                try:
                    e = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if e.get("type") == "would_enter":
                    all_entries.append(e)
                elif e.get("type") == "resolution":
                    all_resolutions.append(e)
    res_by_clip = index_resolutions(all_entries, all_resolutions)

    ref_pnl = sum(float(r.get("ladder_settle_pnl_usd") or 0) for r in resolutions)
    ref_w = sum(1 for r in resolutions if r.get("won"))
    ref_l = len(resolutions) - ref_w

    fills, redeemed = load_submitted_day(Path(args.live_log), utc_start, utc_end)
    legs = match_fills_to_entries(fills, entries)

    live_resolved: list[dict] = []
    live_open: list[dict] = []
    live_pnl = 0.0
    wins = losses = actual_fills = 0
    for leg in legs:
        key = (leg["slug"], leg["side"], leg["clip"])
        res = res_by_clip.get(key)
        if leg["slug"] in redeemed and res:
            pnl, won, actual = leg_pnl(leg, res, clip)
            live_pnl += pnl
            if actual:
                actual_fills += 1
            if won:
                wins += 1
            else:
                losses += 1
            live_resolved.append({**leg, "pnl": pnl, "won": won, "actual_fill": actual})
        else:
            live_open.append(leg)

    cash = wallet_cash(Path(args.balance_bin), Path(args.wallet_env))
    hit = 100.0 * wins / (wins + losses) if (wins + losses) else 0.0

    log_dir = Path(args.log_dir)
    log_dir.mkdir(parents=True, exist_ok=True)
    log_line = (
        f"{now_local.strftime('%Y-%m-%d %H:%M:%S')} {day} "
        f"live_pnl_est=${live_pnl:+.2f} resolved={wins+losses} "
        f"actual_fills={actual_fills} "
        f"open={len(live_open)} sent={'Y' if live_pnl > 0 else 'N'}\n"
    )
    (log_dir / "daily_win_summary.log").open("a").write(log_line)

    if args.reconcile:
        run_reconcile(day, live_pnl)

    if live_pnl <= 0:
        reason = (
            f"{day}: LIVE P&L (ledger-estimated) ${live_pnl:+.2f} "
            f"not positive, no message"
        )
        if args.json:
            print(
                json.dumps(
                    {
                        "send": False,
                        "day": day,
                        "live_pnl": live_pnl,
                        "basis": "ledger-estimated",
                        "reason": reason,
                    }
                )
            )
        else:
            print(reason)
        return 0

    cash_line = f"Venue cash: ${cash:,.2f}" if cash is not None else "Venue cash: ?"
    msg = (
        f"✅ Shadow fade: green day {day}\n"
        f"\n"
        f"LIVE ledger-estimated (${clip:.0f}/clip)\n"
        f"  NET: ${live_pnl:+,.2f} (est; on-chain is ground truth)\n"
        f"  {wins}W / {losses}L ({hit:.0f}% hit)\n"
        f"  fills: {len(legs)} resolved: {len(live_resolved)} "
        f"open: {len(live_open)} actual-fill priced: {actual_fills}\n"
        f"\n"
        f"REF (shadow-final @ ${args.ref_clip_usd:.0f} telemetry)\n"
        f"  NET: ${ref_pnl:+,.2f}  ({ref_w}W/{ref_l}L)\n"
        f"\n"
        f"{cash_line}"
    )

    if args.json:
        print(
            json.dumps(
                {
                    "send": True,
                    "day": day,
                    "live_pnl": live_pnl,
                    "basis": "ledger-estimated",
                    "message": msg,
                }
            )
        )
        return 0

    print(msg)
    if args.dry_run:
        print("(dry-run, not sent)")
        return 0

    if args.notify == "none":
        print("(notify none, not sent)")
        return 0

    if tg_send(msg):
        print("Telegram sent")
        return 0
    print("Telegram not configured or send failed", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())