#!/usr/bin/env python3
"""Alert-only drawdown monitor for the live Polymarket account.

Computes drawdown from the running equity peak and emits a single JSON line
plus a nonzero exit code when thresholds are crossed:

  - hard_floor: equity at or below the 5-share-floor viability line
    (default $550), where fractional sizing breaks. This is the mechanical
    stop-and-reassess line.
  - soft_warn: drawdown from peak at or above the soft threshold
    (default 25%). A review trigger (variance vs breakage check), NOT a halt.

This is a MONITOR. It never calls trading APIs and never halts anything; it
only prints one JSON line and exits nonzero so a cron wrapper can page a
human. See docs/drawdown-handling-plan-2026-07.md.

Usage:
  # one-shot current-equity check
  python3 scripts/ops/drawdown_monitor.py --equity 600 --peak 850
  # series (peak = running max when --peak absent)
  python3 scripts/ops/drawdown_monitor.py --equity-jsonl equity.jsonl
  # else stdin, one float per line
  echo -e "900\\n820\\n600" | python3 scripts/ops/drawdown_monitor.py
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

DEFAULT_SOFT_DD = 0.25
DEFAULT_HARD_FLOOR = 550.0

EXIT_OK = 0
EXIT_SOFT_WARN = 1
EXIT_HARD_FLOOR = 2
EXIT_USAGE = 3


def classify(equity: float, peak: float, soft_dd: float, hard_floor: float) -> tuple[str, int, float]:
    """Return (state, exit_code, drawdown_fraction) for the given equity.

    drawdown_fraction = (peak - equity) / peak, clamped to >= 0.
    hard_floor takes precedence over soft_warn.
    """
    dd = (peak - equity) / peak if peak > 0 else 0.0
    if equity <= hard_floor:
        return "hard_floor", EXIT_HARD_FLOOR, dd
    if dd >= soft_dd:
        return "soft_warn", EXIT_SOFT_WARN, dd
    return "ok", EXIT_OK, dd


def message(state: str, equity: float, peak: float, dd_pct: float, soft_dd: float, hard_floor: float) -> str:
    if state == "hard_floor":
        return (
            f"HARD FLOOR: equity ${equity:.2f} at or below the ${hard_floor:.0f} "
            f"5-share-floor viability line. Stop and reassess; this is an alert, not an auto-halt."
        )
    if state == "soft_warn":
        return (
            f"SOFT WARN: drawdown {dd_pct:.1f}% from peak ${peak:.2f} to ${equity:.2f} "
            f"(>= {soft_dd * 100:.0f}%). Review variance vs breakage; not a halt."
        )
    return (
        f"OK: equity ${equity:.2f} within {soft_dd * 100:.0f}% of peak ${peak:.2f} "
        f"(drawdown {dd_pct:.1f}%)."
    )


def build_report(equity: float, peak: float, soft_dd: float, hard_floor: float) -> tuple[dict, int]:
    state, exit_code, dd = classify(equity, peak, soft_dd, hard_floor)
    dd_pct = round(100.0 * dd, 2)
    report = {
        "state": state,
        "peak": round(peak, 2),
        "equity": round(equity, 2),
        "drawdown_pct": dd_pct,
        "soft_dd": soft_dd,
        "hard_floor": hard_floor,
        "message": message(state, equity, peak, dd_pct, soft_dd, hard_floor),
    }
    return report, exit_code


def read_series(args: argparse.Namespace) -> tuple[float, float]:
    """Resolve (current_equity, peak) from the configured input source.

    Priority: --equity (one-shot) > --equity-jsonl (series) > stdin (floats).
    --peak, when given, overrides the computed running max.
    """
    if args.equity is not None:
        equity = args.equity
        peak = args.peak if args.peak is not None else equity
        return equity, peak

    series: list[float] = []
    if args.equity_jsonl:
        for line in Path(args.equity_jsonl).read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue
            val = rec.get("equity_usd")
            if val is None:
                continue
            series.append(float(val))
    else:
        for tok in sys.stdin.read().split():
            try:
                series.append(float(tok))
            except ValueError:
                continue

    if not series:
        raise SystemExit(
            json.dumps(
                {"state": "error", "message": "no equity values provided"}
            )
        )
    equity = series[-1]
    peak = args.peak if args.peak is not None else max(series)
    return equity, peak


def self_test() -> int:
    cases = [
        (550.0, 850.0, "hard_floor", EXIT_HARD_FLOOR),
        (600.0, 850.0, "soft_warn", EXIT_SOFT_WARN),
        (800.0, 850.0, "ok", EXIT_OK),
    ]
    for equity, peak, want_state, want_exit in cases:
        state, exit_code, _ = classify(equity, peak, DEFAULT_SOFT_DD, DEFAULT_HARD_FLOOR)
        assert state == want_state, f"equity={equity}: state {state} != {want_state}"
        assert exit_code == want_exit, f"equity={equity}: exit {exit_code} != {want_exit}"

    # series path: running max is the peak when --peak absent
    series_peak = max([900.0, 820.0, 600.0])
    state, _, dd = classify(600.0, series_peak, DEFAULT_SOFT_DD, DEFAULT_HARD_FLOOR)
    assert state == "soft_warn", f"series: state {state} != soft_warn"
    assert abs(dd - (900.0 - 600.0) / 900.0) < 1e-9, "series: drawdown mismatch"

    print(json.dumps({"self_test": "passed", "cases": len(cases) + 1}))
    return EXIT_OK


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--equity-jsonl", default=None, help="JSONL series; each line has equity_usd")
    ap.add_argument("--equity", type=float, default=None, help="one-shot current equity")
    ap.add_argument("--peak", type=float, default=None, help="known peak; else running max of the series")
    ap.add_argument("--soft-dd", type=float, default=DEFAULT_SOFT_DD, help="soft drawdown fraction (default 0.25)")
    ap.add_argument("--hard-floor", type=float, default=DEFAULT_HARD_FLOOR, help="hard floor equity USD (default 550)")
    ap.add_argument("--self-test", action="store_true", help="run built-in assertions and exit")
    args = ap.parse_args()

    if args.self_test:
        return self_test()

    equity, peak = read_series(args)
    report, exit_code = build_report(equity, peak, args.soft_dd, args.hard_floor)
    print(json.dumps(report))
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
