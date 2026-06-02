#!/usr/bin/env python3
"""Sweep realistic-fill paired-MM gate/repair parameters.

This is the Phase-3 harness for the shelved neutral-MM question. It deliberately
ranks configs on spread-capture quality, not just net PnL, because the calibrated
fill model showed that positive net can be a thin directional residual coin-flip.
"""

import argparse
import csv
import glob
import itertools
import sys
from contextlib import contextmanager

import mm_paired_realistic_sim as real
import mm_paired_sim as base


@contextmanager
def patched_base(**values):
    old = {name: getattr(base, name) for name in values}
    try:
        for name, value in values.items():
            setattr(base, name, value)
        yield
    finally:
        for name, value in old.items():
            setattr(base, name, value)


def floats(raw):
    return [float(x) for x in raw.split(",") if x.strip()]


def ints(raw):
    return [int(x) for x in raw.split(",") if x.strip()]


def load_parsed(limit):
    book_files = sorted(glob.glob(f"{base.BOOK_ROOT}/date=*/asset_id=*/*.parquet"))
    if limit:
        step = max(1, len(book_files) // limit)
        book_files = book_files[::step][:limit]
    dates = sorted(
        {
            [p for p in path.split("/") if p.startswith("date=")][0].split("=")[1]
            for path in book_files
        }
    )
    print(
        f"book files: {len(book_files)} days={len(dates)} ({dates[0]}..{dates[-1]})",
        file=sys.stderr,
        flush=True,
    )
    bin_cache = {}
    parsed = base.load_all_markets(book_files, bin_cache)
    print(f"parsed {len(parsed)} markets with trades", file=sys.stderr, flush=True)
    return parsed, len(dates)


def eval_config(parsed, n_days, cfg):
    with patched_base(
        REGIME_RANGE_MAX=cfg["range_max"],
        REGIME_SPOT_VOL_MAX=cfg["vol_max"],
        REGIME_FLIP_MIN=cfg["flip_min"],
        REPAIR_DELTA_SHARES=cfg["repair_delta"],
        LATE_PULL_SECS=cfg["late_pull"],
        SPOT_ACCEL_PULL=cfg["spot_pull"],
        LARGE_TAKER_SHARES=cfg["large_taker"],
    ):
        rows = real.run_realistic(
            parsed,
            cfg["clip"],
            True,
            cfg["rebate_on"],
            cfg["K"],
            cfg["rest"],
            cfg["cadence"],
        )
        summary = base.summarize(rows, "realistic", n_days)
        fill = real.implied_fill_stats(rows, cfg["clip"])

    if summary["n_markets"] == 0 or fill is None:
        return None

    pair_per_day = summary["pnl_pairs_total"] / n_days
    resid_per_day = summary["pnl_resid_total"] / n_days
    net_per_day = summary["net_per_day"]
    # Positive score requires real spread capture and penalizes residual reliance.
    score = pair_per_day - 0.5 * abs(resid_per_day)
    return {
        **cfg,
        "markets": summary["n_markets"],
        "net_day": net_per_day,
        "pair_day": pair_per_day,
        "resid_day": resid_per_day,
        "rebate_day": summary["rebate_total"] / n_days,
        "pos_pct": summary["pct_markets_pos"],
        "mean_resid_pct": summary["mean_resid_frac"] * 100,
        "fills_mkt": summary["fills_per_market"],
        "shares_mkt": summary["shares_per_market"],
        "worst": summary["worst_market_net"],
        "order_fill_pct": fill["order_fill_rate_pct"],
        "uncond_share_pct": fill["unconditional_share_frac_pct"],
        "cap_mult_geo": fill["cap_mult_geomean"],
        "score": score,
    }


def print_table(rows, limit):
    headers = [
        "score",
        "clip",
        "repair_delta",
        "range_max",
        "vol_max",
        "flip_min",
        "late_pull",
        "spot_pull",
        "net_day",
        "pair_day",
        "resid_day",
        "pos_pct",
        "mean_resid_pct",
        "order_fill_pct",
        "uncond_share_pct",
        "worst",
    ]
    print(" | ".join(headers))
    print(" | ".join("---" for _ in headers))
    for row in rows[:limit]:
        print(
            " | ".join(
                [
                    f"{row['score']:.3f}",
                    str(row["clip"]),
                    f"{row['repair_delta']:.1f}",
                    f"{row['range_max']:.3f}",
                    f"{row['vol_max']:.5f}",
                    f"{row['flip_min']:.2f}",
                    f"{row['late_pull']:.0f}",
                    f"{row['spot_pull']:.5f}",
                    f"{row['net_day']:.2f}",
                    f"{row['pair_day']:.2f}",
                    f"{row['resid_day']:.2f}",
                    f"{row['pos_pct']:.1f}",
                    f"{row['mean_resid_pct']:.1f}",
                    f"{row['order_fill_pct']:.2f}",
                    f"{row['uncond_share_pct']:.2f}",
                    f"{row['worst']:.2f}",
                ]
            )
        )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--top", type=int, default=20)
    ap.add_argument("--out-csv", default="")
    ap.add_argument("--clips", default="5,10")
    ap.add_argument("--repair-deltas", default="1,2,5")
    ap.add_argument("--range-max", default="0.04,0.06,0.08")
    ap.add_argument("--vol-max", default="0.00008,0.00012,0.00018")
    ap.add_argument("--flip-min", default="0.10,0.20,0.35")
    ap.add_argument("--late-pull", default="30,45,60")
    ap.add_argument("--spot-pull", default="0.0006,0.0008,0.0012")
    ap.add_argument("--large-taker", default="100,150")
    ap.add_argument("--K", type=float, default=real.CAPTURE_K)
    ap.add_argument("--rest", type=float, default=real.REST_WINDOW)
    ap.add_argument("--cadence", type=float, default=real.REQUOTE_CADENCE)
    ap.add_argument("--rebate", action="store_true")
    args = ap.parse_args()

    parsed, n_days = load_parsed(args.limit)

    rows = []
    grid = itertools.product(
        ints(args.clips),
        floats(args.repair_deltas),
        floats(args.range_max),
        floats(args.vol_max),
        floats(args.flip_min),
        floats(args.late_pull),
        floats(args.spot_pull),
        floats(args.large_taker),
    )
    total = 0
    for (
        clip,
        repair_delta,
        range_max,
        vol_max,
        flip_min,
        late_pull,
        spot_pull,
        large_taker,
    ) in grid:
        total += 1
        cfg = {
            "clip": clip,
            "repair_delta": repair_delta,
            "range_max": range_max,
            "vol_max": vol_max,
            "flip_min": flip_min,
            "late_pull": late_pull,
            "spot_pull": spot_pull,
            "large_taker": large_taker,
            "K": args.K,
            "rest": args.rest,
            "cadence": args.cadence,
            "rebate_on": args.rebate,
        }
        row = eval_config(parsed, n_days, cfg)
        if row is not None:
            rows.append(row)

    rows.sort(
        key=lambda r: (
            r["score"],
            r["pair_day"],
            -abs(r["resid_day"]),
            r["pos_pct"],
        ),
        reverse=True,
    )
    print(f"evaluated={total} usable={len(rows)} rebate={args.rebate}", file=sys.stderr)
    print_table(rows, args.top)

    if args.out_csv:
        with open(args.out_csv, "w", newline="") as f:
            writer = csv.DictWriter(f, fieldnames=list(rows[0].keys()) if rows else [])
            writer.writeheader()
            writer.writerows(rows)


if __name__ == "__main__":
    main()
