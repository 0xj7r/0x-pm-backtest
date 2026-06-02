"""Gate-relaxation x residual-cap sweep + mint-then-sell variant for the calm paired-MM.

Resolves the "quote more vs one-sided stranding" tension. Reuses the validated
loaders/fill model from mm_paired_sim.py; reimplements simulate_market to take a
config so the range/mid/flip gates and the residual cap are sweepable, and adds a
mint-then-sell structural variant alongside the current buy-then-pair.

Buy-then-pair (current): rest two-sided YES BUYs (bid = buy YES, ask = sell YES =
buy NO). Pair via fills. When one leg outruns the other we hold a residual long that
resolves at the BTC outcome. The repair band (REPAIR_DELTA_SHARES or residual_cap_frac
of paired volume) bounds how far one leg can outrun the other.

Mint-then-sell: SPLIT $1 -> hold 1 YES + 1 NO (risk-free, redeems to $1). Rest SELL
orders on BOTH legs near mid. Selling YES at a fills on a taker BUY>=a; selling NO at
(1-b) fills on a taker SELL<=b. You are NEVER stranded long: the worst case is you sell
neither leg and redeem the held pair for exactly $1 (zero PnL minus mint working-capital
opportunity cost + fees). Selling a leg is optional spread capture: if you sell YES at a
you keep NO which resolves; net on that unit = a + NO_payoff - 1 (= a - YES_payoff, the
same spread capture as a maker who shorted YES at a). Selling BOTH legs of a minted pair
captures (a + (1-b)) - 1 = (a-b) = the full spread, risk-free, and you've recycled the $1.

Decomposition reported per config: net $/day, gross spread (pairs/oscillation income),
one-sided stranding loss (residual marked to resolution), markets quoted, tick quote
fraction, pairing rate (matched vs residual), worst market.

Usage:
  python3 scripts/mm_paired_gate_cap_sweep.py --limit 400   # quick
  python3 scripts/mm_paired_gate_cap_sweep.py               # full 4000 files
"""
import argparse
import glob
import math
import sys

import numpy as np

import mm_paired_sim as base

ACTIVE_WIN = base.ACTIVE_WIN
WINDOW = base.WINDOW
REBATE_FRAC = base.REBATE_FRAC
REGIME_SPOT_VOL_MAX = base.REGIME_SPOT_VOL_MAX
REGIME_WARMUP = base.REGIME_WARMUP
LATE_PULL_SECS = base.LATE_PULL_SECS
SPOT_ACCEL_PULL = base.SPOT_ACCEL_PULL
LARGE_TAKER_SHARES = base.LARGE_TAKER_SHARES


def simulate(book, close, trades, bin_day, clip, cfg, rebate_on):
    """One market under a config. cfg keys:
        mid_lo, mid_hi      : mid gate band (None,None = off)
        range_max           : range-so-far cap (None = off)
        flip_min            : sign-flip floor (None = off)
        vol_max             : spot vol cap (None = off)
        repair_delta        : absolute share repair band (residual cap, buy-then-pair)
        variant             : 'buy' (buy-then-pair) or 'mint' (mint-then-sell)
        spot_gate           : apply Binance-lead directional pull (bool)
    Returns per-market dict or None if never quotable.
    """
    yes_wins, spot_ts, spot_px = base.binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None

    wall = book['wall']
    bid = book['bid']; ask = book['ask']
    bsz = book['bsz']; asz = book['asz']
    mid = book['mid']
    wall0 = wall[0]
    run_min = np.minimum.accumulate(mid)
    run_max = np.maximum.accumulate(mid)

    def snap_idx(wq):
        i = np.searchsorted(wall, wq, side='right') - 1
        return max(i, 0)

    mid_lo = cfg['mid_lo']; mid_hi = cfg['mid_hi']
    range_max = cfg['range_max']; flip_min = cfg['flip_min']; vol_max = cfg['vol_max']
    repair = cfg['repair_delta']
    variant = cfg['variant']
    spot_gate = cfg['spot_gate']

    yes_long = 0.0
    no_long = 0.0
    yes_long_cost = 0.0
    no_long_cost = 0.0
    rebate_usdc = 0.0
    n_fills = 0
    filled_shares = 0.0
    quotable_ticks = 0
    total_ticks = 0

    # mint-then-sell state: minted pairs (each = 1 YES + 1 NO held, cost $1),
    # then legs sold off. We track sold YES (revenue) and sold NO (revenue) and
    # how many pairs were minted. Working capital = peak unredeemed minted pairs.
    minted = 0.0
    sold_yes = 0.0
    sold_no = 0.0
    sold_yes_rev = 0.0
    sold_no_rev = 0.0
    peak_capital = 0.0

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
        if (wq - wall0) < REGIME_WARMUP:
            continue

        total_ticks += 1

        # regime quote-gate (data so far, no lookahead)
        gated_out = False
        if mid_lo is not None and not (mid_lo <= m <= mid_hi):
            gated_out = True
        if not gated_out and range_max is not None:
            if (run_max[si] - run_min[si]) > range_max:
                gated_out = True
        if not gated_out and (vol_max is not None or flip_min is not None):
            vol, flips = base.spot_metrics(spot_ts, spot_px, close + wall0, close + wq)
            if vol_max is not None and vol > vol_max:
                gated_out = True
            if flip_min is not None and flips < flip_min:
                gated_out = True
        if gated_out:
            continue
        quotable_ticks += 1

        bid_live = True
        ask_live = True
        if t <= LATE_PULL_SECS:
            bid_live = ask_live = False
        if spot_gate:
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

        if variant == 'buy':
            # repair band: never let one leg outrun the other beyond `repair`
            delta = yes_long - no_long
            if delta > repair:
                bid_live = False
            elif delta < -repair:
                ask_live = False

            if bid_live and side == 'sell' and tp <= b + 1e-9:
                ahead = max(bsz[si], 0.0)
                frac = clip / (clip + ahead)
                qty = min(clip, tsz * frac)
                qty = min(qty, max(0.0, no_long + repair - yes_long))
                if qty > 1e-9:
                    yes_long += qty
                    yes_long_cost += qty * b
                    filled_shares += qty
                    n_fills += 1
                    rebate_usdc += qty * b * REBATE_FRAC

            if ask_live and side == 'buy' and tp >= a - 1e-9:
                ahead = max(asz[si], 0.0)
                frac = clip / (clip + ahead)
                qty = min(clip, tsz * frac)
                qty = min(qty, max(0.0, yes_long + repair - no_long))
                if qty > 1e-9:
                    no_long += qty
                    no_long_cost += qty * (1.0 - a)
                    filled_shares += qty
                    n_fills += 1
                    rebate_usdc += qty * (1.0 - a) * REBATE_FRAC

        else:  # mint-then-sell
            # Rest SELL YES at ask a (fills on taker BUY>=a) and SELL NO at (1-b)
            # i.e. a YES bid at b (fills on taker SELL<=b). To sell a leg we hold
            # it; mint on demand (split $1 -> +1 YES +1 NO). Selling ONE leg leaves
            # the OTHER leg held to resolution == the same directional residual as
            # buy-then-pair stranding. So we apply the SAME repair cap on the
            # sold-leg imbalance: never sell one side more than `repair` ahead of
            # the other. delta>0 means we've sold more YES (short YES residual);
            # pull the YES sell, keep selling NO to re-pair.
            delta = sold_yes - sold_no
            if delta > repair:
                ask_live = False
            elif delta < -repair:
                bid_live = False
            if ask_live and side == 'buy' and tp >= a - 1e-9:
                ahead = max(asz[si], 0.0)
                frac = clip / (clip + ahead)
                qty = min(clip, tsz * frac)
                qty = min(qty, max(0.0, sold_no + repair - sold_yes))
                if qty > 1e-9:
                    held_yes = minted - sold_yes
                    need = qty - held_yes
                    if need > 0:
                        minted += need
                    sold_yes += qty
                    sold_yes_rev += qty * a
                    filled_shares += qty
                    n_fills += 1
                    rebate_usdc += qty * a * REBATE_FRAC
            if bid_live and side == 'sell' and tp <= b + 1e-9:
                ahead = max(bsz[si], 0.0)
                frac = clip / (clip + ahead)
                qty = min(clip, tsz * frac)
                qty = min(qty, max(0.0, sold_yes + repair - sold_no))
                if qty > 1e-9:
                    held_no = minted - sold_no
                    need = qty - held_no
                    if need > 0:
                        minted += need
                    sold_no += qty
                    sold_no_rev += qty * (1.0 - b)  # selling NO at 1-b
                    filled_shares += qty
                    n_fills += 1
                    rebate_usdc += qty * (1.0 - b) * REBATE_FRAC
            peak_capital = max(peak_capital, minted)

    rebate = rebate_usdc if rebate_on else 0.0

    if variant == 'buy':
        if quotable_ticks == 0:
            return None
        paired = min(yes_long, no_long)
        pnl_pairs = 0.0
        if paired > 0:
            yes_avg = yes_long_cost / yes_long if yes_long > 0 else 0.0
            no_avg = no_long_cost / no_long if no_long > 0 else 0.0
            pnl_pairs = paired * (1.0 - (yes_avg + no_avg))
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
        net = pnl_pairs + pnl_resid + rebate
        total_leg = yes_long + no_long
        resid_frac = res_shares / total_leg if total_leg > 0 else 0.0
        return {
            'close': close, 'yes_wins': yes_wins,
            'n_fills': n_fills, 'filled_shares': filled_shares,
            'paired': paired, 'res_shares': res_shares, 'resid_frac': resid_frac,
            'pnl_pairs': pnl_pairs, 'pnl_resid': pnl_resid, 'rebate': rebate,
            'net': net, 'quote_frac': quotable_ticks / total_ticks if total_ticks else 0.0,
            'quotable_ticks': quotable_ticks, 'total_ticks': total_ticks,
            'capital': total_leg * 0.5,
        }

    # mint-then-sell settle
    if quotable_ticks == 0:
        return None
    # held legs at expiry redeem: YES pays 1 if yes_wins, NO pays 1 if not.
    held_yes = minted - sold_yes
    held_no = minted - sold_no
    yes_payoff = 1.0 if yes_wins else 0.0
    no_payoff = 0.0 if yes_wins else 1.0
    redeem = held_yes * yes_payoff + held_no * no_payoff
    # PnL = revenue from sold legs + redemption of held legs - mint cost (minted * $1)
    net_core = sold_yes_rev + sold_no_rev + redeem - minted * 1.0
    net = net_core + rebate
    # "pairs" (spread capture) = the part where both legs of a minted pair were
    # sold: matched = min(sold_yes, sold_no); each matched unit captured
    # (avg_yes_px + avg_no_px) - 1.
    matched = min(sold_yes, sold_no)
    avg_yes_px = sold_yes_rev / sold_yes if sold_yes > 0 else 0.0
    avg_no_px = sold_no_rev / sold_no if sold_no > 0 else 0.0
    pnl_pairs = matched * (avg_yes_px + avg_no_px - 1.0)
    # residual / stranding = net minus the matched spread capture and rebate.
    # For mint-then-sell the "stranding" is the unsold leg(s) of a minted pair that
    # redeem at outcome instead of being sold for spread; bounded by construction
    # (worst case redeem $1, never a loss below mint cost on the unsold side).
    pnl_resid = net_core - pnl_pairs
    total_sold = sold_yes + sold_no
    resid_units = abs(sold_yes - sold_no)
    resid_frac = resid_units / total_sold if total_sold > 0 else 0.0
    return {
        'close': close, 'yes_wins': yes_wins,
        'n_fills': n_fills, 'filled_shares': total_sold,
        'paired': matched, 'res_shares': resid_units, 'resid_frac': resid_frac,
        'pnl_pairs': pnl_pairs, 'pnl_resid': pnl_resid, 'rebate': rebate,
        'net': net, 'quote_frac': quotable_ticks / total_ticks if total_ticks else 0.0,
        'quotable_ticks': quotable_ticks, 'total_ticks': total_ticks,
        'capital': peak_capital,
    }


def run(parsed, clip, cfg, rebate_on):
    rows = []
    for book, close, trades, bin_day, date in parsed:
        r = simulate(book, close, trades, bin_day, clip, cfg, rebate_on)
        if r is None:
            continue
        r['date'] = date
        rows.append(r)
    return rows


def summarize(rows, label, n_days, n_total):
    if not rows:
        return {'label': label, 'n_markets': 0}
    net = np.array([r['net'] for r in rows])
    fshares = np.array([r['filled_shares'] for r in rows])
    resid = np.array([r['resid_frac'] for r in rows])
    pnl_pairs = np.array([r['pnl_pairs'] for r in rows])
    pnl_resid = np.array([r['pnl_resid'] for r in rows])
    rebate = np.array([r['rebate'] for r in rows])
    qfrac = np.array([r['quote_frac'] for r in rows])
    paired = np.array([r['paired'] for r in rows])
    res_sh = np.array([r['res_shares'] for r in rows])
    cap = np.array([r['capital'] for r in rows])
    total_sh = fshares.sum()
    pair_rate = paired.sum() / (paired.sum() + res_sh.sum()) if (paired.sum() + res_sh.sum()) > 0 else 0.0
    return {
        'label': label,
        'n_markets': len(rows),
        'pct_universe': 100.0 * len(rows) / n_total if n_total else 0.0,
        'net_per_day': float(net.sum() / n_days),
        'net_total': float(net.sum()),
        'c_per_share': float(net.sum() / total_sh * 100) if total_sh > 0 else 0.0,
        'pct_pos': float((net > 0).mean() * 100),
        'mean_qfrac': float(qfrac.mean() * 100),
        'pairs_per_day': float(pnl_pairs.sum() / n_days),
        'resid_per_day': float(pnl_resid.sum() / n_days),
        'rebate_per_day': float(rebate.sum() / n_days),
        'pair_rate': float(pair_rate * 100),
        'mean_resid_frac': float(resid.mean() * 100),
        'worst': float(net.min()),
        'mean_cap': float(cap.mean()),
    }


def fmt(s):
    if s['n_markets'] == 0:
        return f"  {s['label']:<34} (no quotable markets)"
    return (f"  {s['label']:<34} mkts={s['n_markets']:5d}({s['pct_universe']:4.1f}%) "
            f"q={s['mean_qfrac']:4.1f}% net=${s['net_per_day']:7.2f}/d "
            f"{s['c_per_share']:+.3f}c/sh pair={s['pair_rate']:4.1f}% "
            f"resid={s['mean_resid_frac']:4.1f}% "
            f"[spr=${s['pairs_per_day']:6.2f} strand=${s['resid_per_day']:6.2f} "
            f"reb=${s['rebate_per_day']:5.2f}]/d worst=${s['worst']:.2f} cap=${s['mean_cap']:.1f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--limit', type=int, default=0)
    ap.add_argument('--clip', type=int, default=10)
    args = ap.parse_args()

    book_files = sorted(glob.glob(f'{base.BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    if args.limit:
        step = max(1, len(book_files) // args.limit)
        book_files = book_files[::step][:args.limit]
    dates = sorted({[p for p in f.split('/') if p.startswith('date=')][0].split('=')[1]
                    for f in book_files})
    n_days = len(dates)
    print(f'book files: {len(book_files)}  days: {n_days} ({dates[0]}..{dates[-1]})', file=sys.stderr)

    bin_cache = {}
    print('parsing all markets once...', file=sys.stderr, flush=True)
    parsed = base.load_all_markets(book_files, bin_cache)
    n_total = len(parsed)
    print(f'parsed {n_total} markets with trades', file=sys.stderr, flush=True)

    clip = args.clip
    REP = base.REPAIR_DELTA_SHARES  # 2.0

    base_cfg = dict(mid_lo=0.30, mid_hi=0.70, range_max=0.06, flip_min=0.20,
                    vol_max=REGIME_SPOT_VOL_MAX, repair_delta=REP, variant='buy', spot_gate=True)

    def cfg(**kw):
        c = dict(base_cfg); c.update(kw); return c

    print('=' * 130, flush=True)
    print(f'GATE x CAP SWEEP  clip={clip}  buy-then-pair  rebate=ON  (q=mean tick quote-fraction, spr=spread capture, strand=residual)')
    print(f'days={n_days}  universe={n_total} markets')
    print('=' * 130, flush=True)

    # --- 1. RANGE-GATE relaxation (mid/flip/vol fixed at base) ---
    print('\n--- 1a. RANGE-GATE relaxation (mid 0.30-0.70, flip>=0.20, vol gate ON) ---', flush=True)
    for rng in (0.06, 0.08, 0.10, 0.15, None):
        tag = f'range<={rng}' if rng is not None else 'range OFF'
        s = summarize(run(parsed, clip, cfg(range_max=rng), True), tag, n_days, n_total)
        print(fmt(s), flush=True)

    print('\n--- 1b. MID-GATE relaxation (range<=0.10, flip>=0.20, vol ON) ---', flush=True)
    for lo, hi, tag in ((0.30, 0.70, 'mid 0.30-0.70'), (0.20, 0.80, 'mid 0.20-0.80'), (None, None, 'mid OFF')):
        s = summarize(run(parsed, clip, cfg(range_max=0.10, mid_lo=lo, mid_hi=hi), True), tag, n_days, n_total)
        print(fmt(s), flush=True)

    print('\n--- 1c. FLIP/VOL-GATE relaxation (range<=0.10, mid 0.20-0.80) ---', flush=True)
    for fmin, vmax, tag in ((0.20, REGIME_SPOT_VOL_MAX, 'flip>=0.20 vol ON'),
                            (0.10, REGIME_SPOT_VOL_MAX, 'flip>=0.10 vol ON'),
                            (None, REGIME_SPOT_VOL_MAX, 'flip OFF vol ON'),
                            (None, None, 'flip OFF vol OFF')):
        s = summarize(run(parsed, clip, cfg(range_max=0.10, mid_lo=0.20, mid_hi=0.80,
                                            flip_min=fmin, vol_max=vmax), True), tag, n_days, n_total)
        print(fmt(s), flush=True)

    print('\n--- 1d. FULLY UNGATED (all regime gates off, dynamic gates ON) ---', flush=True)
    s = summarize(run(parsed, clip, cfg(mid_lo=None, mid_hi=None, range_max=None,
                                        flip_min=None, vol_max=None), True), 'all gates OFF', n_days, n_total)
    print(fmt(s), flush=True)

    # --- 2. RESIDUAL CAP sweep at two gate settings ---
    print('\n--- 2a. RESIDUAL-CAP sweep @ current gate (range<=0.06, mid 0.30-0.70) ---', flush=True)
    for rep in (1.0, 2.0, 4.0, 8.0):
        s = summarize(run(parsed, clip, cfg(repair_delta=rep), True), f'cap={rep:.0f}sh', n_days, n_total)
        print(fmt(s), flush=True)

    print('\n--- 2b. RESIDUAL-CAP sweep @ relaxed gate (range<=0.10, mid 0.20-0.80) ---', flush=True)
    for rep in (1.0, 2.0, 4.0, 8.0):
        s = summarize(run(parsed, clip, cfg(range_max=0.10, mid_lo=0.20, mid_hi=0.80,
                                            repair_delta=rep), True), f'cap={rep:.0f}sh', n_days, n_total)
        print(fmt(s), flush=True)

    print('\n--- 2c. RESIDUAL-CAP sweep @ fully ungated ---', flush=True)
    for rep in (1.0, 2.0, 4.0, 8.0):
        s = summarize(run(parsed, clip, cfg(mid_lo=None, mid_hi=None, range_max=None,
                                            flip_min=None, vol_max=None, repair_delta=rep), True),
                      f'cap={rep:.0f}sh', n_days, n_total)
        print(fmt(s), flush=True)

    # --- 3. The quote-more sweet spot grid: {range gate} x {cap} ---
    print('\n--- 3. SWEET-SPOT GRID  range x cap  (mid 0.20-0.80, flip OFF, vol ON) ---', flush=True)
    for rng in (0.06, 0.10, 0.15, None):
        for rep in (1.0, 2.0, 4.0):
            rtag = f'r<={rng}' if rng is not None else 'rOFF'
            s = summarize(run(parsed, clip, cfg(range_max=rng, mid_lo=0.20, mid_hi=0.80,
                                                flip_min=None, repair_delta=rep), True),
                          f'{rtag} cap={rep:.0f}', n_days, n_total)
            print(fmt(s), flush=True)

    # --- 4. mint-then-sell vs buy-then-pair, at current + relaxed gate ---
    print('\n--- 4. MINT-THEN-SELL vs BUY-THEN-PAIR (rebate ON) ---', flush=True)
    for gate_tag, gate_kw in (('current(r<=.06,mid.3-.7)', dict()),
                              ('relaxed(r<=.10,mid.2-.8)', dict(range_max=0.10, mid_lo=0.20, mid_hi=0.80, flip_min=None)),
                              ('ungated', dict(mid_lo=None, mid_hi=None, range_max=None, flip_min=None, vol_max=None))):
        for variant in ('buy', 'mint'):
            s = summarize(run(parsed, clip, cfg(variant=variant, **gate_kw), True),
                          f'{variant:4s} {gate_tag}', n_days, n_total)
            print(fmt(s), flush=True)

    print('\nDONE', flush=True)


if __name__ == '__main__':
    main()
