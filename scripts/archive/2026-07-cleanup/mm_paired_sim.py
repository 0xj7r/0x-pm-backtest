"""Queue-aware PAIRED market-making simulator for Polymarket BTC-5m (calm regime).

Realistic, conservative backtest of a two-sided matched-pair maker. The engine's
maker fill model has NO queue and overstates fills, so this works from the TAPE:

  - A resting YES bid fills only when a real taker SELL prints at/below our price.
  - A resting YES ask fills only when a real taker BUY  prints at/above our price.
  - Fill quantity is pro-rata by clip/(clip + shares_ahead), where shares_ahead is
    the resting top-of-book depth at our price from the book snapshot.
  - NO is the mirror of YES (single mirrored book, NO=1-YES). A YES short == a NO
    long, so two-sided YES quoting already accumulates both legs.

STRICT PAIRING: we accumulate long-YES and short-YES (==long-NO) inventory. Matched
pairs redeem to $1 at expiry. P&L is the round-trip: sum(redeemed pairs * (1 - pair_cost))
+ rebate, minus the residual (unmatched leg) marked to the realized outcome. We never
trade out of the residual at a loss; it resolves at the BTC up/down outcome (from Binance).

REGIME GATE (quote-gate): only quote a market when, over the data so far, spot vol is
low, the YES range is narrow, sign-flips are healthy (oscillation not whipsaw) and mid
sits in ~0.30-0.70. DYNAMIC GATES (tick): pull the exposed side when Binance spot
accelerates, pull in the last window, pull on large taker prints.

Outcome (YES wins) is derived from Binance BTCUSDT spot: close_px > start_px over the
5-minute window [close-300, close].

Usage:
  python3 scripts/mm_paired_sim.py            # full run, all clips/rebate, gated vs ungated
  python3 scripts/mm_paired_sim.py --limit 400  # quick subset for tuning
"""
import argparse
import glob
import math
import os
import sys

import numpy as np
import pyarrow.parquet as pq

BOOK_ROOT = 'data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25'
TRADE_ROOT = 'data/cache/raw/telonex/exchange=polymarket/channel=trades'
BINANCE_ROOT = 'data/cache/raw/binance/exchange=binance/channel=agg_trades/symbol=BTCUSDT'

ACTIVE_WIN = 300.0          # quote over the last 5 minutes
WINDOW = 300.0              # 5m up/down window for resolution

# Regime classifier thresholds (tuned on data; see report).
REGIME_MID_LO, REGIME_MID_HI = 0.30, 0.70
REGIME_RANGE_MAX = 0.06     # YES mid range-so-far must be <= 6c (p90 of calm markets)
REGIME_SPOT_VOL_MAX = 0.00012  # max stdev of 30s spot returns (5s grid); ~p75 of universe
REGIME_FLIP_MIN = 0.20      # min fraction of spot-return sign flips (oscillation, not drift)
REGIME_WARMUP = 60.0        # need >=60s of history before quoting

# Dynamic gates.
LATE_PULL_SECS = 45.0       # pull both legs in the last N seconds
SPOT_ACCEL_PULL = 0.0008    # pull exposed side if |30s spot return| exceeds this (~p97)
LARGE_TAKER_SHARES = 150.0  # pull exposed side after a taker print this large

# Maker economics.
TAKER_FEE_FRAC = 0.0156     # ~1.56% taker fee
REBATE_FRAC = 0.20 * TAKER_FEE_FRAC  # maker rebate ~20% of taker fee, on notional
RESIDUAL_CAP_FRAC = 0.05    # target: unmatched residual <= 5% of paired volume
REPAIR_DELTA_SHARES = 2.0   # if |yes_long-no_long| exceeds this, quote only the re-pairing leg
TICK = 0.01


def read_parquet(path, cols=None):
    return pq.ParquetFile(path).read(columns=cols).to_pandas()


def load_binance_day(date):
    cand = glob.glob(f'{BINANCE_ROOT}/date={date}/*.parquet')
    if not cand:
        return None
    df = read_parquet(cand[0], ['price', 'transact_time_ms'])
    ts = df.transact_time_ms.values.astype(np.float64) / 1e6   # micros -> seconds
    px = df.price.values.astype(np.float64)
    order = np.argsort(ts, kind='stable')
    return ts[order], px[order]


def binance_outcome_and_vol(bin_day, close):
    """Returns (yes_wins, spot_ts, spot_px) for the market window."""
    if bin_day is None:
        return None, None, None
    ts, px = bin_day
    start = close - WINDOW
    i0 = np.searchsorted(ts, start, side='right') - 1
    i1 = np.searchsorted(ts, close, side='right') - 1
    if i0 < 0 or i1 < 0:
        return None, None, None
    yes_wins = px[i1] > px[i0]
    return yes_wins, ts, px


def load_book(path):
    df = read_parquet(path, ['timestamp_us', 'slug',
                             'bid_price_0', 'bid_size_0', 'ask_price_0', 'ask_size_0'])
    if len(df) == 0:
        return None, None
    slug = df.slug.iloc[0]
    # Polymarket BTC 5m slugs encode the window start timestamp. Resolution is
    # exactly five minutes later.
    close = int(slug.split('-')[-1]) + int(WINDOW)
    t = close - df.timestamp_us.values.astype(np.float64) / 1e6
    bp = np.asarray(df.bid_price_0, dtype=np.float64)
    ap = np.asarray(df.ask_price_0, dtype=np.float64)
    bs = np.asarray(df.bid_size_0, dtype=np.float64)
    asz = np.asarray(df.ask_size_0, dtype=np.float64)
    ok = np.isfinite(bp) & np.isfinite(ap) & (t >= 0) & (t <= ACTIVE_WIN)
    bp, ap, bs, asz, t = bp[ok], ap[ok], bs[ok], asz[ok], t[ok]
    if len(t) < 5:
        return None, None
    # sort ascending by wall time (= descending t)
    order = np.argsort(-t, kind='stable')
    return {
        'wall': (-t)[order],   # ascending wall seconds (negative = early)
        't': t[order],         # seconds-to-close (descending)
        'bid': bp[order], 'ask': ap[order],
        'bsz': bs[order], 'asz': asz[order],
        'mid': ((bp + ap) / 2)[order],
    }, (slug, close)


def trade_path_for(book_path):
    parts = book_path.split('/')
    date = [p for p in parts if p.startswith('date=')][0]
    asset = [p for p in parts if p.startswith('asset_id=')][0]
    cand = glob.glob(f'{TRADE_ROOT}/{date}/{asset}/*.parquet')
    return cand[0] if cand else None


def load_trades(path, close):
    df = read_parquet(path, ['timestamp_us', 'price', 'size', 'side'])
    if len(df) == 0:
        return None
    t = close - df.timestamp_us.values.astype(np.float64) / 1e6
    p = np.asarray(df.price, dtype=np.float64)
    sz = np.asarray(df['size'], dtype=np.float64)
    side = df.side.values.astype(object)
    ok = np.isfinite(p) & np.isfinite(sz) & (t >= 0) & (t <= ACTIVE_WIN)
    p, sz, side, t = p[ok], sz[ok], side[ok], t[ok]
    order = np.argsort(-t, kind='stable')   # ascending wall time
    return {'t': t[order], 'wall': (-t)[order], 'p': p[order],
            'sz': sz[order], 'side': side[order]}


def spot_metrics(spot_ts, spot_px, abs_lo, abs_hi):
    """Stdev of 30s spot returns (5s grid) and sign-flip fraction over [abs_lo, abs_hi].

    Uses absolute unix-second bounds. A 5s sampling grid makes the vol measure
    discriminate calm vs active windows (raw per-tick stdev is too granular).
    The sign-flip fraction on the gridded returns measures micro-oscillation
    (healthy two-sided flow) vs one-directional drift (whipsaw risk).
    """
    i0 = np.searchsorted(spot_ts, abs_lo, side='left')
    i1 = np.searchsorted(spot_ts, abs_hi, side='right')
    if i1 - i0 < 10:
        return 0.0, 0.0
    seg_t = spot_ts[i0:i1]
    grid = np.arange(seg_t[0], seg_t[-1], 5.0)
    if len(grid) < 8:
        return 0.0, 0.0
    gp = spot_px[np.searchsorted(spot_ts, grid, side='right') - 1]
    rets = np.diff(gp) / gp[:-1]
    vol = float(np.std(rets))
    signs = np.sign(rets)
    nz = signs[signs != 0]
    flips = float(np.mean(nz[1:] != nz[:-1])) if len(nz) > 1 else 0.0
    return vol, flips


def spot_return_30s(spot_ts, spot_px, wall_now):
    i1 = np.searchsorted(spot_ts, wall_now, side='right') - 1
    i0 = np.searchsorted(spot_ts, wall_now - 30.0, side='right') - 1
    if i0 < 0 or i1 < 0 or i0 == i1:
        return 0.0
    return (spot_px[i1] - spot_px[i0]) / spot_px[i0]


def simulate_market(book, close, trades, bin_day, clip, regime_gated, rebate_on):
    """One market. Returns a dict of per-market results or None if unquotable."""
    yes_wins, spot_ts, spot_px = binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None

    wall = book['wall']; tsec = book['t']
    bid = book['bid']; ask = book['ask']
    bsz = book['bsz']; asz = book['asz']
    mid = book['mid']
    n = len(wall)
    wall0 = wall[0]

    # range-so-far per snapshot
    run_min = np.minimum.accumulate(mid)
    run_max = np.maximum.accumulate(mid)

    def snap_idx(wq):
        i = np.searchsorted(wall, wq, side='right') - 1
        return max(i, 0)

    # accumulate inventory across taker prints
    yes_long = 0.0   # shares of YES held long (bought our resting bid)
    no_long = 0.0    # shares of NO held long == YES short (sold our resting ask)
    yes_long_cost = 0.0  # $ paid for YES
    no_long_cost = 0.0   # $ paid for NO (price = 1 - our_ask_yes)
    rebate_usdc = 0.0
    n_fills = 0
    filled_shares = 0.0
    # adverse tail bookkeeping (mark each fill 5s out)
    adverse_5s = []

    market_quotable_any = False

    for k in range(len(trades['t'])):
        t = trades['t'][k]
        wq = trades['wall'][k]
        side = trades['side'][k]
        tp = trades['p'][k]
        tsz = trades['sz'][k]

        si = snap_idx(wq)
        b = bid[si]; a = ask[si]; m = mid[si]
        if not (math.isfinite(b) and math.isfinite(a)):
            continue
        spread = a - b
        if spread <= 0:
            continue

        # warmup: need history before quoting
        if (wq - wall0) < REGIME_WARMUP:
            continue

        # ---- regime quote-gate (evaluated on data SO FAR, no lookahead) ----
        if regime_gated:
            if not (REGIME_MID_LO <= m <= REGIME_MID_HI):
                continue
            rng = run_max[si] - run_min[si]
            if rng > REGIME_RANGE_MAX:
                continue
            vol, flips = spot_metrics(spot_ts, spot_px, close + wall0, close + wq)
            if vol > REGIME_SPOT_VOL_MAX:
                continue
            if flips < REGIME_FLIP_MIN:
                continue
        market_quotable_any = True

        # ---- dynamic gates: which legs are live this tick ----
        bid_live = True   # our resting YES bid (we buy YES)
        ask_live = True   # our resting YES ask (we sell YES == buy NO)

        # late-window pull: pull both
        if t <= LATE_PULL_SECS:
            bid_live = ask_live = False

        # spot-lead pull: if spot accelerating up, our YES ask is about to be
        # picked off (YES will rise) -> pull the ask; symmetric for the bid.
        sr = spot_return_30s(spot_ts, spot_px, close + wq)
        if sr > SPOT_ACCEL_PULL:
            ask_live = False
        elif sr < -SPOT_ACCEL_PULL:
            bid_live = False

        # large-taker toxicity pull: a big print means informed flow; pull the
        # side that print would hit.
        if tsz >= LARGE_TAKER_SHARES:
            if side == 'sell':
                bid_live = False
            else:
                ask_live = False

        # strict-pairing repair: never let one leg outrun the other. If we are
        # long more YES than NO, stop buying YES (pull the bid) and only quote
        # the YES ask (== buy NO) to re-pair; symmetric on the other side. This
        # is the structural residual cap, not a trade-out.
        delta = yes_long - no_long
        if delta > REPAIR_DELTA_SHARES:
            bid_live = False
        elif delta < -REPAIR_DELTA_SHARES:
            ask_live = False

        # ---- conservative tape fill with pro-rata queue ----
        # YES bid at the touch (join best bid); fills on a taker SELL <= our bid.
        if bid_live and side == 'sell' and tp <= b + 1e-9:
            ahead = max(bsz[si], 0.0)
            frac = clip / (clip + ahead)
            qty = min(clip, tsz * frac)
            # do not let this fill push yes ahead of no by more than the repair band
            qty = min(qty, max(0.0, no_long + REPAIR_DELTA_SHARES - yes_long))
            if qty > 1e-9:
                yes_long += qty
                yes_long_cost += qty * b
                filled_shares += qty
                n_fills += 1
                rebate_usdc += qty * b * REBATE_FRAC
                # adverse mark 5s later
                fj = snap_idx(wq + 5.0)
                adverse_5s.append((+1) * (mid[fj] - m) * qty)

        # YES ask at the touch; fills on a taker BUY >= our ask. Selling YES at a
        # == buying NO at (1 - a).
        if ask_live and side == 'buy' and tp >= a - 1e-9:
            ahead = max(asz[si], 0.0)
            frac = clip / (clip + ahead)
            qty = min(clip, tsz * frac)
            qty = min(qty, max(0.0, yes_long + REPAIR_DELTA_SHARES - no_long))
            if qty > 1e-9:
                no_long += qty
                no_long_cost += qty * (1.0 - a)
                filled_shares += qty
                n_fills += 1
                rebate_usdc += qty * (1.0 - a) * REBATE_FRAC
                fj = snap_idx(wq + 5.0)
                adverse_5s.append((-1) * (mid[fj] - m) * qty)

    if not market_quotable_any:
        return None

    # ---- strict pairing + redemption ----
    paired = min(yes_long, no_long)
    pnl_pairs = 0.0
    pair_cost_total = 0.0
    if paired > 0:
        yes_avg = yes_long_cost / yes_long if yes_long > 0 else 0.0
        no_avg = no_long_cost / no_long if no_long > 0 else 0.0
        pair_cost = yes_avg + no_avg
        pair_cost_total = pair_cost
        # each matched pair redeems to $1
        pnl_pairs = paired * (1.0 - pair_cost)

    # residual (unmatched leg): NEVER traded out; resolves at outcome.
    res_yes = yes_long - paired   # extra long YES
    res_no = no_long - paired     # extra long NO
    res_shares = res_yes + res_no
    yes_avg = yes_long_cost / yes_long if yes_long > 0 else 0.0
    no_avg = no_long_cost / no_long if no_long > 0 else 0.0
    pnl_resid = 0.0
    if res_yes > 0:
        payoff = 1.0 if yes_wins else 0.0
        pnl_resid += res_yes * (payoff - yes_avg)
    if res_no > 0:
        payoff = 0.0 if yes_wins else 1.0   # NO wins when YES loses
        pnl_resid += res_no * (payoff - no_avg)

    rebate = rebate_usdc if rebate_on else 0.0
    net = pnl_pairs + pnl_resid + rebate

    total_leg = yes_long + no_long
    resid_frac = res_shares / total_leg if total_leg > 0 else 0.0

    return {
        'slug_close': close,
        'yes_wins': yes_wins,
        'n_fills': n_fills,
        'filled_shares': filled_shares,
        'yes_long': yes_long, 'no_long': no_long,
        'paired': paired, 'res_shares': res_shares, 'resid_frac': resid_frac,
        'pnl_pairs': pnl_pairs, 'pnl_resid': pnl_resid, 'rebate': rebate,
        'net': net,
        'pair_cost': pair_cost_total,
        'adverse_5s_sum': float(np.sum(adverse_5s)) if adverse_5s else 0.0,
        'adverse_5s_min': float(np.min(adverse_5s)) if adverse_5s else 0.0,
    }


def load_all_markets(book_files, bin_cache):
    """Parse every market once: (book, close, trades, bin_day, date). Reused across configs."""
    parsed = []
    for bf in book_files:
        try:
            book, meta = load_book(bf)
        except Exception:
            continue
        if book is None:
            continue
        slug, close = meta
        date = [p for p in bf.split('/') if p.startswith('date=')][0].split('=')[1]
        bin_day = bin_cache.get(date)
        if bin_day is None:
            bin_day = load_binance_day(date)
            bin_cache[date] = bin_day
        tpath = trade_path_for(bf)
        if not tpath:
            continue
        try:
            trades = load_trades(tpath, close)
        except Exception:
            continue
        if trades is None or len(trades['t']) == 0:
            continue
        parsed.append((book, close, trades, bin_day, date))
    return parsed


def run(parsed, clip, regime_gated, rebate_on):
    rows = []
    for book, close, trades, bin_day, date in parsed:
        r = simulate_market(book, close, trades, bin_day, clip, regime_gated, rebate_on)
        if r is None:
            continue
        r['date'] = date
        rows.append(r)
    return rows


def summarize(rows, label, n_days):
    if not rows:
        return {'label': label, 'n_markets': 0}
    net = np.array([r['net'] for r in rows])
    fills = np.array([r['n_fills'] for r in rows])
    fshares = np.array([r['filled_shares'] for r in rows])
    resid = np.array([r['resid_frac'] for r in rows])
    pnl_pairs = np.array([r['pnl_pairs'] for r in rows])
    pnl_resid = np.array([r['pnl_resid'] for r in rows])
    rebate = np.array([r['rebate'] for r in rows])
    adv_min = np.array([r['adverse_5s_min'] for r in rows])
    total_shares = fshares.sum()
    return {
        'label': label,
        'n_markets': len(rows),
        'net_total': float(net.sum()),
        'net_per_day': float(net.sum() / n_days),
        'net_per_market': float(net.mean()),
        'net_per_share_c': float(net.sum() / total_shares * 100) if total_shares > 0 else 0.0,
        'pct_markets_pos': float((net > 0).mean() * 100),
        'fills_per_market': float(fills.mean()),
        'shares_per_market': float(fshares.mean()),
        'mean_resid_frac': float(resid.mean()),
        'pnl_pairs_total': float(pnl_pairs.sum()),
        'pnl_resid_total': float(pnl_resid.sum()),
        'rebate_total': float(rebate.sum()),
        'adverse_tail_p1_c': float(np.percentile(adv_min, 1) * 100) if len(adv_min) else 0.0,
        'worst_market_net': float(net.min()),
    }


def fmt_row(s):
    if s['n_markets'] == 0:
        return f"  {s['label']:<28} (no quotable markets)"
    return (f"  {s['label']:<28} mkts={s['n_markets']:5d} "
            f"net=${s['net_total']:8.2f} ${s['net_per_day']:7.2f}/day "
            f"{s['net_per_share_c']:+.3f}c/sh pos={s['pct_markets_pos']:4.1f}% "
            f"resid={s['mean_resid_frac']*100:4.1f}% "
            f"pairs=${s['pnl_pairs_total']:7.1f} res=${s['pnl_resid_total']:7.1f} "
            f"reb=${s['rebate_total']:6.1f} worst=${s['worst_market_net']:.1f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--limit', type=int, default=0, help='limit book files (for tuning)')
    ap.add_argument('--clips', type=str, default='5,10,20')
    ap.add_argument('--oos', action='store_true', help='split IS May7-15 / OOS May16-20')
    args = ap.parse_args()

    book_files = sorted(glob.glob(f'{BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    if args.limit:
        # stride sample to keep day coverage
        step = max(1, len(book_files) // args.limit)
        book_files = book_files[::step][:args.limit]
    dates = sorted({[p for p in f.split('/') if p.startswith('date=')][0].split('=')[1]
                    for f in book_files})
    n_days = len(dates)
    clips = [int(x) for x in args.clips.split(',')]
    print(f'book files: {len(book_files)}  days: {n_days} ({dates[0]}..{dates[-1]})',
          file=sys.stderr)

    bin_cache = {}
    print('loading + parsing all markets once...', file=sys.stderr, flush=True)
    parsed = load_all_markets(book_files, bin_cache)
    print(f'parsed {len(parsed)} markets with trades', file=sys.stderr, flush=True)

    print('=' * 100, flush=True)
    print('QUEUE-AWARE PAIRED-MM SIMULATOR  (conservative tape+queue fills, round-trip redemption)')
    print(f'days={n_days}  rebate(on)={REBATE_FRAC*100:.3f}% of notional (~{REBATE_FRAC*0.5*100:.3f}c/sh @ px0.5)')
    print('=' * 100, flush=True)

    for clip in clips:
        print(f'\n#### CLIP = {clip} shares ' + '#' * 60, flush=True)
        for rebate_on in (False, True):
            rtag = 'rebate' if rebate_on else 'norebate'
            for gated in (True, False):
                gtag = 'GATED' if gated else 'ungated'
                rows = run(parsed, clip, gated, rebate_on)
                s = summarize(rows, f'{gtag}/{rtag}', n_days)
                print(fmt_row(s), flush=True)

    if args.oos:
        print('\n' + '=' * 100, flush=True)
        print('OOS SPLIT  (IS = May 07-15, OOS = May 16-20)  [caveat: same regime month, not Feb-Apr]')
        print('=' * 100, flush=True)
        is_dates = {f'2026-05-{d:02d}' for d in range(7, 16)}
        oos_dates = {f'2026-05-{d:02d}' for d in range(16, 21)}
        is_parsed = [p for p in parsed if p[4] in is_dates]
        oos_parsed = [p for p in parsed if p[4] in oos_dates]
        for split, sub in (('IS', is_parsed), ('OOS', oos_parsed)):
            nd = len({p[4] for p in sub})
            for gated in (True, False):
                rows = run(sub, 10, gated, False)
                s = summarize(rows, f'{split} clip10 {"GATED" if gated else "ungated"} norebate', nd)
                print(fmt_row(s), flush=True)


if __name__ == '__main__':
    main()
