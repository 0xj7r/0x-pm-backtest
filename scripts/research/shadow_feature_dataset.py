#!/usr/bin/env python3
"""Build labeled entry dataset from shadow-final JSONL feature telemetry.

Joins would_enter rows (with exo/dir + binance flow features) to resolution
outcomes for offline gate screens and calibrator training.

Usage:
  python3 scripts/research/shadow_feature_dataset.py \\
    --shadow-dir /home/ubuntu/data/pm-alpha/shadow-final \\
    --out data/runs/shadow_features/entries.jsonl

  # With daily regime sidecar (scripts/research/shadow_day_regime.py):
  python3 scripts/research/shadow_feature_dataset.py \\
    --shadow-dir /home/ubuntu/data/pm-alpha/shadow-final \\
    --day-regime data/runs/shadow_features/day_regime.json \\
    --out data/runs/shadow_features/entries.jsonl

Feature name lists match pm-alpha:
  EXO_FEATURE_NAMES (16), DIR_FEATURE_NAMES (14) — see crates/pm-alpha/src/calibrator.rs
  and crates/pm-alpha/src/directional.rs.
"""
from __future__ import annotations

import argparse
import glob
import json
from collections import defaultdict
from pathlib import Path

EXO_NAMES = [
    "z_signed", "z_abs", "tau_fraction", "sigma_bar", "delta_bps",
    "mom_30s_sigma", "mom_120s_sigma", "mom_300s_sigma", "mom_accel",
    "flow_imbalance_60s", "flow_intensity", "large_adverse",
    "vol_ratio_short_long", "tod_sin", "tod_cos", "base_p_centered",
]
DIR_NAMES = [
    "funding_rate_bps", "oi_delta_5m", "oi_delta_30m", "basis_bps",
    "perp_flow_imbal_60s", "perp_burst_300s", "liq_proxy",
    "spot_flow_imbal_60s", "trend_60s_sigma", "trend_300s_sigma",
    "trend_1800s_sigma", "trend_alignment", "vol_expansion", "tau_fraction",
]
FLOW_NAMES = [
    "binance_flow_imbal_5s", "binance_flow_imbal_15s", "binance_flow_imbal_30s",
    "binance_adverse_vol_5s", "binance_adverse_vol_15s", "binance_adverse_vol_30s",
    "basis_d60_bps",
]


def index_resolutions(entries: list[dict], resolutions: list[dict]) -> dict:
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
    out = {}
    for (slug, side), ents in by_ent.items():
        for ent, res in zip(ents, by_res.get((slug, side), [])):
            out[(slug, side, int(ent.get("clip", 1)))] = (ent, res)
    return out


def named_features(values: list[float] | None, names: list[str]) -> dict[str, float] | None:
    if not values:
        return None
    return {n: float(v) for n, v in zip(names, values)}


def utc_day(ts_utc: str) -> str:
    return ts_utc[:10]


def load_day_regime(path: Path | None) -> dict[str, dict]:
    if path is None or not path.is_file():
        return {}
    return json.loads(path.read_text(encoding="utf-8"))


def flow_from_entry(ent: dict) -> dict[str, float]:
    out: dict[str, float] = {}
    for name in FLOW_NAMES:
        if name in ent and ent[name] is not None:
            out[name] = float(ent[name])
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--shadow-dir", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--day-regime", default=None, help="JSON sidecar from shadow_day_regime.py")
    ap.add_argument("--require-features", action="store_true",
                    help="skip rows missing exo_features/dir_features")
    ap.add_argument("--require-flow", action="store_true",
                    help="skip rows missing binance flow telemetry")
    args = ap.parse_args()

    shadow_dir = Path(args.shadow_dir)
    day_regime = load_day_regime(Path(args.day_regime) if args.day_regime else None)
    entries: list[dict] = []
    resolutions: list[dict] = []
    for fp in sorted(glob.glob(str(shadow_dir / "shadow-*.jsonl"))):
        for line in Path(fp).read_text().splitlines():
            if not line.strip():
                continue
            try:
                ev = json.loads(line)
            except json.JSONDecodeError:
                continue
            if ev.get("type") == "would_enter":
                entries.append(ev)
            elif ev.get("type") == "resolution":
                resolutions.append(ev)

    paired = index_resolutions(entries, resolutions)
    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    n_skip = 0
    with out_path.open("w", encoding="utf-8") as out:
        for (slug, side, clip), (ent, res) in sorted(
            paired.items(), key=lambda x: x[1][0]["ts_utc"]
        ):
            if args.require_features and (
                not ent.get("exo_features") or not ent.get("dir_features")
            ):
                n_skip += 1
                continue
            flow = flow_from_entry(ent)
            if args.require_flow and len(flow) < len(FLOW_NAMES):
                n_skip += 1
                continue
            day = utc_day(ent["ts_utc"])
            row = {
                "ts_utc": ent["ts_utc"],
                "utc_day": day,
                "slug": slug,
                "side": side,
                "clip": clip,
                "won": bool(res.get("won")),
                "pnl_usd": float(res.get("ladder_settle_pnl_usd") or 0),
                "p_side": ent.get("p_side"),
                "touch_price": ent.get("touch_price"),
                "model_book_gap": ent.get("model_book_gap"),
                "secs_from_open": ent.get("secs_from_open"),
                "delta_bps": ent.get("delta_bps"),
                "regime": ent.get("regime"),
                "spot_ret_10s_bps": ent.get("spot_ret_10s_bps"),
                "spot_ret_30s_bps": ent.get("spot_ret_30s_bps"),
                "spot_ret_60s_bps": ent.get("spot_ret_60s_bps"),
                "spot_ret_120s_bps": ent.get("spot_ret_120s_bps"),
                "spot_ret_300s_bps": ent.get("spot_ret_300s_bps"),
                "spot_ret_600s_bps": ent.get("spot_ret_600s_bps"),
                "spot_ret_900s_bps": ent.get("spot_ret_900s_bps"),
                "exo": named_features(ent.get("exo_features"), EXO_NAMES),
                "dir": named_features(ent.get("dir_features"), DIR_NAMES),
                "flow": flow or None,
                "day_regime": day_regime.get(day),
            }
            out.write(json.dumps(row) + "\n")

    print(f"wrote {out_path} rows={len(paired) - n_skip} skipped={n_skip}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())