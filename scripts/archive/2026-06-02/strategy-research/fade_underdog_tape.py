"""FADE / buy-the-underdog EV: at T seconds-to-close, buy the CHEAPER side (mid<0.5) at
its ask and hold to resolution. The mirror of late_favourite_tape (which showed favourites
are -EV / overpriced). If favourites are overpriced, the underdog should be UNDERpriced =>
+EV. This validates the contrarian/mean-reversion play before we'd build it.

Key checks: (1) is underdog EV>0 after paying the ask (spread)?, (2) does it hold across
time-to-close?, (3) the TAIL - fading gets crushed when a market genuinely trends, so we
report worst buckets + the low/high vol split (mean-reversion vs trend).
"""
import glob, sys
import numpy as np
import mm_paired_sim as S

CHECKPOINTS = [60.0, 45.0, 30.0, 20.0, 15.0, 10.0, 5.0]
PRICE_EDGES = [0.03, 0.20, 0.30, 0.40, 0.50]  # underdog price buckets (<0.5)


def realized_vol_bps(bin_day, close):
    if bin_day is None: return None
    ts, px = bin_day
    lo, hi = close - S.WINDOW, close
    i0 = np.searchsorted(ts, lo, 'left'); i1 = np.searchsorted(ts, hi, 'right')
    if i1 - i0 < 5: return None
    grid = np.arange(lo, hi, 5.0)
    gp = px[np.clip(np.searchsorted(ts, grid, 'right') - 1, 0, len(px) - 1)]
    if len(gp) < 3 or np.any(gp <= 0): return None
    return float(np.std(np.diff(gp) / gp[:-1]) * 1e4)


def underdog_at(book, T):
    """(underdog_is_yes, underdog_buy_price) at ~T sec-to-close, or None."""
    t = book['t']; i = int(np.argmin(np.abs(t - T)))
    if abs(t[i] - T) > 8.0: return None
    ymid, ybid, yask = book['mid'][i], book['bid'][i], book['ask'][i]
    if not (0 < ybid <= yask < 1): return None
    if ymid < 0.5:
        return True, yask          # YES is the underdog, buy at its ask
    return False, 1.0 - ybid        # NO is the underdog, buy NO at 1 - yes_bid


def main():
    book_files = sorted(glob.glob(f'{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    bin_cache = {}
    print('loading...', file=sys.stderr, flush=True)
    parsed = S.load_all_markets(book_files, bin_cache)
    print(f'parsed {len(parsed)}', file=sys.stderr, flush=True)

    rows, vols = [], []
    for book, close, trades, bin_day, date in parsed:
        yes_wins, _, _ = S.binance_outcome_and_vol(bin_day, close)
        if yes_wins is None: continue
        vb = realized_vol_bps(bin_day, close)
        if vb is not None: vols.append(vb)
        for T in CHECKPOINTS:
            u = underdog_at(book, T)
            if u is None: continue
            is_yes, price = u
            if not (0.03 <= price < 0.50): continue
            won = yes_wins if is_yes else (not yes_wins)
            rows.append((T, price, 1 if won else 0, vb))
    vmed = float(np.median([v for v in vols if v is not None])) if vols else 0.0
    print('=' * 92)
    print('FADE / BUY-THE-UNDERDOG EV  (May BTC-5m; buy cheaper side at ask, hold to resolution)')
    print(f'markets={len(parsed)} obs={len(rows)} vol_median={vmed:.1f}bps. EV/$1=P(win)-price. +EV=>underdog underpriced.')
    print('=' * 92)

    def report(label, sub):
        print(f'\n#### {label} (n={len(sub)}) ' + '#' * 36)
        print(f"  {'T(s)':>5}{'u_price':>10}{'n':>6}{'P(win)':>8}{'avg_px':>7}{'EV/$1':>8}{'EV%':>7}")
        for T in CHECKPOINTS:
            for lo, hi in zip(PRICE_EDGES[:-1], PRICE_EDGES[1:]):
                g = [r for r in sub if r[0] == T and lo <= r[1] < hi]
                if len(g) < 10: continue
                pw = np.mean([r[2] for r in g]); ap = np.mean([r[1] for r in g]); ev = pw - ap
                print(f"  {T:>5.0f}{f'{lo:.2f}-{hi:.2f}':>10}{len(g):>6}{pw:>8.3f}{ap:>7.3f}{ev:>+8.3f}{100*ev/ap:>+6.1f}%")

    report('ALL', rows)
    report('LOW VOL (<=median; mean-reverting)', [r for r in rows if r[3] is not None and r[3] <= vmed])
    report('HIGH VOL (>median; trending risk)', [r for r in rows if r[3] is not None and r[3] > vmed])

    # pooled headline: near-coinflip underdog (0.40-0.50) and how it nets
    for lab, lo, hi in [('coinflip-underdog 0.40-0.50', 0.40, 0.50), ('mid-underdog 0.20-0.40', 0.20, 0.40)]:
        g = [r for r in rows if lo <= r[1] < hi]
        if g:
            pw = np.mean([r[2] for r in g]); ap = np.mean([r[1] for r in g])
            # per-$1-staked net pnl distribution: win -> (1-price), lose -> -price
            pnls = [(1 - r[1]) if r[2] else (-r[1]) for r in g]
            print(f"\n{lab}: n={len(g)} P(win)={pw:.3f} avg_px={ap:.3f} EV/$1={pw-ap:+.3f} ({100*(pw-ap)/ap:+.1f}%)  "
                  f"mean_pnl/$1={np.mean(pnls):+.4f} worst={min(pnls):+.3f} stdev={np.std(pnls):.3f}")


if __name__ == '__main__':
    main()
