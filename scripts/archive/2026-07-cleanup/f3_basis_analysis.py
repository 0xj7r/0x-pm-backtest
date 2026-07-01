"""F3 analysis: consume /tmp/f3_rows.json, test basis-momentum splits.

Splits per window: widening vs narrowing (d60/d300 sign), and side agreement
(Yes with widening / No with narrowing = agree). Threshold sweep on W3 only;
frozen rule applied to W1/W2.
"""
import json
import math

import numpy as np


def welch_t(a, b):
    a, b = np.asarray(a), np.asarray(b)
    if len(a) < 2 or len(b) < 2:
        return float("nan")
    va, vb = a.var(ddof=1) / len(a), b.var(ddof=1) / len(b)
    if va + vb == 0:
        return float("nan")
    return (a.mean() - b.mean()) / math.sqrt(va + vb)


def stats(rows):
    if not rows:
        return dict(n=0)
    pnl = np.array([r["pnl"] for r in rows])
    won = np.array([1.0 if r["won"] else 0.0 for r in rows])
    return dict(n=len(rows), avg_pnl=pnl.mean(), tot_pnl=pnl.sum(), hit=won.mean())


def fmt(s):
    if s["n"] == 0:
        return "n=0"
    return f"n={s['n']:5d} avg={s['avg_pnl']:+7.3f} tot={s['tot_pnl']:+9.1f} hit={s['hit']:.3f}"


def side_dir(side):
    return 1.0 if side == "Yes" else -1.0


def main():
    with open("/tmp/f3_rows.json") as fh:
        data = json.load(fh)

    for w in ("W3", "W1", "W2"):
        rows = data[w]
        print(f"\n=== {w} ({len(rows)} trades) overall: {fmt(stats(rows))}")
        for hz in ("d60", "d300"):
            widen = [r for r in rows if r[hz] > 0]
            narrow = [r for r in rows if r[hz] < 0]
            print(f"  {hz} widening : {fmt(stats(widen))}")
            print(f"  {hz} narrowing: {fmt(stats(narrow))}")
            agree = [r for r in rows if side_dir(r["side"]) * r[hz] > 0]
            disag = [r for r in rows if side_dir(r["side"]) * r[hz] < 0]
            t = welch_t([r["pnl"] for r in agree], [r["pnl"] for r in disag])
            print(f"  {hz} agree    : {fmt(stats(agree))}")
            print(f"  {hz} disagree : {fmt(stats(disag))}  (agree-disagree t={t:+.2f})")

    # threshold sweep on W3 only: signed momentum s = side_dir * d
    print("\n=== W3 threshold sweep (fit window only) ===")
    w3 = data["W3"]
    base = stats(w3)
    for hz in ("d60", "d300"):
        svals = np.array([side_dir(r["side"]) * r[hz] for r in w3])
        qs = [np.quantile(np.abs(svals), q) for q in (0.0, 0.25, 0.5, 0.75)]
        print(f"  {hz}: |s| quantiles q0/q25/q50/q75 = "
              + "/".join(f"{q:.3f}" for q in qs))
        for thr in qs:
            keep = [r for r, s in zip(w3, svals) if s > -thr]  # drop strong disagree
            drop = [r for r, s in zip(w3, svals) if s <= -thr]
            ks, ds = stats(keep), stats(drop)
            if ks["n"] == 0 or ds["n"] == 0:
                continue
            print(f"    skip s<=-{thr:.3f}: kept {fmt(ks)} | dropped {fmt(ds)}")

    # frozen rule validation happens after inspecting W3 output (edit FROZEN below)
    FROZEN = json.load(open("/tmp/f3_frozen.json")) if __import__("os").path.exists("/tmp/f3_frozen.json") else None
    if FROZEN:
        hz, thr = FROZEN["hz"], FROZEN["thr"]
        print(f"\n=== frozen rule (fit on W3): skip when side_dir*{hz} <= -{thr} ===")
        for w in ("W1", "W2", "W3"):
            rows = data[w]
            svals = np.array([side_dir(r["side"]) * r[hz] for r in rows])
            keep = [r for r, s in zip(rows, svals) if s > -thr]
            drop = [r for r, s in zip(rows, svals) if s <= -thr]
            print(f"  {w}: all {fmt(stats(rows))}")
            print(f"      kept {fmt(stats(keep))}")
            print(f"      drop {fmt(stats(drop))}")


if __name__ == "__main__":
    main()
