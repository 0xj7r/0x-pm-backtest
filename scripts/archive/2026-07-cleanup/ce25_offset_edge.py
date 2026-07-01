#!/usr/bin/env python3
"""Net edge by time-into-window and price band, btc 5m and 15m."""
import json, re
from collections import defaultdict

slug_re = re.compile(r"^(.*?)-(\d{9,11})$")
mkts = {}
for line in open('/Users/jackreid/go/polymarket-backtest/data/external/whale_ce25/activity_raw.jsonl'):
    r = json.loads(line); t = r['type']
    cid = r['conditionId']
    m = mkts.setdefault(cid, {'b': [0.0, 0.0], 'red': 0.0, 'buys': [], 'ser': None, 'ms': None})
    if m['ser'] is None:
        mm = slug_re.match(r.get('slug') or '')
        if mm: m['ser'], m['ms'] = mm.group(1), int(mm.group(2))
    if t == 'TRADE' and r.get('side') == 'BUY':
        px = float(r['price']); sz = float(r['size']); u = float(r['usdcSize'])
        m['b'][r['outcomeIndex']] += sz
        m['buys'].append((px, sz, u, r['outcomeIndex'], r['timestamp']))
    elif t == 'REDEEM':
        m['red'] += float(r['usdcSize'])

# (series, offset_bucket, band) -> [n, stake_cash, net_pnl]
agg = defaultdict(lambda: [0, 0.0, 0.0])
for cid, m in mkts.items():
    if m['red'] <= 0 or m['ms'] is None: continue
    if m['ser'] not in ('btc-updown-5m', 'btc-updown-15m'): continue
    b0, b1 = m['b']
    woi = 0 if abs(m['red'] - b0) <= abs(m['red'] - b1) else 1
    horizon = 300 if m['ser'].endswith('-5m') else 900
    for px, sz, u, oi, ts in m['buys']:
        off = ts - m['ms']
        ob = min(int(off / (horizon / 5)), 4)  # quintiles of window
        band = 'lo<0.10' if px < 0.10 else ('mid0.10-0.55' if px < 0.55 else ('hi0.55-0.88' if px < 0.88 else 'fav>=0.88'))
        st = agg[(m['ser'], ob, band)]
        net = sz - u if oi == woi else -u
        st[0] += 1; st[1] += u; st[2] += net

print('series           window-quintile band            n        $stake     net$   net/$')
for k in sorted(agg):
    ser, ob, band = k
    n, stk, net = agg[k]
    print('%-16s q%d              %-14s %7d %12.0f %8.0f %7.2f%%' % (ser, ob, band, n, stk, net, 100 * net / stk))
