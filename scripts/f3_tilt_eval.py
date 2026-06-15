"""F3 frozen-rule evaluation: sign-based sizing tilt on side_dir * d60.

Rule chosen on W3: stake x1.25 when signed basis momentum s60 > 0,
x0.75 when s60 < 0, x1.0 when s60 == 0. No fitted threshold (sign only).
Linear pnl scaling assumption (per-trade stake scales pnl proportionally).
"""
import json

import numpy as np

WA, WD = 1.25, 0.75


def side_dir(side):
    return 1.0 if side == "Yes" else -1.0


def main():
    with open("/tmp/f3_rows.json") as fh:
        data = json.load(fh)
    for w in ("W3", "W1", "W2"):
        rows = data[w]
        pnl = np.array([r["pnl"] for r in rows])
        s = np.array([side_dir(r["side"]) * r["d60"] for r in rows])
        mult = np.where(s > 0, WA, np.where(s < 0, WD, 1.0))
        base, tilt = pnl.sum(), (pnl * mult).sum()
        stake = mult.mean()
        print(f"{w}: base={base:+9.1f} tilt={tilt:+9.1f} "
              f"delta={tilt - base:+8.1f} ({100 * (tilt / base - 1):+.1f}%) "
              f"avg_stake_mult={stake:.3f}")
        # magnitude monotonicity: terciles of |s| within agree and disagree
        for label, mask in (("agree", s > 0), ("disagree", s < 0)):
            ps, ss = pnl[mask], np.abs(s[mask])
            q1, q2 = np.quantile(ss, [1 / 3, 2 / 3])
            for name, m in (("small", ss <= q1), ("mid", (ss > q1) & (ss <= q2)),
                            ("large", ss > q2)):
                print(f"    {label:8s} |s| {name:5s}: n={m.sum():5d} "
                      f"avg={ps[m].mean():+7.3f} med={np.median(ps[m]):+7.3f}")
        # median pnl check (outlier robustness)
        print(f"    median pnl agree={np.median(pnl[s > 0]):+.3f} "
              f"disagree={np.median(pnl[s < 0]):+.3f}")


if __name__ == "__main__":
    main()
