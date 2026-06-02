"""Queue-aware PAIRED LADDER market-making simulator for Polymarket BTC-5m (calm regime).

Extends scripts/mm_paired_sim.py (touch-only) with configurable QUOTING DEPTH
(laddering) and a Binance-lead gate on the DEEP levels, to test the design
question: does laddering deeper capture more oscillation in a range-bound market,
and does the spot-lead gate neutralize the reversal risk that depth introduces?

LADDER: N levels per side. Level j (0=touch) rests at price (b - j*TICK) on the
bid and (a + j*TICK) on the ask, with size LEVEL_SIZE[j] * clip (decreasing deeper).
Each level fills independently on a real taker cross at/through its price, pro-rata
by clip_j/(clip_j + depth_ahead_at_that_level), where depth_ahead is read from the
25-level book snapshot at the matching price level. Touch-only (N=1) reproduces the
baseline sim exactly.

BINANCE-LEAD GATE ON DEEP LEVELS (j>=1): only rest deep bid levels when spot is NOT
falling, only rest deep ask levels when spot is NOT rising (trailing 30s spot return
vs DEEP_SPOT_GATE). The hypothesis: a buyable dip (PM-YES wobbling, spot range-bound)
keeps the deep bid live to buy the dip; a falling knife (PM-YES falling AND spot
falling with it) pulls the deep bid the instant spot accelerates down -> we do not
ladder into the knife. Touch level (j=0) is always governed by the existing gates
(late-pull, spot-accel, large-taker, repair) -- the ladder gate only adds/removes the
DEEP rungs.

Everything else (strict pairing, hard residual cap, inventory-skew repair, late-window
pull, conservative tape+queue fill, single mirrored book, Binance-derived outcome) is
inherited unchanged from the touch-only sim.

DECOMPOSITION: per config we record fills tagged by level so we can split
  - oscillation-capture uplift = extra gross spread from deep (j>=1) fills vs touch-only
  - reversal/stranding cost     = residual losses + adverse tail attributable to deep fills
and break out CALM vs WHIPSAW markets separately.

Usage:
  python3 scripts/mm_paired_ladder_sim.py            # full sweep
  python3 scripts/mm_paired_ladder_sim.py --limit 400  # quick subset
"""
import argparse
import glob
import math
import os
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import mm_paired_sim as base  # reuse loaders + constants

TICK = base.TICK
ACTIVE_WIN = base.ACTIVE_WIN
WINDOW = base.WINDOW
TAKER_FEE_FRAC = base.TAKER_FEE_FRAC
REBATE_FRAC = base.REBATE_FRAC
REPAIR_DELTA_SHARES = base.REPAIR_DELTA_SHARES
LATE_PULL_SECS = base.LATE_PULL_SECS
SPOT_ACCEL_PULL = base.SPOT_ACCEL_PULL
LARGE_TAKER_SHARES = base.LARGE_TAKER_SHARES
REGIME_WARMUP = base.REGIME_WARMUP

# Ladder per-level size as a fraction of the clip (decreasing deeper).
LEVEL_SIZE = [1.00, 0.60, 0.40]

# Deep-level spot gate: deep bid pulled when trailing 30s spot return < -this
# (spot falling = potential knife); deep ask pulled when > +this. Looser than the
# touch-level SPOT_ACCEL_PULL because deep rungs are the ones we most want to keep
# OUT of a developing move, but we still want them live in genuine oscillation.
DEEP_SPOT_GATE = 0.0003


def load_book_laddered(path):
    """Like base.load_book but also returns 25-level price/size arrays for queue lookup."""
    nlv = 3  # only the first 3 price levels are ever used by the ladder (touch + 2 deep)
    cols = ['timestamp_us', 'slug']
    for j in range(nlv):
        cols += [f'bid_price_{j}', f'bid_size_{j}', f'ask_price_{j}', f'ask_size_{j}']
    df = base.read_parquet(path, cols)
    if len(df) == 0:
        return None, None
    slug = df.slug.iloc[0]
    # Polymarket BTC 5m slugs encode the window start timestamp. Resolution is
    # exactly five minutes later.
    close = int(slug.split('-')[-1]) + int(WINDOW)
    t = close - df.timestamp_us.values.astype(np.float64) / 1e6
    bp = np.asarray(df.bid_price_0, dtype=np.float64)
    ap = np.asarray(df.ask_price_0, dtype=np.float64)
    bsz = np.asarray(df.bid_size_0, dtype=np.float64)
    asz = np.asarray(df.ask_size_0, dtype=np.float64)
    ok = np.isfinite(bp) & np.isfinite(ap) & (t >= 0) & (t <= ACTIVE_WIN)
    bid_lv = np.stack([np.asarray(df[f'bid_price_{j}'], dtype=np.float64) for j in range(nlv)], axis=1)
    bsz_lv = np.stack([np.asarray(df[f'bid_size_{j}'], dtype=np.float64) for j in range(nlv)], axis=1)
    ask_lv = np.stack([np.asarray(df[f'ask_price_{j}'], dtype=np.float64) for j in range(nlv)], axis=1)
    asz_lv = np.stack([np.asarray(df[f'ask_size_{j}'], dtype=np.float64) for j in range(nlv)], axis=1)
    bp, ap, bsz, asz = bp[ok], ap[ok], bsz[ok], asz[ok]
    t = t[ok]
    bid_lv, bsz_lv, ask_lv, asz_lv = bid_lv[ok], bsz_lv[ok], ask_lv[ok], asz_lv[ok]
    if len(t) < 5:
        return None, None
    order = np.argsort(-t, kind='stable')
    return {
        'wall': (-t)[order], 't': t[order],
        'bid': bp[order], 'ask': ap[order], 'bsz': bsz[order], 'asz': asz[order],
        'mid': ((bp + ap) / 2)[order],
        'bid_lv': bid_lv[order], 'bsz_lv': bsz_lv[order],
        'ask_lv': ask_lv[order], 'asz_lv': asz_lv[order],
    }, (slug, close)


def depth_at_price(prices_row, sizes_row, target, is_bid):
    """Resting depth at the book level whose price equals target (within half a tick).
    If no level matches the target price, the rung is in empty book space; return 0
    (no queue ahead -> but also means the price may be inside the spread/illiquid).
    """
    for j in range(prices_row.shape[0]):
        p = prices_row[j]
        if math.isfinite(p) and abs(p - target) < TICK / 2:
            s = sizes_row[j]
            return s if math.isfinite(s) else 0.0
    return 0.0


def classify_calm(book, close, spot_ts, spot_px):
    """Per-market calm vs whipsaw using FULL-window spot behaviour (post-hoc label,
    used only for breakout reporting, NOT as a live gate). Whipsaw = a market whose
    spot path over the window is high-vol AND directional (low sign-flip) -> a real
    move / knife, the dangerous kind. Calm = low-vol or oscillatory.
    """
    if spot_ts is None:
        return 'calm'
    vol, flips = base.spot_metrics(spot_ts, spot_px, close - WINDOW, close)
    # whipsaw: elevated vol with one-directional drift (the knife)
    if vol > base.REGIME_SPOT_VOL_MAX and flips < base.REGIME_FLIP_MIN:
        return 'whipsaw'
    # also flag big spot moves regardless of flips (large net displacement)
    ts, px = spot_ts, spot_px
    i0 = np.searchsorted(ts, close - WINDOW, side='left')
    i1 = np.searchsorted(ts, close, side='right') - 1
    if i0 < len(px) and 0 <= i1 < len(px) and i0 <= i1:
        net = abs(px[i1] - px[i0]) / px[i0]
        if net > 0.0015:  # >0.15% net move over 5m
            return 'whipsaw'
    return 'calm'


def simulate_market(book, close, trades, bin_day, clip, n_levels, deep_gate, rebate_on):
    yes_wins, spot_ts, spot_px = base.binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None

    wall = book['wall']; tsec = book['t']
    bid = book['bid']; ask = book['ask']
    bsz = book['bsz']; asz = book['asz']
    mid = book['mid']
    bid_lv = book['bid_lv']; bsz_lv = book['bsz_lv']
    ask_lv = book['ask_lv']; asz_lv = book['asz_lv']
    wall0 = wall[0]
    run_min = np.minimum.accumulate(mid)
    run_max = np.maximum.accumulate(mid)

    def snap_idx(wq):
        i = np.searchsorted(wall, wq, side='right') - 1
        return max(i, 0)

    yes_long = 0.0; no_long = 0.0
    yes_long_cost = 0.0; no_long_cost = 0.0
    rebate_usdc = 0.0
    n_fills = 0; filled_shares = 0.0
    adverse_5s = []
    # per-level decomposition: gross spread captured at fill (mid - price for bid,
    # price - mid for ask = half-spread+ proxy) and shares, tagged touch(0) vs deep(>=1)
    gross_touch = 0.0; gross_deep = 0.0
    shares_touch = 0.0; shares_deep = 0.0
    adverse_deep = []
    deep_resid_yes = 0.0; deep_resid_no = 0.0  # shares of deep-origin inventory (for attribution)

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
        if a - b <= 0:
            continue
        if (wq - wall0) < REGIME_WARMUP:
            continue
        market_quotable_any = True

        bid_live = True; ask_live = True
        if t <= LATE_PULL_SECS:
            bid_live = ask_live = False
        sr = base.spot_return_30s(spot_ts, spot_px, close + wq)
        if sr > SPOT_ACCEL_PULL:
            ask_live = False
        elif sr < -SPOT_ACCEL_PULL:
            bid_live = False
        if tsz >= LARGE_TAKER_SHARES:
            if side == 'sell':
                bid_live = False
            else:
                ask_live = False
        delta = yes_long - no_long
        if delta > REPAIR_DELTA_SHARES:
            bid_live = False
        elif delta < -REPAIR_DELTA_SHARES:
            ask_live = False

        # deep-level spot gate (only affects rungs j>=1)
        deep_bid_ok = True; deep_ask_ok = True
        if deep_gate:
            if sr < -DEEP_SPOT_GATE:   # spot falling -> do not ladder the bid into it
                deep_bid_ok = False
            if sr > DEEP_SPOT_GATE:    # spot rising -> do not ladder the ask into it
                deep_ask_ok = False

        # ---- BID ladder: resting YES bids at b, b-1c, b-2c. Fills on taker SELL. ----
        if bid_live and side == 'sell':
            for j in range(n_levels):
                if j >= 1 and (not deep_bid_ok):
                    continue
                price = b - j * TICK
                if price <= 0:
                    continue
                if tp > price + 1e-9:   # taker sell did not reach this level
                    continue
                ahead = depth_at_price(bid_lv[si], bsz_lv[si], price, True)
                clip_j = clip * LEVEL_SIZE[j]
                frac = clip_j / (clip_j + max(ahead, 0.0))
                qty = min(clip_j, tsz * frac)
                qty = min(qty, max(0.0, no_long + REPAIR_DELTA_SHARES - yes_long))
                if qty <= 1e-9:
                    continue
                yes_long += qty
                yes_long_cost += qty * price
                filled_shares += qty
                n_fills += 1
                rebate_usdc += qty * price * REBATE_FRAC
                fj = snap_idx(wq + 5.0)
                adv = (+1) * (mid[fj] - m) * qty
                adverse_5s.append(adv)
                g = (m - price) * qty
                if j == 0:
                    gross_touch += g; shares_touch += qty
                else:
                    gross_deep += g; shares_deep += qty
                    adverse_deep.append(adv); deep_resid_yes += qty

        # ---- ASK ladder: resting YES asks at a, a+1c, a+2c. Fills on taker BUY. ----
        if ask_live and side == 'buy':
            for j in range(n_levels):
                if j >= 1 and (not deep_ask_ok):
                    continue
                price = a + j * TICK
                if price >= 1:
                    continue
                if tp < price - 1e-9:
                    continue
                ahead = depth_at_price(ask_lv[si], asz_lv[si], price, False)
                clip_j = clip * LEVEL_SIZE[j]
                frac = clip_j / (clip_j + max(ahead, 0.0))
                qty = min(clip_j, tsz * frac)
                qty = min(qty, max(0.0, yes_long + REPAIR_DELTA_SHARES - no_long))
                if qty <= 1e-9:
                    continue
                no_long += qty
                no_long_cost += qty * (1.0 - price)
                filled_shares += qty
                n_fills += 1
                rebate_usdc += qty * (1.0 - price) * REBATE_FRAC
                fj = snap_idx(wq + 5.0)
                adv = (-1) * (mid[fj] - m) * qty
                adverse_5s.append(adv)
                g = (price - m) * qty
                if j == 0:
                    gross_touch += g; shares_touch += qty
                else:
                    gross_deep += g; shares_deep += qty
                    adverse_deep.append(adv); deep_resid_no += qty

    if not market_quotable_any:
        return None

    paired = min(yes_long, no_long)
    pnl_pairs = 0.0; pair_cost_total = 0.0
    if paired > 0:
        yes_avg = yes_long_cost / yes_long if yes_long > 0 else 0.0
        no_avg = no_long_cost / no_long if no_long > 0 else 0.0
        pair_cost = yes_avg + no_avg
        pair_cost_total = pair_cost
        pnl_pairs = paired * (1.0 - pair_cost)

    res_yes = yes_long - paired
    res_no = no_long - paired
    res_shares = res_yes + res_no
    yes_avg = yes_long_cost / yes_long if yes_long > 0 else 0.0
    no_avg = no_long_cost / no_long if no_long > 0 else 0.0
    pnl_resid = 0.0
    if res_yes > 0:
        payoff = 1.0 if yes_wins else 0.0
        pnl_resid += res_yes * (payoff - yes_avg)
    if res_no > 0:
        payoff = 0.0 if yes_wins else 1.0
        pnl_resid += res_no * (payoff - no_avg)

    rebate = rebate_usdc if rebate_on else 0.0
    net = pnl_pairs + pnl_resid + rebate
    total_leg = yes_long + no_long
    resid_frac = res_shares / total_leg if total_leg > 0 else 0.0

    return {
        'slug_close': close, 'yes_wins': yes_wins,
        'n_fills': n_fills, 'filled_shares': filled_shares,
        'yes_long': yes_long, 'no_long': no_long,
        'paired': paired, 'res_shares': res_shares, 'resid_frac': resid_frac,
        'pnl_pairs': pnl_pairs, 'pnl_resid': pnl_resid, 'rebate': rebate, 'net': net,
        'pair_cost': pair_cost_total,
        'adverse_5s_sum': float(np.sum(adverse_5s)) if adverse_5s else 0.0,
        'adverse_5s_min': float(np.min(adverse_5s)) if adverse_5s else 0.0,
        'gross_touch': gross_touch, 'gross_deep': gross_deep,
        'shares_touch': shares_touch, 'shares_deep': shares_deep,
        'adverse_deep_sum': float(np.sum(adverse_deep)) if adverse_deep else 0.0,
        'adverse_deep_min': float(np.min(adverse_deep)) if adverse_deep else 0.0,
        'deep_resid': deep_resid_yes + deep_resid_no,
    }


def load_all_markets(book_files, bin_cache):
    parsed = []
    for bf in book_files:
        try:
            book, meta = load_book_laddered(bf)
        except Exception:
            continue
        if book is None:
            continue
        slug, close = meta
        date = [p for p in bf.split('/') if p.startswith('date=')][0].split('=')[1]
        bin_day = bin_cache.get(date)
        if bin_day is None:
            bin_day = base.load_binance_day(date)
            bin_cache[date] = bin_day
        tpath = base.trade_path_for(bf)
        if not tpath:
            continue
        try:
            trades = base.load_trades(tpath, close)
        except Exception:
            continue
        if trades is None or len(trades['t']) == 0:
            continue
        regime = classify_calm(book, close, *base.binance_outcome_and_vol(bin_day, close)[1:])
        parsed.append((book, close, trades, bin_day, date, regime))
    return parsed


def run(parsed, clip, n_levels, deep_gate, rebate_on, regime_filter=None):
    rows = []
    for book, close, trades, bin_day, date, regime in parsed:
        if regime_filter is not None and regime != regime_filter:
            continue
        r = simulate_market(book, close, trades, bin_day, clip, n_levels, deep_gate, rebate_on)
        if r is None:
            continue
        r['date'] = date; r['regime'] = regime
        rows.append(r)
    return rows


def summarize(rows, label, n_days):
    if not rows:
        return {'label': label, 'n_markets': 0}
    g = lambda k: np.array([r[k] for r in rows], dtype=np.float64)
    net = g('net'); fshares = g('filled_shares')
    total_shares = fshares.sum()
    return {
        'label': label, 'n_markets': len(rows),
        'net_total': float(net.sum()), 'net_per_day': float(net.sum() / n_days),
        'net_per_market': float(net.mean()),
        'net_per_share_c': float(net.sum() / total_shares * 100) if total_shares > 0 else 0.0,
        'pct_markets_pos': float((net > 0).mean() * 100),
        'shares_per_market': float(fshares.mean()),
        'mean_resid_frac': float(g('resid_frac').mean()),
        'pnl_pairs_total': float(g('pnl_pairs').sum()),
        'pnl_resid_total': float(g('pnl_resid').sum()),
        'rebate_total': float(g('rebate').sum()),
        'worst_market_net': float(net.min()),
        'adverse_tail_p1_c': float(np.percentile(g('adverse_5s_min'), 1) * 100),
        'gross_touch': float(g('gross_touch').sum()),
        'gross_deep': float(g('gross_deep').sum()),
        'shares_deep': float(g('shares_deep').sum()),
        'adverse_deep_sum': float(g('adverse_deep_sum').sum()),
        'adverse_deep_min': float(g('adverse_deep_min').min()) if len(rows) else 0.0,
        'deep_resid': float(g('deep_resid').sum()),
    }


def fmt(s):
    if s['n_markets'] == 0:
        return f"  {s['label']:<34} (no markets)"
    return (f"  {s['label']:<34} mkts={s['n_markets']:5d} "
            f"${s['net_per_day']:7.2f}/day {s['net_per_share_c']:+.3f}c/sh pos={s['pct_markets_pos']:4.1f}% "
            f"resid={s['mean_resid_frac']*100:4.1f}% "
            f"pairs=${s['pnl_pairs_total']:7.1f} res=${s['pnl_resid_total']:7.1f} reb=${s['rebate_total']:6.1f} "
            f"gT=${s['gross_touch']:6.1f} gD=${s['gross_deep']:6.1f} advD=${s['adverse_deep_sum']:+6.2f} "
            f"worst=${s['worst_market_net']:.2f}")


DEPTHS = {'touch': 1, 'L2': 2, 'L3': 3}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--limit', type=int, default=0)
    ap.add_argument('--clip', type=int, default=10)
    args = ap.parse_args()

    book_files = sorted(glob.glob(f'{base.BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    if args.limit:
        step = max(1, len(book_files) // args.limit)
        book_files = book_files[::step][:args.limit]
    dates = sorted({[p for p in f.split('/') if p.startswith('date=')][0].split('=')[1] for f in book_files})
    n_days = len(dates)
    clip = args.clip
    print(f'book files: {len(book_files)}  days: {n_days} ({dates[0]}..{dates[-1]})', file=sys.stderr)

    bin_cache = {}
    print('parsing all markets (25-level book)...', file=sys.stderr, flush=True)
    parsed = load_all_markets(book_files, bin_cache)
    n_calm = sum(1 for p in parsed if p[5] == 'calm')
    n_whip = sum(1 for p in parsed if p[5] == 'whipsaw')
    print(f'parsed {len(parsed)} markets ({n_calm} calm, {n_whip} whipsaw)', file=sys.stderr, flush=True)

    print('=' * 130, flush=True)
    print(f'PAIRED-LADDER MM  clip={clip}sh  level_sizes={LEVEL_SIZE}  deep_spot_gate={DEEP_SPOT_GATE}  '
          f'rebate={REBATE_FRAC*100:.3f}% notional')
    print(f'days={n_days}  markets={len(parsed)} (calm={n_calm}, whipsaw={n_whip})')
    print('gT=gross spread $ touch level | gD=gross spread $ deep levels | advD=adverse-5s $ from deep fills')
    print('=' * 130, flush=True)

    splits = [('ALL', None), ('CALM', 'calm'), ('WHIPSAW', 'whipsaw')]
    for sname, rf in splits:
        nd = n_days
        print(f'\n#### {sname} ' + '#' * 80, flush=True)
        for depth_name, nlv in DEPTHS.items():
            for deep_gate in ([False] if nlv == 1 else [False, True]):
                for rebate_on in (False, True):
                    gtag = 'gate' if deep_gate else 'nogate'
                    rtag = 'reb' if rebate_on else 'noreb'
                    lbl = f'{depth_name}/{gtag}/{rtag}'
                    rows = run(parsed, clip, nlv, deep_gate, rebate_on, regime_filter=rf)
                    s = summarize(rows, lbl, nd)
                    print(fmt(s), flush=True)


if __name__ == '__main__':
    main()
