"""Per-day, regime-tagged breakdown of the paired-MM (reuses mm_paired_sim internals).

Answers: within our May 7-20 sample, does the paired-MM's edge concentrate in the
CALM days (its regime) vs the TRENDING days (br2's regime)? All BTC-5m book data we
have is May, so this is a within-month calm-vs-trending contrast, not a cross-month OOS.

Regime axis per day = mean absolute 5-minute BTC move across that day's markets (bps).
Low = calm (MM-eligible), high = trending (br2's regime). We run the RECOMMENDED LIVE
config (clip 10, ungated, no rebate) and the gated variant, and report net per day
sorted by that vol axis.
"""
import glob
import sys

import numpy as np

import mm_paired_sim as S


def market_move_bps(bin_day, close):
    """Absolute 5-minute spot return over the market window, in bps."""
    if bin_day is None:
        return None
    ts, px = bin_day
    i0 = np.searchsorted(ts, close - S.WINDOW, side='right') - 1
    i1 = np.searchsorted(ts, close, side='right') - 1
    if i0 < 0 or i1 < 0 or px[i0] <= 0:
        return None
    return abs(px[i1] / px[i0] - 1.0) * 1e4


def main():
    book_files = sorted(glob.glob(f'{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    bin_cache = {}
    print('loading + parsing all markets once...', file=sys.stderr, flush=True)
    parsed = S.load_all_markets(book_files, bin_cache)
    print(f'parsed {len(parsed)} markets with trades', file=sys.stderr, flush=True)

    # per-market move magnitude, keyed by close ts within a date
    move_by_date = {}
    for book, close, trades, bin_day, date in parsed:
        m = market_move_bps(bin_day, close)
        if m is not None:
            move_by_date.setdefault(date, []).append(m)

    # run recommended (ungated) and gated, clip 10, no rebate
    rows_ungated = S.run(parsed, 10, False, False)
    rows_gated = S.run(parsed, 10, True, False)

    def by_day(rows):
        d = {}
        for r in rows:
            d.setdefault(r['date'], []).append(r)
        return d

    ug = by_day(rows_ungated)
    g = by_day(rows_gated)

    dates = sorted(move_by_date)
    # classify: a day is TRENDING if its mean |5m move| is in the top third of days
    day_vol = {d: float(np.mean(move_by_date[d])) for d in dates}
    vols = np.array([day_vol[d] for d in dates])
    trend_cut = float(np.percentile(vols, 66.6))

    print('=' * 104)
    print('PAIRED-MM PER-DAY  (clip 10, no rebate; recommended=ungated, also gated)  [all May 2026]')
    print('regime axis = mean |5m BTC move| across day; TRENDING = top third (>= %.1f bps), else CALM' % trend_cut)
    print('=' * 104)
    hdr = (f"{'date':<12}{'regime':<9}{'vol_bps':>8}{'mkts':>6}"
           f"{'UNGATED net':>13}{'/mkt':>8}{'pos%':>6}{'resid%':>8}"
           f"{'GATED net':>12}{'g_mkts':>8}")
    print(hdr)
    print('-' * 104)

    calm_net = trend_net = 0.0
    calm_days = trend_days = 0
    for d in dates:
        vb = day_vol[d]
        regime = 'TREND' if vb >= trend_cut else 'calm'
        ur = ug.get(d, [])
        gr = g.get(d, [])
        un = sum(r['net'] for r in ur)
        gn = sum(r['net'] for r in gr)
        pos = 100.0 * np.mean([r['net'] > 0 for r in ur]) if ur else 0.0
        resid = 100.0 * np.mean([r['resid_frac'] for r in ur]) if ur else 0.0
        pm = un / len(ur) if ur else 0.0
        print(f"{d:<12}{regime:<9}{vb:8.1f}{len(ur):6d}"
              f"{un:13.2f}{pm:8.3f}{pos:6.0f}{resid:8.1f}"
              f"{gn:12.2f}{len(gr):8d}")
        if regime == 'calm':
            calm_net += un; calm_days += 1
        else:
            trend_net += un; trend_days += 1

    print('-' * 104)
    tot = sum(r['net'] for r in rows_ungated)
    print(f"TOTAL ungated: ${tot:.2f} over {len(dates)} days = ${tot/len(dates):.2f}/day")
    if calm_days:
        print(f"  CALM  days ({calm_days}): ${calm_net:.2f}  = ${calm_net/calm_days:.2f}/day")
    if trend_days:
        print(f"  TREND days ({trend_days}): ${trend_net:.2f}  = ${trend_net/trend_days:.2f}/day")
    gtot = sum(r['net'] for r in rows_gated)
    print(f"TOTAL gated:   ${gtot:.2f} = ${gtot/len(dates):.2f}/day  ({sum(len(v) for v in g.values())} quotable mkts)")


if __name__ == '__main__':
    main()
