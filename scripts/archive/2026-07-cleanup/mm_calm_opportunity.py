"""Calm-regime MM opportunity estimate from PM book + trade tapes.

Tape-based analysis (does NOT use the engine's optimistic touch-fill maker model).
A resting bid only fills when a taker SELL prints at/below our price; a resting ask
only fills when a taker BUY prints at/above our price. We then measure adverse
selection as the signed mid move over the next few seconds after the fill.

NO = 1 - YES (byte-exact), so we analyse the YES (e.g. 'Up') outcome only.
"""
import glob, os, sys, math
import numpy as np
import pyarrow.parquet as pq
import pandas as pd

BOOK_ROOT = 'data/cache/raw/telonex/exchange=polymarket/channel=book_snapshot_25'
TRADE_ROOT = 'data/cache/raw/telonex/exchange=polymarket/channel=trades'

ACTIVE_WIN = 300.0          # last 5 minutes
COINFLIP_LO, COINFLIP_HI = 0.40, 0.60
CALM_RANGE_MAX = 0.20       # max-min YES mid over active window <= 20c => calm
TOXIC_SECS = 30.0           # last-N-seconds toxic window
ADV_HORIZONS = [2.0, 5.0, 10.0]  # seconds after fill to measure adverse mid move
REQUOTE_CANCEL_MOVE = 0.005  # cancel/requote if mid moves >= 0.5c against pre-fill mid


def read_parquet(path, cols=None):
    pf = pq.ParquetFile(path)
    return pf.read(columns=cols).to_pandas()


def load_book(path):
    df = read_parquet(path, ['timestamp_us', 'slug',
                             'bid_price_0', 'bid_size_0', 'ask_price_0', 'ask_size_0'])
    if len(df) == 0:
        return None, None
    slug = df.slug.iloc[0]
    close = int(slug.split('-')[-1])
    df['t'] = close - df.timestamp_us / 1e6
    df['bp'] = pd.to_numeric(df.bid_price_0, errors='coerce')
    df['ap'] = pd.to_numeric(df.ask_price_0, errors='coerce')
    df['bs'] = pd.to_numeric(df.bid_size_0, errors='coerce')
    df['asz'] = pd.to_numeric(df.ask_size_0, errors='coerce')
    df = df.dropna(subset=['bp', 'ap'])
    df = df[(df.t >= -1) & (df.t <= ACTIVE_WIN)].sort_values('t', ascending=False).reset_index(drop=True)
    df['mid'] = (df.bp + df.ap) / 2
    df['spread'] = df.ap - df.bp
    return df, (slug, close)


def trade_path_for(book_path):
    # mirror dir structure: book .../book_snapshot_25/date=D/asset_id=A/<file>
    parts = book_path.split('/')
    date = [p for p in parts if p.startswith('date=')][0]
    asset = [p for p in parts if p.startswith('asset_id=')][0]
    cand = glob.glob(f'{TRADE_ROOT}/{date}/{asset}/*.parquet')
    return cand[0] if cand else None


def load_trades(path, close):
    df = read_parquet(path, ['timestamp_us', 'price', 'size', 'side'])
    if len(df) == 0:
        return None
    df['t'] = close - df.timestamp_us / 1e6
    df['p'] = pd.to_numeric(df.price, errors='coerce')
    df['sz'] = pd.to_numeric(df['size'], errors='coerce')
    df = df.dropna(subset=['p', 'sz'])
    df = df[(df.t >= 0) & (df.t <= ACTIVE_WIN)].sort_values('t', ascending=False).reset_index(drop=True)
    return df


def mid_at(book, t):
    """mid at the snapshot active just before/at time-to-close t (book sorted desc by t)."""
    # book.t descending; we want the most recent snapshot with t >= query t (i.e. earlier in wall time)
    sub = book[book.t >= t]
    if len(sub) == 0:
        return book.mid.iloc[0]
    return sub.mid.iloc[-1]


def analyze_market(book_path):
    book, meta = load_book(book_path)
    if book is None or len(book) < 5:
        return None
    slug, close = meta
    act = book[(book.t >= 0) & (book.t <= ACTIVE_WIN)]
    if len(act) < 5:
        return None
    yrange = act.mid.max() - act.mid.min()
    mid_at_open = act[act.t <= ACTIVE_WIN].mid.iloc[0] if len(act) else np.nan
    median_mid = act.mid.median()
    is_coinflip = COINFLIP_LO <= median_mid <= COINFLIP_HI
    is_calm = yrange <= CALM_RANGE_MAX

    res = {
        'slug': slug, 'close': close,
        'n_snap': len(act), 'yrange': yrange, 'median_mid': median_mid,
        'coinflip': is_coinflip, 'calm': is_calm,
        'spread_mean': act.spread.mean(), 'spread_med': act.spread.median(),
        'spread_p90': act.spread.quantile(0.90),
        'frac_spread_ge2c': (act.spread >= 0.0199).mean(),
        'frac_spread_eq1c': ((act.spread > 0.005) & (act.spread < 0.0149)).mean(),
        'tob_bid_med': act.bs.median(), 'tob_ask_med': act.asz.median(),
        'tob_bid_p25': act.bs.quantile(0.25), 'tob_ask_p25': act.asz.quantile(0.25),
    }

    tpath = trade_path_for(book_path)
    fills = []
    if tpath:
        tr = load_trades(tpath, close)
        if tr is not None and len(tr):
            # Book sorted desc by t (t = seconds-to-close). Build ascending wall-time
            # arrays so we can searchsorted: wall = -t increases toward close.
            bk = book.sort_values('t', ascending=False).reset_index(drop=True)
            wall = (-bk.t.values)          # ascending
            bmid = bk.mid.values
            bbid = bk.bp.values
            bask = bk.ap.values

            def state_at(wq):
                # most recent snapshot at or before wall time wq
                idx = np.searchsorted(wall, wq, side='right') - 1
                if idx < 0:
                    idx = 0
                return bbid[idx], bask[idx], bmid[idx]

            for _, row in tr.iterrows():
                t = row.t
                wq = -t
                bid, ask, mid = state_at(wq)
                if not (math.isfinite(bid) and math.isfinite(ask)):
                    continue
                side = row.side
                if side == 'sell' and row.p <= bid + 1e-9:
                    entry_mid = mid; fill_price = bid; pos = +1  # long YES
                elif side == 'buy' and row.p >= ask - 1e-9:
                    entry_mid = mid; fill_price = ask; pos = -1  # short YES
                else:
                    continue
                adv = {}
                for h in ADV_HORIZONS:
                    _, _, fut_mid = state_at(wq + h)  # h seconds later in wall time
                    adv[h] = pos * (fut_mid - entry_mid)
                fills.append({
                    't': t, 'pos': pos, 'fill_price': fill_price, 'entry_mid': entry_mid,
                    'size': row.sz, 'spread_at_fill': ask - bid,
                    **{f'mtm_{h}': adv[h] for h in ADV_HORIZONS},
                })
    res['fills'] = fills
    return res


def main():
    book_files = sorted(glob.glob(f'{BOOK_ROOT}/date=*/asset_id=*/*.parquet'))
    print(f'total book files: {len(book_files)}', file=sys.stderr)
    rows = []
    all_fills = []
    for i, bf in enumerate(book_files):
        try:
            r = analyze_market(bf)
        except Exception as e:
            continue
        if r is None:
            continue
        for f in r['fills']:
            f['slug'] = r['slug']
            f['calm'] = r['calm']
            f['coinflip'] = r['coinflip']
            f['median_mid'] = r['median_mid']
            all_fills.append(f)
        r2 = {k: v for k, v in r.items() if k != 'fills'}
        r2['n_fills'] = len(r['fills'])
        rows.append(r2)
        if (i + 1) % 500 == 0:
            print(f'  processed {i+1}/{len(book_files)}', file=sys.stderr)
    md = pd.DataFrame(rows)
    fd = pd.DataFrame(all_fills)
    md.to_parquet('/tmp/mm_markets.parquet')
    if len(fd):
        fd.to_parquet('/tmp/mm_fills.parquet')
    print(f'markets analysed: {len(md)}; total simulated maker fills: {len(fd)}', file=sys.stderr)

    report(md, fd)


def report(md, fd):
    def p(s=''): print(s)
    p('=' * 70)
    p('UNIVERSE')
    p('=' * 70)
    p(f'markets: {len(md)}  | coinflip(0.40-0.60 median): {md.coinflip.sum()} '
      f'| calm(range<=20c): {md.calm.sum()} | calm&coinflip: {(md.calm & md.coinflip).sum()}')

    for label, sub in [('ALL', md), ('CALM&COINFLIP', md[md.calm & md.coinflip]),
                       ('NON-CALM', md[~md.calm])]:
        if len(sub) == 0:
            continue
        p()
        p('-' * 70)
        p(f'[{label}]  n={len(sub)}')
        p('-' * 70)
        p('SPREAD (yes, active 300s):')
        p(f'  mean={sub.spread_mean.mean():.4f}  median-of-medians={sub.spread_med.median():.4f}')
        p(f'  frac time spread==1c: {sub.frac_spread_eq1c.mean():.3f}'
          f'   frac time spread>=2c: {sub.frac_spread_ge2c.mean():.3f}')
        p('TOP-OF-BOOK SIZE (shares, ~=USDC*2 at 0.5):')
        p(f'  bid median {sub.tob_bid_med.median():.1f}  ask median {sub.tob_ask_med.median():.1f}'
          f'   (p25 bid {sub.tob_bid_p25.median():.1f})')
        p(f'  fills/market median {sub.n_fills.median():.0f}  mean {sub.n_fills.mean():.1f}')

    if len(fd) == 0:
        p('\nNO FILLS')
        return

    p()
    p('=' * 70)
    p('FILL DYNAMICS + ADVERSE SELECTION  (mark-to-mid, signed PnL per fill, in $/share)')
    p('=' * 70)
    for label, sub in [('ALL FILLS', fd),
                       ('CALM&COINFLIP', fd[fd.calm & fd.coinflip]),
                       ('NON-CALM', fd[~fd.calm])]:
        if len(sub) == 0:
            continue
        p()
        p(f'[{label}] fills={len(sub)}  median clip={sub["size"].median():.1f} shares'
          f'  mean clip={sub["size"].mean():.1f}')
        for h in ADV_HORIZONS:
            c = f'mtm_{h}'
            p(f'  +{h:>4.0f}s mid move (signed, +=favourable): '
              f'mean={sub[c].mean()*100:+.3f}c  median={sub[c].median()*100:+.3f}c')
        # small-clip subset
        small = sub[sub['size'] <= 10]
        if len(small):
            p(f'  small clips (<=10 sh) n={len(small)}: '
              + '  '.join(f'+{h:.0f}s mean={small[f"mtm_{h}"].mean()*100:+.3f}c' for h in ADV_HORIZONS))

    p()
    p('=' * 70)
    p('NET MM ECONOMICS PER ROUND-TRIP (calm&coinflip), $/share captured')
    p('=' * 70)
    cc = fd[fd.calm & fd.coinflip]
    if len(cc):
        half_spread = cc.spread_at_fill.mean() / 2
        adv5 = cc['mtm_5.0'].mean()       # signed; negative = adverse
        adv5_small = cc[cc['size'] <= 10]['mtm_5.0'].mean() if (cc['size'] <= 10).any() else float('nan')
        # Polymarket: no taker fee for makers; maker rebate program ~ a few bps if active.
        p(f'  avg spread at fill: {cc.spread_at_fill.mean()*100:.3f}c -> half-spread captured ~{half_spread*100:.3f}c/share')
        p(f'  avg post-fill 5s mid move (adverse if neg): {adv5*100:+.3f}c/share (all clips)')
        p(f'                                  small clips: {adv5_small*100:+.3f}c/share')
        p(f'  net edge per single fill (half-spread + mtm5): {(half_spread+adv5)*100:+.3f}c/share (all)')
        p(f'                                   small clips : {(half_spread+adv5_small)*100:+.3f}c/share')
        # requote model: assume fast requote removes fills where mid had already moved against
        # us by >= REQUOTE_CANCEL_MOVE at +2s (proxy: filter on mtm_2 worst tail)
        worst = cc['mtm_2.0'] < -REQUOTE_CANCEL_MOVE
        kept = cc[~worst]
        p(f'  requote proxy: dropping fills with >0.5c adverse @2s removes {worst.mean()*100:.0f}% of fills;')
        if len(kept):
            p(f'                 remaining avg mtm5 = {kept["mtm_5.0"].mean()*100:+.3f}c -> '
              f'net {(half_spread+kept["mtm_5.0"].mean())*100:+.3f}c/share')

    p()
    p('=' * 70)
    p('TOXIC WINDOW (last 30s vs earlier), calm&coinflip')
    p('=' * 70)
    cc = fd[fd.calm & fd.coinflip]
    if len(cc):
        for label, sub in [('t>30s before close', cc[cc.t > TOXIC_SECS]),
                           ('last 30s', cc[cc.t <= TOXIC_SECS])]:
            if len(sub):
                p(f'  {label:>22}: n={len(sub):5d}  '
                  + '  '.join(f'mtm{h:.0f}={sub[f"mtm_{h}"].mean()*100:+.3f}c' for h in ADV_HORIZONS))


if __name__ == '__main__':
    main()
