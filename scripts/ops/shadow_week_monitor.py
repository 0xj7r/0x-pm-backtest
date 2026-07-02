#!/usr/bin/env python3
"""Hands-off week monitor for gated shadow live stack (observe-only).

Appends JSONL snapshots + human log. Optional Telegram on CRITICAL only.
Does NOT change clip, gates, or restart unless --repair-processes and executor dead.

Wallet floor ($500) is handled separately by poly-shadow-wallet-guard.sh.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def strip_ansi(text: str) -> str:
    return ANSI_RE.sub("", text)


def tg_send(msg: str) -> None:
    token = os.environ.get("TG_TOKEN", "")
    chat = os.environ.get("TG_CHAT_ID", "")
    if not token or not chat:
        return
    try:
        subprocess.run(
            [
                "curl",
                "-s",
                f"https://api.telegram.org/bot{token}/sendMessage",
                "-d",
                f"chat_id={chat}",
                "--data-urlencode",
                f"text={msg}",
            ],
            check=False,
            capture_output=True,
            timeout=30,
        )
    except OSError:
        pass


def pgrep(pattern: str) -> bool:
    r = subprocess.run(["pgrep", "-f", pattern], capture_output=True)
    return r.returncode == 0


def wallet_cash(balance_bin: Path, wallet_env: Path) -> float | None:
    if not balance_bin.is_file() or not wallet_env.is_file():
        return None
    cmd = f"set -a && source '{wallet_env}' && set +a && '{balance_bin}'"
    r = subprocess.run(["bash", "-lc", cmd], capture_output=True, text=True)
    raw = (r.stdout or "").strip()
    try:
        return float(raw)
    except ValueError:
        return None


def redeem_stats(live_log: Path) -> dict:
    submitted_re = re.compile(r"shadow SUBMITTED.*slug=(?P<s>\S+)")
    redeem_re = re.compile(r"redeem OK slug=(?P<s>\S+)")
    fills: set[str] = set()
    redeemed: set[str] = set()
    if live_log.is_file():
        for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
            line = strip_ansi(raw)
            m = submitted_re.search(line)
            if m:
                fills.add(m.group("s"))
            rm = redeem_re.search(line)
            if rm:
                redeemed.add(rm.group("s"))
    open_slugs = sorted(fills - redeemed)
    return {
        "submitted": len(fills),
        "redeemed": len(redeemed),
        "unredeemed": len(open_slugs),
        "open_slugs": open_slugs[:10],
    }


def exec_stats(live_log: Path, since_hours: float) -> dict:
    submitted_re = re.compile(r"shadow SUBMITTED")
    miss_re = re.compile(r"submit miss")
    cutoff = utc_now().timestamp() - since_hours * 3600
    iso_re = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")
    subs = misses = 0
    if live_log.is_file():
        for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
            line = strip_ansi(raw)
            iso = iso_re.search(line)
            if iso:
                ts = datetime.fromisoformat(
                    iso.group(1).replace("Z", "+00:00")
                ).timestamp()
                if ts < cutoff:
                    continue
            if submitted_re.search(line):
                subs += 1
            if miss_re.search(line):
                misses += 1
    return {"submitted": subs, "submit_miss": misses, "since_hours": since_hours}


def parity_submitted(
    shadow_jsonl: Path, live_log: Path, since_hours: float, window_s: float = 3.0
) -> dict:
    """Match shadow would_enter ↔ shadow SUBMITTED (post-fix live fills only)."""
    cutoff = utc_now().timestamp() - since_hours * 3600
    refs: list[dict] = []
    if shadow_jsonl.is_file():
        for line in shadow_jsonl.read_text(encoding="utf-8", errors="replace").splitlines():
            if not line.strip():
                continue
            try:
                e = json.loads(line)
            except json.JSONDecodeError:
                continue
            if e.get("type") != "would_enter":
                continue
            ts = e.get("ts_utc", "")
            epoch = (
                datetime.fromisoformat(ts.replace("Z", "+00:00")).timestamp()
                if ts
                else 0.0
            )
            if epoch < cutoff:
                continue
            refs.append(
                {
                    "slug": e["slug"],
                    "side": e.get("side", ""),
                    "clip": int(e.get("clip", 1)),
                    "epoch": epoch,
                }
            )

    subs: list[dict] = []
    iso_re = re.compile(r"^(\d{4}-\d{2}-\d{2}T[\d:.]+Z)")
    sub_re = re.compile(
        r"shadow SUBMITTED.*slug=(?P<slug>\S+).*accepted=(?P<acc>true|false)"
    )
    enter_re = re.compile(
        r"LIVE ENTER (UP|DOWN) (?P<slug>\S+).*clip=(?P<clip>\d+)"
    )
    if live_log.is_file():
        for raw in live_log.read_text(encoding="utf-8", errors="replace").splitlines():
            line = strip_ansi(raw)
            iso = iso_re.search(line)
            if not iso:
                continue
            epoch = datetime.fromisoformat(
                iso.group(1).replace("Z", "+00:00")
            ).timestamp()
            if epoch < cutoff:
                continue
            m = sub_re.search(line)
            if m and m.group("acc") == "true":
                subs.append({"slug": m.group("slug"), "epoch": epoch})
                continue
            # Fallback: LIVE ENTER after deploy only pairs with prior context: skip

    orphans = 0
    matched_ref: set[int] = set()
    for sub in subs:
        ok = False
        for j, ref in enumerate(refs):
            if j in matched_ref:
                continue
            if sub["slug"] != ref["slug"]:
                continue
            if abs(sub["epoch"] - ref["epoch"]) <= window_s:
                matched_ref.add(j)
                ok = True
                break
        if not ok:
            orphans += 1

    missed = 0
    sub_floor = min((s["epoch"] for s in subs), default=0.0)
    for j, ref in enumerate(refs):
        if j in matched_ref:
            continue
        if sub_floor > 0.0 and ref["epoch"] < sub_floor - window_s:
            continue
        missed += 1

    return {
        "refs": len(refs),
        "submitted": len(subs),
        "orphans": orphans,
        "missed": missed,
        "since_hours": since_hours,
    }


def parity_check(
    compare_script: Path, shadow_jsonl: Path, live_log: Path, since_hours: float
) -> dict:
    if not compare_script.is_file() or not shadow_jsonl.is_file():
        return {"error": "missing inputs", "orphans": -1, "missed": -1}
    cmd = [
        sys.executable,
        str(compare_script),
        "--shadow",
        str(shadow_jsonl),
        "--live-log",
        str(live_log),
        "--since-hours",
        str(since_hours),
    ]
    r = subprocess.run(cmd, capture_output=True, text=True)
    out = strip_ansi(r.stdout + r.stderr)
    orphans = missed = 0
    for line in out.splitlines():
        m = re.search(r"orphans=(\d+).*missed_ref=(\d+)", line)
        if m:
            orphans = int(m.group(1))
            missed = int(m.group(2))
    return {
        "exit_code": r.returncode,
        "orphans": orphans,
        "missed": missed,
        "summary": out.strip().splitlines()[-3:] if out.strip() else [],
    }


def ledger_pending(ledger_path: Path) -> int:
    if not ledger_path.is_file():
        return 0
    try:
        data = json.loads(ledger_path.read_text())
        return len(data) if isinstance(data, list) else 0
    except json.JSONDecodeError:
        return -1


def latest_shadow_jsonl(shadow_dir: Path) -> Path | None:
    files = sorted(shadow_dir.glob("shadow-*.jsonl"), key=lambda p: p.stat().st_mtime)
    return files[-1] if files else None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--data-dir", default=os.path.expanduser("~/data/pm-alpha"))
    ap.add_argument("--shadow-dir", default=os.path.expanduser("~/data/pm-alpha/shadow-final"))
    ap.add_argument("--live-log", default=os.path.expanduser("~/data/pm-alpha/shadow_exec_tail.log"))
    ap.add_argument("--ledger", default=os.path.expanduser("~/data/pm-alpha/shadow_redeem_ledger.json"))
    ap.add_argument("--kill-switch", default=os.path.expanduser("~/fade.kill"))
    ap.add_argument(
        "--compare-script",
        default=os.path.expanduser("~/pm-backtest/scripts/ops/compare_live_ref.py"),
    )
    ap.add_argument("--wallet-env", default=os.path.expanduser("~/.config/polymarket-exec/wallet.env"))
    ap.add_argument(
        "--balance-bin",
        default=os.path.expanduser(
            "~/deploy-main/polymarket-agent/target/release/balance_once"
        ),
    )
    ap.add_argument(
        "--parity-hours",
        type=float,
        default=4.0,
        help="Rolling parity window (align with cron interval)",
    )
    ap.add_argument("--exec-hours", type=float, default=4.0)
    ap.add_argument("--alert-unredeemed", type=int, default=5)
    ap.add_argument("--alert-orphans", type=int, default=5)
    ap.add_argument("--telegram-env", default=os.path.expanduser("~/.config/polymarket-watchdog/telegram.env"))
    args = ap.parse_args()

    if Path(args.telegram_env).is_file():
        for line in Path(args.telegram_env).read_text().splitlines():
            if line.startswith("export "):
                line = line[7:]
            if "=" in line and not line.strip().startswith("#"):
                k, v = line.split("=", 1)
                os.environ.setdefault(k.strip(), v.strip().strip('"').strip("'"))

    now = utc_now()
    data_dir = Path(args.data_dir)
    mon_dir = data_dir / "week_monitor"
    mon_dir.mkdir(parents=True, exist_ok=True)

    shadow_proc = pgrep("pm-app shadow")
    exec_proc = pgrep("shadow_exec_tail")
    kill_active = Path(args.kill_switch).is_file()
    cash = wallet_cash(Path(args.balance_bin), Path(args.wallet_env))
    redeem = redeem_stats(Path(args.live_log))
    exec_h = exec_stats(Path(args.live_log), args.exec_hours)
    pending = ledger_pending(Path(args.ledger))
    shadow_file = latest_shadow_jsonl(Path(args.shadow_dir))
    parity = (
        parity_submitted(shadow_file, Path(args.live_log), args.parity_hours)
        if shadow_file
        else {"error": "no shadow jsonl", "orphans": -1, "missed": -1}
    )

    critical: list[str] = []
    warn: list[str] = []

    if kill_active:
        warn.append("kill_switch_active")
    if not shadow_proc:
        critical.append("shadow-final NOT RUNNING")
    if not exec_proc and not kill_active:
        critical.append("shadow_exec_tail NOT RUNNING")
    if cash is not None and cash < 600:
        warn.append(f"venue_cash_low=${cash:.0f}")
    if redeem["unredeemed"] >= args.alert_unredeemed:
        critical.append(f"unredeemed={redeem['unredeemed']}")
    if parity.get("orphans", 0) >= args.alert_orphans:
        critical.append(f"parity_orphans_{args.parity_hours:g}h={parity.get('orphans')}")
    if parity.get("missed", 0) >= 10:
        warn.append(f"parity_missed_{args.parity_hours:g}h={parity.get('missed')}")
    if exec_h["submitted"] > 0 and exec_h["submit_miss"] > exec_h["submitted"]:
        warn.append(
            f"submit_miss_rate high ({exec_h['submit_miss']}/{exec_h['submitted']})"
        )

    snap = {
        "ts_utc": now.isoformat().replace("+00:00", "Z"),
        "venue_cash_usd": cash,
        "kill_switch": kill_active,
        "shadow_final_running": shadow_proc,
        "executor_running": exec_proc,
        "redeem": redeem,
        "exec_24h": exec_h,
        "ledger_pending": pending,
        "parity_24h": parity,
        "shadow_jsonl": str(shadow_file) if shadow_file else None,
        "critical": critical,
        "warn": warn,
    }

    day = now.strftime("%Y-%m-%d")
    jsonl_path = mon_dir / f"{day}.jsonl"
    with jsonl_path.open("a", encoding="utf-8") as f:
        f.write(json.dumps(snap) + "\n")

    human = (
        f"[{now.strftime('%Y-%m-%d %H:%M:%S')} UTC] "
        f"cash=${cash if cash is not None else '?'} "
        f"shadow={'Y' if shadow_proc else 'N'} exec={'Y' if exec_proc else 'N'} "
        f"kill={'Y' if kill_active else 'N'} | "
        f"redeem open={redeem['unredeemed']} | "
        f"{args.exec_hours:g}h sub={exec_h['submitted']} miss={exec_h['submit_miss']} | "
        f"parity_{args.parity_hours:g}h ref={parity.get('refs')} sub={parity.get('submitted')} "
        f"orphans={parity.get('orphans')} missed={parity.get('missed')}"
    )
    log_path = mon_dir / "week_monitor.log"
    with log_path.open("a", encoding="utf-8") as f:
        f.write(human + "\n")
        if critical:
            f.write("  CRITICAL: " + "; ".join(critical) + "\n")
        if warn:
            f.write("  WARN: " + "; ".join(warn) + "\n")

    print(human)
    if critical:
        print("CRITICAL:", "; ".join(critical))
        tg_send("shadow week_monitor CRITICAL\n" + human + "\n" + "; ".join(critical))
    elif warn:
        print("WARN:", "; ".join(warn))

    return 1 if critical else 0


if __name__ == "__main__":
    raise SystemExit(main())