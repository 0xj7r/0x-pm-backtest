"""Late-favourite directional EV: at T seconds-to-close, does a favourite priced p
win MORE than p of the time? (i.e. is the late favourite underpriced -> +EV to buy?)

Tape analysis on the May BTC-5m data (reuses mm_paired_sim loaders). For each market,
at several seconds-to-close checkpoints, take the favourite's BUY price from the book
(yes_ask if yes_mid>0.5 else 1-yes_bid) and whether that favourite ultimately WON
(from Binance spot outcome). Bucket by (T, price) and compute P(win) vs price paid =>
EV per $1 staked = P(win) - price. Split by per-market realized vol (low = the regime
br2 vetoes and skips). Conservative: uses the ASK we'd actually pay (taker), no rebate.
"""
import glob
import sys
import numpy as np
import mm_paired_sim as S

CHECKPOINTS = [60.0, 45.0, 30.0, 20.0, 15.0, 10.0, 5.0]
PRICE_EDGES = [0.50, 0.60, 0.70, 0.80, 0.90, 0.97, 1.01]


def realized_vol_bps(bin_day, close):
    """Stdev of 5s spot returns over the market window, in bps (regime proxy)."""
    if bin_day is None:
        return None
    ts, px = bin_day
    lo, hi = close - S.WINDOW, close
    i0 = np.searchsorted(ts, lo, side='left'); i1 = np.searchsorted(ts, hi, side='right')
    if i1 - i0 < 5:
        return None
    grid = np.arange(lo, hi, 5.0)
    gp = px[np.clip(np.searchsorted(ts, grid, side='right') - 1, 0, len(px) - 1)]
    if len(gp) < 3 or np.any(gp <= 0):
        return None
    rets = np.diff(gp) / gp[:-1]
    return float(np.std(rets) * 1e4)


def fav_at(book, T):
    """(fav_is_yes, fav_buy_price) at seconds-to-close ~ T, or None."""
    t = book['t']  # descending seconds-to-close
    i = int(np.argmin(np.abs(t - T)))
    if abs(t[i] - T) > 8.0:  # no snapshot within 8s of the checkpoint
        return None
    ymid, ybid, yask = book['mid'][i], book['bid'][i], book['ask'][i]
    if not (0 < ybid <= yask < 1):
        return None
    if ymid >= 0.5:
        return True, yask            # buy YES at its ask
    return False, 1.0 - ybid          # buy NO at 1 - yes_bid


def main():
    book_files = sorted(glob.glob(f'{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    bin_cache = {}
    print('loading + parsing all markets once...', file=sys.stderr, flush=True)
    parsed = S.load_all_markets(book_files, bin_cache)
    print(f'parsed {len(parsed)} markets', file=sys.stderr, flush=True)

    # rows: (T, fav_price, fav_won, vol_bps)
    rows = []
    vols = []
    for book, close, trades, bin_day, date in parsed:
        yes_wins, _, _ = S.binance_outcome_and_vol(bin_day, close)
        if yes_wins is None:
            continue
        vb = realized_vol_bps(bin_day, close)
        if vb is not None:
            vols.append(vb)
        for T in CHECKPOINTS:
            f = fav_at(book, T)
            if f is None:
                continue
            fav_is_yes, price = f
            if not (0.5 <= price < 1.0):
                continue
            fav_won = yes_wins if fav_is_yes else (not yes_wins)
            rows.append((T, price, 1 if fav_won else 0, vb))

    vmed = float(np.median([v for v in vols if v is not None])) if vols else 0.0
    print('=' * 96)
    print('LATE-FAVOURITE DIRECTIONAL EV  (May BTC-5m; buy favourite at its ask, hold to resolution)')
    print(f'markets={len(parsed)}  obs={len(rows)}  per-market realized-vol median={vmed:.1f} bps (low/high split)')
    print('EV per $1 = P(fav wins) - price_paid.  +EV => favourite UNDERPRICED (edge to buy).')
    print('=' * 96)

    def report(label, sub):
        print(f'\n#### {label}  (n={len(sub)}) ' + '#' * 40)
        print(f"  {'T(s)':>5} {'price_bkt':>11} {'n':>5} {'P(win)':>8} {'avg_px':>7} {'EV/$1':>8} {'EV%':>7}")
        for T in CHECKPOINTS:
            for lo, hi in zip(PRICE_EDGES[:-1], PRICE_EDGES[1:]):
                g = [r for r in sub if r[0] == T and lo <= r[1] < hi]
                if len(g) < 10:
                    continue
                pw = np.mean([r[2] for r in g])
                ap = np.mean([r[1] for r in g])
                ev = pw - ap
                print(f"  {T:>5.0f} {f'{lo:.2f}-{hi:.2f}':>11} {len(g):>5} {pw:>8.3f} {ap:>7.3f} {ev:>+8.3f} {100*ev/ap:>+6.1f}%")

    report('ALL', rows)
    report('LOW VOL (<= median; the regime br2 SKIPS)', [r for r in rows if r[3] is not None and r[3] <= vmed])
    report('HIGH VOL (> median)', [r for r in rows if r[3] is not None and r[3] > vmed])

    # headline: pooled EV for strong late favourites (price>=0.80, T<=20s)
    strong = [r for r in rows if r[0] <= 20 and r[1] >= 0.80]
    if strong:
        pw = np.mean([r[2] for r in strong]); ap = np.mean([r[1] for r in strong])
        print(f"\nHEADLINE strong-late-fav (px>=0.80, T<=20s): n={len(strong)} P(win)={pw:.3f} avg_px={ap:.3f} EV/$1={pw-ap:+.3f} ({100*(pw-ap)/ap:+.1f}%)")
        lowstrong = [r for r in strong if r[3] is not None and r[3] <= vmed]
        if lowstrong:
            pw2 = np.mean([r[2] for r in lowstrong]); ap2 = np.mean([r[1] for r in lowstrong])
            print(f"  low-vol subset: n={len(lowstrong)} P(win)={pw2:.3f} avg_px={ap2:.3f} EV/$1={pw2-ap2:+.3f} ({100*(pw2-ap2)/ap2:+.1f}%)")


if __name__ == '__main__':
    main()
