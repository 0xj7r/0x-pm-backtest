"""REALISTIC-FILL paired-MM simulator (Phase 2 of the robust MM backtest).

The optimistic sim (mm_paired_sim.py) fills a resting leg on EVERY taker print
that crosses our price, pro-rata by clip/(clip+depth). The Phase-1 queue-model
fit (scripts/mm_queue_model_fit.py, docs/mm_queue_model_2026-06-01.md) showed
that overstates the live fill RATE ~14x: live orders rest ~4s on a replace
cycle and only 1.6% of posted orders ever fill, because flow rarely arrives
during the short window the order is actually resting at the touch. CONDITIONAL
on a fill we capture ~4x pro-rata (front-of-queue from joining at the touch).

This sim replaces the fill logic with the rest-and-hold model:

  - We post a quote, hold it for REST_WINDOW seconds, then requote on a
    REQUOTE_CADENCE. An order can fill ONLY from taker prints that arrive DURING
    its own rest window (not the whole market). Between requotes there is a tiny
    idle gap so we are not continuously resting (matches the live replace cycle
    where the order is briefly absent / being re-submitted).
  - When a crossing print lands inside the active rest window, the realized fill
    is the conditional front-of-queue capture:
        realized = min(clip, K * pro_rata * taker_through_at_level)
    with pro_rata = clip/(clip+depth), K~4 (CAPTURE_K). The min(clip,..) caps a
    single order at its clip; cumulative fills across a rest window also cap at
    clip (an order cannot over-fill).

Everything else (strict pairing, residual mark-to-outcome, the regime quote-gate
and the dynamic tick gates) is reused unchanged from mm_paired_sim via import.

The net effect reproduces the live ~1.6% order-fill-rate and a much lower per-day
P&L than the optimistic sim; the sim reports its OWN implied fill-rate and
conditional capture so they can be validated against the live numbers.

Tunable params (top-level, for Phase 3 sweeps):
  CAPTURE_K        - conditional front-of-queue multiplier on pro-rata (~4)
  REST_WINDOW      - seconds an order rests at the touch before requote (~4)
  REQUOTE_CADENCE  - seconds between successive posts (~5, the live replace cycle)

Usage:
  python3 scripts/mm_paired_realistic_sim.py            # full run vs optimistic
  python3 scripts/mm_paired_realistic_sim.py --limit 300
  python3 scripts/mm_paired_realistic_sim.py --K 4 --rest 4 --cadence 5
"""
import argparse
import glob
import math
import sys

import numpy as np

import mm_paired_sim as S

# Realistic-fill tunables (calibrated from the Phase-1 live queue log).
CAPTURE_K = 4.0          # conditional front-of-queue beat over pro-rata (geomean ~4.3, median 3.8)
REST_WINDOW = 4.0        # seconds an order actually rests at the touch (live median ~4.0s)
REQUOTE_CADENCE = 5.0    # seconds between posts (live 5s replace cycle)


def simulate_market_realistic(book, close, trades, bin_day, clip, regime_gated,
                              rebate_on, K, rest_window, requote_cadence):
    """Realistic-fill variant of S.simulate_market.

    Mirrors the optimistic sim's gating/pairing/redemption exactly, but fills a
    leg only from taker prints inside the order's active rest window, with the
    conditional front-of-queue capture K*pro_rata*through.
    """
    yes_wins, spot_ts, spot_px = S.binance_outcome_and_vol(bin_day, close)
    if yes_wins is None:
        return None

    wall = book['wall']; tsec = book['t']
    bid = book['bid']; ask = book['ask']
    bsz = book['bsz']; asz = book['asz']
    mid = book['mid']
    wall0 = wall[0]

    run_min = np.minimum.accumulate(mid)
    run_max = np.maximum.accumulate(mid)

    def snap_idx(wq):
        i = np.searchsorted(wall, wq, side='right') - 1
        return max(i, 0)

    yes_long = 0.0
    no_long = 0.0
    yes_long_cost = 0.0
    no_long_cost = 0.0
    rebate_usdc = 0.0
    n_fills = 0
    filled_shares = 0.0
    adverse_5s = []

    # Rest-window bookkeeping. A "post cycle" begins at the start of each
    # requote_cadence bucket and is live for rest_window seconds. Within one
    # cycle a leg fills at most `clip`; the cycle's filled amount is tracked so
    # cumulative fills cannot exceed the clip. CRITICAL realism: the quoted
    # prices are LOCKED at the cycle start (post_bid/post_ask) and held for the
    # whole rest window. A taker print fills us only if it crosses the price WE
    # POSTED, not the live touch. When mid drifts away during the rest window our
    # locked quote sits behind the touch and never fills: that price-following
    # lag is the dominant live attrition that collapses the fill rate to ~1.6%.
    cycle_start = None       # wall time the current post cycle began
    post_bid = None          # YES bid price locked at cycle start
    post_ask = None          # YES ask price locked at cycle start
    post_bsz = 0.0           # depth ahead at the bid when we posted
    post_asz = 0.0           # depth ahead at the ask when we posted
    cycle_bid_filled = 0.0   # shares filled on the YES bid this cycle
    cycle_ask_filled = 0.0   # shares filled on the YES ask this cycle
    n_post_cycles = 0        # count of bid-side + ask-side order-posts (live-quotable)
    # capture diagnostics: realized fill_frac and the pro-rata it beat, per fill
    cap_fill_frac = []       # realized qty/clip on each fill event
    cap_prorata = []         # pro-rata frac at that fill (for the realized/pro-rata multiple)

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

        if (wq - wall0) < S.REGIME_WARMUP:
            continue

        # regime quote-gate (identical to the optimistic sim)
        if regime_gated:
            if not (S.REGIME_MID_LO <= m <= S.REGIME_MID_HI):
                continue
            rng = run_max[si] - run_min[si]
            if rng > S.REGIME_RANGE_MAX:
                continue
            vol, flips = S.spot_metrics(spot_ts, spot_px, close + wall0, close + wq)
            if vol > S.REGIME_SPOT_VOL_MAX:
                continue
            if flips < S.REGIME_FLIP_MIN:
                continue
        market_quotable_any = True

        # ---- rest-and-hold: are we actually resting a quote at this instant? ----
        # We re-post on a requote_cadence grid and the order is live for the
        # first rest_window seconds of each cycle; the rest of the cadence is the
        # idle/replace gap where no order is resting and nothing can fill.
        if cycle_start is None or (wq - cycle_start) >= requote_cadence:
            cycle_start = wq - ((wq - wall0) % requote_cadence)
            post_bid = b           # lock the quote at the touch as of the post
            post_ask = a
            post_bsz = max(bsz[si], 0.0)
            post_asz = max(asz[si], 0.0)
            cycle_bid_filled = 0.0
            cycle_ask_filled = 0.0
            n_post_cycles += 2   # we post both a bid and an ask each cycle (subject to gates)
        resting = (wq - cycle_start) < rest_window
        if not resting:
            continue

        # dynamic tick gates (identical to the optimistic sim)
        bid_live = True
        ask_live = True
        if t <= S.LATE_PULL_SECS:
            bid_live = ask_live = False
        sr = S.spot_return_30s(spot_ts, spot_px, close + wq)
        if sr > S.SPOT_ACCEL_PULL:
            ask_live = False
        elif sr < -S.SPOT_ACCEL_PULL:
            bid_live = False
        if tsz >= S.LARGE_TAKER_SHARES:
            if side == 'sell':
                bid_live = False
            else:
                ask_live = False
        delta = yes_long - no_long
        if delta > S.REPAIR_DELTA_SHARES:
            bid_live = False
        elif delta < -S.REPAIR_DELTA_SHARES:
            ask_live = False

        # ---- realistic fill: only flow arriving DURING the rest window that
        # crosses the LOCKED post price, with the conditional front-of-queue
        # capture, capped at clip per cycle. A SELL only fills us if it trades
        # AT/THROUGH our posted bid AND has eaten the depth that was ahead of us
        # when we posted (we are at the back of the resting queue at our level).
        through = max(0.0, tsz - post_bsz) if post_bid is not None else 0.0
        if bid_live and post_bid is not None and side == 'sell' and tp <= post_bid + 1e-9 and through > 0:
            pro_rata = clip / (clip + post_bsz)
            # Through-flow GATE (above): a fill happens only when a taker SELL
            # crosses our LOCKED bid AND its size exceeds the depth that was ahead
            # of us at post (otherwise the resting queue ahead absorbs it). This
            # gate is what collapses the order fill RATE to the live ~1.6%.
            # SIZE conditional on a fill is the front-of-queue beat K*pro_rata
            # of a clip (the live ~4x), capped at clip and the cycle's remaining
            # room (cumulative fills across the rest window cannot exceed clip).
            cap = K * pro_rata * clip
            room_cycle = max(0.0, clip - cycle_bid_filled)
            qty = min(clip, cap, room_cycle)
            qty = min(qty, max(0.0, no_long + S.REPAIR_DELTA_SHARES - yes_long))
            if qty > 1e-9:
                yes_long += qty
                yes_long_cost += qty * post_bid
                filled_shares += qty
                cycle_bid_filled += qty
                n_fills += 1
                cap_fill_frac.append(qty / clip)
                cap_prorata.append(pro_rata)
                rebate_usdc += qty * post_bid * S.REBATE_FRAC
                fj = snap_idx(wq + 5.0)
                adverse_5s.append((+1) * (mid[fj] - m) * qty)

        # YES ask: fills on a taker BUY crossing our LOCKED posted ask (== buy NO).
        through_a = max(0.0, tsz - post_asz) if post_ask is not None else 0.0
        if ask_live and post_ask is not None and side == 'buy' and tp >= post_ask - 1e-9 and through_a > 0:
            pro_rata = clip / (clip + post_asz)
            cap = K * pro_rata * clip
            room_cycle = max(0.0, clip - cycle_ask_filled)
            qty = min(clip, cap, room_cycle)
            qty = min(qty, max(0.0, yes_long + S.REPAIR_DELTA_SHARES - no_long))
            if qty > 1e-9:
                no_long += qty
                no_long_cost += qty * (1.0 - post_ask)
                filled_shares += qty
                cycle_ask_filled += qty
                n_fills += 1
                cap_fill_frac.append(qty / clip)
                cap_prorata.append(pro_rata)
                rebate_usdc += qty * (1.0 - post_ask) * S.REBATE_FRAC
                fj = snap_idx(wq + 5.0)
                adverse_5s.append((-1) * (mid[fj] - m) * qty)

    if not market_quotable_any:
        return None

    # strict pairing + redemption (identical to the optimistic sim)
    paired = min(yes_long, no_long)
    pnl_pairs = 0.0
    pair_cost_total = 0.0
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
        'slug_close': close,
        'yes_wins': yes_wins,
        'n_fills': n_fills,
        'filled_shares': filled_shares,
        'n_post_cycles': n_post_cycles,
        'yes_long': yes_long, 'no_long': no_long,
        'paired': paired, 'res_shares': res_shares, 'resid_frac': resid_frac,
        'pnl_pairs': pnl_pairs, 'pnl_resid': pnl_resid, 'rebate': rebate,
        'net': net,
        'pair_cost': pair_cost_total,
        'adverse_5s_sum': float(np.sum(adverse_5s)) if adverse_5s else 0.0,
        'adverse_5s_min': float(np.min(adverse_5s)) if adverse_5s else 0.0,
        'cap_fill_frac': cap_fill_frac,
        'cap_prorata': cap_prorata,
    }


def run_realistic(parsed, clip, regime_gated, rebate_on, K, rest_window, requote_cadence):
    rows = []
    for book, close, trades, bin_day, date in parsed:
        r = simulate_market_realistic(book, close, trades, bin_day, clip,
                                      regime_gated, rebate_on, K, rest_window,
                                      requote_cadence)
        if r is None:
            continue
        r['date'] = date
        rows.append(r)
    return rows


def implied_fill_stats(rows, clip):
    """Sim's own implied order-fill-rate and conditional capture, for validation
    against the live ~1.6% rate / ~4x conditional capture."""
    if not rows:
        return None
    n_posts = sum(r['n_post_cycles'] for r in rows)
    n_filled_orders = sum(r['n_fills'] for r in rows)  # each fill event ~ one order-cross
    posted_shares = n_posts * clip
    filled_shares = sum(r['filled_shares'] for r in rows)
    # conditional capture: realized shares / clip on the cycles that filled,
    # expressed as a multiple of mean pro-rata is not directly recoverable here,
    # so report realized fill fraction conditional on a fill = filled/clip avg.
    fill_rate = (n_filled_orders / n_posts) if n_posts else 0.0
    unconditional_frac = (filled_shares / posted_shares) if posted_shares else 0.0
    cond_capture = (filled_shares / n_filled_orders / clip) if n_filled_orders else 0.0
    # realized/pro-rata multiple per fill (directly comparable to live ~4x).
    mults = []
    for r in rows:
        for ff, pr in zip(r.get('cap_fill_frac', []), r.get('cap_prorata', [])):
            if pr and pr > 0:
                mults.append(ff / pr)
    cap_mult_median = float(np.median(mults)) if mults else 0.0
    cap_mult_geomean = (float(np.exp(np.mean(np.log(np.clip(mults, 1e-9, None)))))
                        if mults else 0.0)
    return {
        'n_posts': n_posts,
        'n_filled_orders': n_filled_orders,
        'order_fill_rate_pct': fill_rate * 100,
        'posted_shares': posted_shares,
        'filled_shares': filled_shares,
        'unconditional_share_frac_pct': unconditional_frac * 100,
        'cond_capture_frac': cond_capture,
        'cap_mult_median': cap_mult_median,
        'cap_mult_geomean': cap_mult_geomean,
    }


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--limit', type=int, default=0)
    ap.add_argument('--clips', type=str, default='5,10,20')
    ap.add_argument('--K', type=float, default=CAPTURE_K)
    ap.add_argument('--rest', type=float, default=REST_WINDOW)
    ap.add_argument('--cadence', type=float, default=REQUOTE_CADENCE)
    args = ap.parse_args()

    K, rest_window, requote_cadence = args.K, args.rest, args.cadence

    book_files = sorted(glob.glob(f'{S.BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    if args.limit:
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
    parsed = S.load_all_markets(book_files, bin_cache)
    print(f'parsed {len(parsed)} markets with trades', file=sys.stderr, flush=True)

    print('=' * 110, flush=True)
    print('REALISTIC-FILL PAIRED-MM SIMULATOR  (rest-and-hold + conditional front-of-queue capture)')
    print(f'K={K}  rest_window={rest_window}s  requote_cadence={requote_cadence}s  '
          f'days={n_days}  rebate(on)={S.REBATE_FRAC*100:.3f}% of notional')
    print('=' * 110, flush=True)

    for clip in clips:
        print(f'\n#### CLIP = {clip} shares ' + '#' * 70, flush=True)
        for rebate_on in (False, True):
            rtag = 'rebate' if rebate_on else 'norebate'
            for gated in (True, False):
                gtag = 'GATED' if gated else 'ungated'
                # realistic
                rows_r = run_realistic(parsed, clip, gated, rebate_on, K,
                                       rest_window, requote_cadence)
                s_r = S.summarize(rows_r, f'REALISTIC {gtag}/{rtag}', n_days)
                print(S.fmt_row(s_r), flush=True)
                # optimistic (the old sim) for the same config
                rows_o = S.run(parsed, clip, gated, rebate_on)
                s_o = S.summarize(rows_o, f'OPTIMISTIC {gtag}/{rtag}', n_days)
                print(S.fmt_row(s_o), flush=True)
                # inflation factor
                if s_r['n_markets'] and s_o['n_markets'] and s_r['net_total'] != 0:
                    infl = s_o['net_total'] / s_r['net_total']
                    print(f'      -> optimistic / realistic net = {infl:.1f}x '
                          f'(${s_o["net_per_day"]:.2f}/day vs ${s_r["net_per_day"]:.2f}/day)',
                          flush=True)

    # validation block: implied fill-rate + conditional capture vs live numbers
    print('\n' + '=' * 110, flush=True)
    print('FILL-MODEL VALIDATION  (sim implied vs live: order-fill-rate ~1.6%, conditional capture ~4x)')
    print('=' * 110, flush=True)
    for clip in clips:
        rows = run_realistic(parsed, clip, True, False, K, rest_window, requote_cadence)
        st = implied_fill_stats(rows, clip)
        if st is None:
            print(f'  clip={clip}: no quotable markets')
            continue
        print(f'  clip={clip:2d} GATED: order_fill_rate={st["order_fill_rate_pct"]:.2f}% '
              f'(live ~1.6%)  uncond_share_frac={st["unconditional_share_frac_pct"]:.2f}% '
              f'(live 0.84%)  realized/pro-rata={st["cap_mult_geomean"]:.1f}x geomean '
              f'(median {st["cap_mult_median"]:.1f}x; live ~4x)  '
              f'posts={st["n_posts"]} fills={st["n_filled_orders"]}', flush=True)


if __name__ == '__main__':
    main()
