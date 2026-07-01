#!/usr/bin/env python3
"""Shadow A/B gate sidecar — tails shadow-final JSONL without restarting the engine.

Evaluates counterfactual gates on each baseline `would_enter` and logs `gate_eval`
lines to a separate JSONL. Session state (loss streaks, last win/side) advances on
`resolution` events only, matching live decision timing.

Default gate: loss2 OR (after_win AND flip_side AND NOT side_aligned_30s).

Does NOT touch shadow-final buffers (spot/perp/vol stay warm). LIVE can keep
following ungated REF; use score_shadow_gate_ab.py to compare arms.

Usage (Dublin):
  python3 scripts/shadow_gate_sidecar.py \\
    --shadow-dir /home/ubuntu/data/pm-alpha/shadow-final \\
    --out /home/ubuntu/data/pm-alpha/shadow-gate-ab/gate_ab.jsonl
"""

from __future__ import annotations

import argparse
import glob
import json
import threading
import time
import urllib.request
from collections import deque
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path


GATE_NAME = "loss2_or_flip_misalign"


@dataclass
class SessionState:
    last_ts_utc: str | None = None
    last_side: str | None = None
    last_won: bool | None = None
    consec_losses: int = 0

    def observe_resolution(self, ev: dict) -> None:
        self.last_ts_utc = ev.get("ts_utc")
        self.last_side = str(ev.get("side", "")).lower() or None
        self.last_won = bool(ev.get("won"))
        if self.last_won:
            self.consec_losses = 0
        else:
            self.consec_losses += 1


@dataclass
class SpotBuffer:
    symbol: str = "BTCUSDT"
    maxlen: int = 180
    prices: deque[tuple[float, float]] = field(default_factory=deque)
    lock: threading.Lock = field(default_factory=threading.Lock)
    _stop: threading.Event = field(default_factory=threading.Event)

    def start(self, poll_s: float = 1.0) -> None:
        t = threading.Thread(target=self._poll_loop, args=(poll_s,), daemon=True)
        t.start()

    def stop(self) -> None:
        self._stop.set()

    def _poll_loop(self, poll_s: float) -> None:
        url = f"https://api.binance.com/api/v3/ticker/price?symbol={self.symbol}"
        while not self._stop.is_set():
            try:
                with urllib.request.urlopen(url, timeout=10) as resp:
                    data = json.loads(resp.read())
                px = float(data["price"])
                now = time.time()
                with self.lock:
                    self.prices.append((now, px))
                    while len(self.prices) > self.maxlen:
                        self.prices.popleft()
            except Exception:
                pass
            time.sleep(poll_s)

    def ret_bps(self, lookback_s: float = 30.0) -> float | None:
        with self.lock:
            if len(self.prices) < 2:
                return None
            now, last = self.prices[-1]
            target = now - lookback_s
            base = None
            for ts, px in self.prices:
                if ts <= target:
                    base = px
                else:
                    break
            if base is None:
                base = self.prices[0][1]
            if base <= 0:
                return None
            return (last / base - 1.0) * 10_000.0


def parse_ts(ts: str) -> datetime:
    return datetime.fromisoformat(ts.replace("Z", "+00:00"))


def utc_now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def side_aligned_30s(side: str, spot_ret_30s_bps: float | None) -> bool | None:
    if spot_ret_30s_bps is None:
        return None
    if side == "up":
        return spot_ret_30s_bps > 0
    return spot_ret_30s_bps < 0


def gate_loss2(state: SessionState) -> bool:
    return state.consec_losses < 2


def gate_flip_misalign_after_win(
    side: str, state: SessionState, aligned: bool | None
) -> bool:
    if state.last_side is None or state.last_won is not True:
        return True
    if side == state.last_side:
        return True
    if aligned is False:
        return False
    return True


def eval_gate(
    side: str, state: SessionState, spot_ret_30s_bps: float | None
) -> tuple[bool, str | None]:
    if not gate_loss2(state):
        return False, "loss2"
    aligned = side_aligned_30s(side, spot_ret_30s_bps)
    if not gate_flip_misalign_after_win(side, state, aligned):
        return False, "flip_misalign"
    return True, None


@dataclass
class TailState:
    path: str
    offset: int = 0

    @classmethod
    def load(cls, path: Path) -> TailState:
        if path.exists():
            raw = json.loads(path.read_text())
            return cls(path=str(raw.get("path", "")), offset=int(raw.get("offset", 0)))
        return cls(path="", offset=0)

    def save(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({"path": self.path, "offset": self.offset}))


def latest_shadow_file(shadow_dir: Path) -> Path:
    files = sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl")))
    if not files:
        raise SystemExit(f"no shadow-*.jsonl in {shadow_dir}")
    return Path(files[-1])


def process_line(
    line: str,
    state: SessionState,
    spot: SpotBuffer,
    out_fp,
    gate_name: str,
) -> None:
    line = line.strip()
    if not line:
        return
    try:
        ev = json.loads(line)
    except json.JSONDecodeError:
        return

    typ = ev.get("type")
    if typ == "resolution":
        state.observe_resolution(ev)
        return
    if typ != "would_enter":
        return

    side = str(ev.get("side", "")).lower()
    spot_ret = ev.get("spot_ret_30s_bps")
    if spot_ret is not None:
        spot_ret_30s_bps = float(spot_ret)
    else:
        spot_ret_30s_bps = spot.ret_bps(30.0)

    gated_allow, block_reason = eval_gate(side, state, spot_ret_30s_bps)
    aligned = side_aligned_30s(side, spot_ret_30s_bps)

    row = {
        "type": "gate_eval",
        "ts_utc": ev.get("ts_utc") or utc_now(),
        "slug": ev.get("slug"),
        "side": side,
        "clip": ev.get("clip", 1),
        "gate": gate_name,
        "baseline_allow": True,
        "gated_allow": gated_allow,
        "block_reason": block_reason,
        "consec_losses": state.consec_losses,
        "last_side": state.last_side,
        "last_won": state.last_won,
        "spot_ret_30s_bps": spot_ret_30s_bps,
        "side_aligned_30s": aligned,
        "p_side": ev.get("p_side"),
        "touch_price": ev.get("touch_price"),
        "target_notional": ev.get("target_notional", 50.0),
    }
    out_fp.write(json.dumps(row) + "\n")
    out_fp.flush()


def warmup_session_from_dir(shadow_dir: Path, state: SessionState) -> None:
    """Replay resolutions only so the first live gate_eval has correct streak state."""
    resolutions: list[dict] = []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for ev in load_jsonl_events(fp):
            if ev.get("type") == "resolution":
                resolutions.append(ev)
    resolutions.sort(key=lambda r: r.get("ts_utc", ""))
    for ev in resolutions:
        state.observe_resolution(ev)


def load_jsonl_events(path: str | Path) -> list[dict]:
    out: list[dict] = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return out


def catch_up_file(
    fp,
    state: SessionState,
    spot: SpotBuffer,
    out_fp,
    gate_name: str,
    start_offset: int,
) -> int:
    fp.seek(start_offset)
    offset = start_offset
    while True:
        line = fp.readline()
        if not line:
            break
        offset = fp.tell()
        process_line(line, state, spot, out_fp, gate_name)
    return offset


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--shadow-dir",
        default="/home/ubuntu/data/pm-alpha/shadow-final",
        help="shadow-final directory (latest shadow-*.jsonl)",
    )
    ap.add_argument(
        "--out",
        default="/home/ubuntu/data/pm-alpha/shadow-gate-ab/gate_ab.jsonl",
    )
    ap.add_argument(
        "--state",
        default="/home/ubuntu/data/pm-alpha/shadow-gate-ab/tail_state.json",
    )
    ap.add_argument(
        "--session-state",
        default="/home/ubuntu/data/pm-alpha/shadow-gate-ab/session_state.json",
        help="persisted gate session (consec losses, last win)",
    )
    ap.add_argument("--follow", action="store_true", default=True)
    ap.add_argument("--no-follow", action="store_true")
    ap.add_argument("--from-start", action="store_true")
    ap.add_argument("--spot-poll-s", type=float, default=1.0)
    args = ap.parse_args()

    follow = args.follow and not args.no_follow
    shadow_dir = Path(args.shadow_dir)
    out_path = Path(args.out)
    state_path = Path(args.state)
    session_path = Path(args.session_state)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    session = SessionState()
    if session_path.exists() and not args.from_start:
        raw = json.loads(session_path.read_text())
        session = SessionState(
            last_ts_utc=raw.get("last_ts_utc"),
            last_side=raw.get("last_side"),
            last_won=raw.get("last_won"),
            consec_losses=int(raw.get("consec_losses", 0)),
        )
    else:
        warmup_session_from_dir(shadow_dir, session)
        print(
            f"warmed session: consec_losses={session.consec_losses} "
            f"last={session.last_side} won={session.last_won}",
            flush=True,
        )

    tail = TailState.load(state_path)
    shadow_file = latest_shadow_file(shadow_dir)
    offset = 0 if args.from_start else tail.offset
    if tail.path == str(shadow_file) and not args.from_start:
        offset = tail.offset

    spot = SpotBuffer()
    spot.start(args.spot_poll_s)

    print(
        f"shadow_gate_sidecar: file={shadow_file.name} offset={offset} "
        f"gate={GATE_NAME} follow={follow}",
        flush=True,
    )

    try:
        with out_path.open("a", encoding="utf-8") as out_fp:
            while True:
                shadow_file = latest_shadow_file(shadow_dir)
                with shadow_file.open("r", encoding="utf-8") as inf:
                    if str(shadow_file) != tail.path:
                        print(f"rolled to {shadow_file.name}", flush=True)
                        tail.path = str(shadow_file)
                        offset = 0
                    offset = catch_up_file(
                        inf, session, spot, out_fp, GATE_NAME, offset
                    )
                    tail.offset = offset
                    tail.path = str(shadow_file)
                    tail.save(state_path)
                    session_path.write_text(
                        json.dumps(
                            {
                                "last_ts_utc": session.last_ts_utc,
                                "last_side": session.last_side,
                                "last_won": session.last_won,
                                "consec_losses": session.consec_losses,
                            }
                        )
                    )
                if not follow:
                    break
                time.sleep(0.25)
    finally:
        spot.stop()

    return 0


if __name__ == "__main__":
    raise SystemExit(main())