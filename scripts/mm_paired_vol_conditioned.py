"""Vol-conditioned paired-MM backtest (reuses mm_paired_sim internals).

Question: does the two-sided paired-MM make POSITIVE P&L in LOW-VOL markets (its
intended calm regime) and where does it turn negative as vol rises?

Per market we compute net P&L (clip 10, ungated AND gated, no rebate) and the
market's realized vol (stdev of 5s spot returns over the 5m window, in bps), then
bucket by vol and report per-bucket economics. We also split LOW-vol markets into
OSCILLATING (healthy spot sign-flip rate) vs DRIFTING (low flip), since the pair
only completes when price oscillates back through our quotes.

HONEST CAVEATS (also in the doc):
  - PRO-RATA queue fill model. Does NOT model favourite-side stranding / the
    reversal-day -EV losses we see LIVE (passive flatten failing). Absolute P&L is
    an UPPER BOUND / optimistic. The live gap is the stranding (being fixed via an
    active flatten). The VALUE here is the RELATIVE gradient across vol buckets,
    not the absolute level.
  - May calm sample only; no cross-regime OOS.
"""
import glob
import sys

import numpy as np

import mm_paired_sim as S


def realized_vol_bps(bin_day, close):
    """Stdev of 5s spot returns over the market window, in bps."""
    if bin_day is None:
        return None
    ts, px = bin_day
    lo, hi = close - S.WINDOW, close
    i0 = np.searchsorted(ts, lo, side='left')
    i1 = np.searchsorted(ts, hi, side='right')
    if i1 - i0 < 5:
        return None
    grid = np.arange(lo, hi, 5.0)
    gp = px[np.clip(np.searchsorted(ts, grid, side='right') - 1, 0, len(px) - 1)]
    if len(gp) < 3 or np.any(gp <= 0):
        return None
    rets = np.diff(gp) / gp[:-1]
    return float(np.std(rets) * 1e4)


def flip_frac(bin_day, close):
    """Fraction of 5s spot-return sign flips over the window (oscillation proxy)."""
    if bin_day is None:
        return None
    ts, px = bin_day
    lo, hi = close - S.WINDOW, close
    i0 = np.searchsorted(ts, lo, side='left')
    i1 = np.searchsorted(ts, hi, side='right')
    if i1 - i0 < 5:
        return None
    grid = np.arange(lo, hi, 5.0)
    gp = px[np.clip(np.searchsorted(ts, grid, side='right') - 1, 0, len(px) - 1)]
    if len(gp) < 4 or np.any(gp <= 0):
        return None
    rets = np.diff(gp) / gp[:-1]
    signs = np.sign(rets)
    nz = signs[signs != 0]
    if len(nz) < 2:
        return 0.0
    return float(np.mean(nz[1:] != nz[:-1]))


# Realized 5m vol on BTC-5m is concentrated under ~2 bps, so the coarse 2-bps-wide
# buckets all land in the first bin. We add fine sub-buckets in the dense low range
# to expose the gradient, then keep the coarse high buckets for the tail.
BUCKETS = [(0, 0.5), (0.5, 1), (1, 1.5), (1.5, 2),
           (2, 4), (4, 6), (6, 8), (8, 12), (12, float('inf'))]


def bucket_label(lo, hi):
    if hi == float('inf'):
        return f'>{lo:g}'
    return f'{lo:g}-{hi:g}'


def report_buckets(title, rows_by_close, vol_by_close, n_days):
    print('=' * 118)
    print(title)
    print('=' * 118)
    hdr = (f"{'vol_bucket':<12}{'n':>6}{'mean_net':>11}{'tot_net':>11}"
           f"{'$/day':>10}{'pos%':>7}{'mean_resid%':>13}{'worst_net':>12}")
    print(hdr)
    print('-' * 118)
    for lo, hi in BUCKETS:
        sub = [rows_by_close[c] for c in rows_by_close
               if lo <= vol_by_close.get(c, -1) < hi]
        if not sub:
            print(f"{bucket_label(lo, hi):<12}{0:>6}  (no markets)")
            continue
        net = np.array([r['net'] for r in sub])
        resid = np.array([r['resid_frac'] for r in sub])
        print(f"{bucket_label(lo, hi):<12}{len(sub):>6}{net.mean():>11.3f}"
              f"{net.sum():>11.2f}{net.sum() / n_days:>10.2f}"
              f"{100 * (net > 0).mean():>7.0f}{100 * resid.mean():>13.1f}"
              f"{net.min():>12.2f}")
    print('-' * 118)


def main():
    book_files = sorted(glob.glob(f'{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    bin_cache = {}
    print('loading + parsing all markets once...', file=sys.stderr, flush=True)
    parsed = S.load_all_markets(book_files, bin_cache)
    print(f'parsed {len(parsed)} markets with trades', file=sys.stderr, flush=True)

    dates = sorted({p[4] for p in parsed})
    n_days = len(dates)

    # per-market vol + flip, keyed by close ts (unique per market)
    vol_by_close = {}
    flip_by_close = {}
    for book, close, trades, bin_day, date in parsed:
        v = realized_vol_bps(bin_day, close)
        f = flip_frac(bin_day, close)
        if v is not None:
            vol_by_close[close] = v
        if f is not None:
            flip_by_close[close] = f

    rows_ungated = S.run(parsed, 10, False, False)
    rows_gated = S.run(parsed, 10, True, False)

    ug_by_close = {r['slug_close']: r for r in rows_ungated if r['slug_close'] in vol_by_close}
    g_by_close = {r['slug_close']: r for r in rows_gated if r['slug_close'] in vol_by_close}

    print()
    print(f'days={n_days} ({dates[0]}..{dates[-1]})  parsed_markets={len(parsed)}  '
          f'ungated_quotable={len(ug_by_close)}  gated_quotable={len(g_by_close)}')
    print('clip=10, no rebate. vol = stdev of 5s spot returns over 5m window (bps).')
    print()

    report_buckets('UNGATED (clip 10, no rebate)  -- net P&L by realized-vol bucket',
                   ug_by_close, vol_by_close, n_days)
    print()
    report_buckets('GATED (clip 10, no rebate)  -- net P&L by realized-vol bucket',
                   g_by_close, vol_by_close, n_days)

    # low vs high vs median (ungated)
    vols_seen = np.array([vol_by_close[c] for c in ug_by_close])
    med = float(np.median(vols_seen))
    low = [ug_by_close[c] for c in ug_by_close if vol_by_close[c] <= med]
    high = [ug_by_close[c] for c in ug_by_close if vol_by_close[c] > med]
    low_net = sum(r['net'] for r in low)
    high_net = sum(r['net'] for r in high)
    print()
    print('=' * 118)
    print(f'HEADLINE (UNGATED, clip 10)  median realized vol = {med:.2f} bps')
    print('=' * 118)
    print(f"  LOW-vol  (<= median): n={len(low):5d}  total=${low_net:9.2f}  "
          f"${low_net / n_days:7.2f}/day  pos={100 * np.mean([r['net'] > 0 for r in low]):.0f}%")
    print(f"  HIGH-vol (>  median): n={len(high):5d}  total=${high_net:9.2f}  "
          f"${high_net / n_days:7.2f}/day  pos={100 * np.mean([r['net'] > 0 for r in high]):.0f}%")

    # oscillating vs drifting WITHIN low-vol (<= median)
    low_with_flip = [(ug_by_close[c], flip_by_close[c]) for c in ug_by_close
                     if vol_by_close[c] <= med and c in flip_by_close]
    if low_with_flip:
        flips = np.array([f for _, f in low_with_flip])
        fmed = float(np.median(flips))
        osc = [r for r, f in low_with_flip if f >= fmed]
        drift = [r for r, f in low_with_flip if f < fmed]
        osc_net = sum(r['net'] for r in osc)
        drift_net = sum(r['net'] for r in drift)
        print()
        print('=' * 118)
        print(f'OSCILLATING vs DRIFTING within LOW-vol (<= median vol). '
              f'split at median flip frac = {fmed:.2f}')
        print('=' * 118)
        print(f"  OSCILLATING (flip >= {fmed:.2f}): n={len(osc):5d}  total=${osc_net:9.2f}  "
              f"${osc_net / n_days:7.2f}/day  mean=${osc_net / len(osc):.4f}  "
              f"pos={100 * np.mean([r['net'] > 0 for r in osc]):.0f}%")
        print(f"  DRIFTING    (flip <  {fmed:.2f}): n={len(drift):5d}  total=${drift_net:9.2f}  "
              f"${drift_net / n_days:7.2f}/day  mean=${drift_net / len(drift):.4f}  "
              f"pos={100 * np.mean([r['net'] > 0 for r in drift]):.0f}%")

    print()
    print('CAVEATS: pro-rata queue fills do NOT model favourite-side stranding / reversal-day')
    print('-EV losses seen LIVE -> absolute P&L is an UPPER BOUND. Value is the RELATIVE vol')
    print('gradient, not the level. May calm sample only; no cross-regime OOS.')


if __name__ == '__main__':
    main()
