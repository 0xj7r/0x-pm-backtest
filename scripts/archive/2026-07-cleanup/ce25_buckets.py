#!/usr/bin/env python3
import json
from collections import defaultdict
from datetime import datetime, timezone

def week(ts): return datetime.fromtimestamp(ts, timezone.utc).strftime('%G-W%V')

mkts = {}
maker_w = defaultdict(float); tot_w = defaultdict(float)
for line in open('/Users/jackreid/go/polymarket-backtest/data/external/whale_ce25/activity_raw.jsonl'):
    r = json.loads(line); t = r['type']
    cid = r['conditionId']
    m = mkts.setdefault(cid, {'b': [0.0, 0.0], 'red': 0.0, 'buys': []})
    if t == 'TRADE' and r.get('side') == 'BUY':
        px = float(r['price']); sz = float(r['size']); u = float(r['usdcSize']); oi = r['outcomeIndex']
        m['b'][oi] += sz
        f = u - px * sz
        is_maker = f < 0.0005 or f < 0.2 * (0.07 * px * (1 - px) * sz)
        m['buys'].append((px, sz, u, oi, is_maker))
        w = week(r['timestamp']); tot_w[w] += px * sz
        if is_maker: maker_w[w] += px * sz
    elif t == 'REDEEM':
        m['red'] += float(r['usdcSize'])

bucket = defaultdict(lambda: [0, 0, 0.0, 0.0, 0.0])
mk = [0, 0, 0.0, 0.0]; tk = [0, 0, 0.0, 0.0]
maj_win = 0; maj_n = 0
for cid, m in mkts.items():
    if m['red'] <= 0: continue
    b0, b1 = m['b']
    woi = 0 if abs(m['red'] - b0) <= abs(m['red'] - b1) else 1
    if b0 > 0 and b1 > 0 and abs(b0 - b1) / max(b0, b1) > 0.05:
        maj_n += 1
        heavy = 0 if b0 > b1 else 1
        if heavy == woi: maj_win += 1
    for px, sz, u, oi, ismk in m['buys']:
        bk = round(px, 1); st = bucket[bk]
        won = (oi == woi)
        st[0] += 1; st[2] += px * sz
        gross = (1 - px) * sz if won else -px * sz
        net = sz - u if won else -u
        st[3] += gross; st[4] += net
        if won: st[1] += 1
        tgt = mk if ismk else tk
        tgt[0] += 1; tgt[2] += net; tgt[3] += u
        if won: tgt[1] += 1

print('majority-side wins: %d/%d = %.1f%%' % (maj_win, maj_n, 100 * maj_win / maj_n))
print()
print('bucket      n    hit%  impl%   gross_pnl    net_pnl   gross/$   net/$')
for b in sorted(bucket):
    n, wins, stk, g, nt = bucket[b]
    print('%5.1f %8d  %5.1f  %5.0f  %10.0f %10.0f   %6.2f%%  %6.2f%%' % (b, n, 100 * wins / n, 100 * b, g, nt, 100 * g / stk, 100 * nt / stk))
print()
n, wn, np_, stk = mk
print('maker fills: n=%d hit=%.1f%% net_pnl=%.0f net_per_dollar=%.2f%%' % (n, 100 * wn / n, np_, 100 * np_ / stk))
n, wn, np_, stk = tk
print('taker fills: n=%d hit=%.1f%% net_pnl=%.0f net_per_dollar=%.2f%%' % (n, 100 * wn / n, np_, 100 * np_ / stk))
print()
print('maker share of buy notional by week:')
for w in sorted(tot_w):
    print(' %s %.1f%%' % (w, 100 * maker_w[w] / tot_w[w]))
