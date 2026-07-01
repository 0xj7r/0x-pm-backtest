#!/usr/bin/env python3
"""Analyze split/merge/redeem/sell rolling-capital wallets on BTC 5m.

Reads data/runs/whales/<addr>.jsonl (from whale_pull.py).

Usage: python3 scripts/whale_split_redeem_analyze.py <address>
"""
from __future__ import annotations

import json
import re
import statistics as st
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path

UPDOWN = re.compile(r"^([a-z]+)-updown-(\d+)m-(\d+)$")


def load_rows(path: Path) -> list[dict]:
    seen: set[tuple] = set()
    rows: list[dict] = []
    if not path.exists():
        return rows
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        r = json.loads(line)
        key = (
            r.get("transactionHash"), r.get("type"), r.get("asset"),
            r.get("side"), r.get("size"), r.get("price"), r.get("timestamp"),
            r.get("slug"),
        )
        if key in seen:
            continue
        seen.add(key)
        rows.append(r)
    rows.sort(key=lambda x: x["timestamp"])
    return rows


def slug_meta(slug: str) -> dict | None:
    m = UPDOWN.match(slug or "")
    if not m:
        return None
    asset, mins, start = m.group(1), int(m.group(2)), int(m.group(3))
    return {
        "asset": asset,
        "horizon": f"{mins}m",
        "horizon_s": mins * 60,
        "wstart": start,
        "wclose": start + mins * 60,
    }


def main() -> int:
    addr = sys.argv[1].lower()
    path = Path(f"data/runs/whales/{addr}.jsonl")
    rows = load_rows(path)
    if not rows:
        print(f"no data at {path}")
        return 1

    ts0, ts1 = rows[0]["timestamp"], rows[-1]["timestamp"]
    span_d = (ts1 - ts0) / 86400
    print(f"# Wallet {addr}")
    print(f"events: {len(rows):,}  span: {span_d:.1f}d")
    print(f"first: {datetime.fromtimestamp(ts0, tz=timezone.utc).isoformat()}")
    print(f"last:  {datetime.fromtimestamp(ts1, tz=timezone.utc).isoformat()}")
    if rows[0].get("pseudonym"):
        print(f"profile: {rows[0].get('name')} / {rows[0].get('pseudonym')}")
    print()

    type_ct = Counter(r["type"] for r in rows)
    print("## Event mix")
    for t, n in type_ct.most_common():
        usd = sum(float(r.get("usdcSize") or 0) for r in rows if r["type"] == t)
        print(f"  {t:8s} {n:6,}  usdc=${usd:,.0f}")
    print()

    # BTC 5m only
    btc5 = [r for r in rows if (meta := slug_meta(r.get("slug", ""))) and meta["asset"] == "btc" and meta["horizon"] == "5m"]
    print(f"BTC 5m events: {len(btc5):,} ({100*len(btc5)/len(rows):.1f}% of all)")

    by_slug: dict[str, list[dict]] = defaultdict(list)
    for r in btc5:
        by_slug[r["slug"]].append(r)

    # Per-market ledger
    markets: list[dict] = []
    split_sizes = []
    redeem_sizes = []
    merge_sizes = []
    penny_sells = []
    roll_gaps = []  # seconds between redeem on N and split on N+1

    redeems_by_close: dict[int, dict] = {}
    splits_by_open: dict[int, dict] = {}

    for slug, evs in sorted(by_slug.items(), key=lambda kv: slug_meta(kv[0])["wstart"]):
        meta = slug_meta(slug)
        wstart, wclose = meta["wstart"], meta["wclose"]
        evs.sort(key=lambda x: x["timestamp"])

        split_usd = split_sh = 0.0
        merge_usd = merge_sh = 0.0
        redeem_usd = redeem_sh = 0.0
        buy_usd = sell_usd = 0.0
        buy_by_out: dict[str, list[float]] = defaultdict(list)
        sell_by_out: dict[str, list[float]] = defaultdict(list)
        first_ts = evs[0]["timestamp"]
        last_ts = evs[-1]["timestamp"]

        for r in evs:
            typ = r["type"]
            usd = float(r.get("usdcSize") or 0)
            sh = float(r.get("size") or 0)
            if typ == "SPLIT":
                split_usd += usd
                split_sh += sh
            elif typ == "MERGE":
                merge_usd += usd
                merge_sh += sh
            elif typ == "REDEEM":
                redeem_usd += usd
                redeem_sh += sh
            elif typ == "TRADE":
                if r.get("side") == "BUY":
                    buy_usd += usd
                    buy_by_out[r.get("outcome", "?")].append(float(r.get("price") or 0))
                elif r.get("side") == "SELL":
                    sell_usd += usd
                    px = float(r.get("price") or 0)
                    sell_by_out[r.get("outcome", "?")].append(px)
                    if px <= 0.02:
                        penny_sells.append({"slug": slug, "usd": usd, "sh": sh, "px": px, "out": r.get("outcome")})

        if split_sh:
            split_sizes.append(split_sh)
        if redeem_sh:
            redeem_sizes.append(redeem_sh)
            redeems_by_close[wclose] = {"ts": last_ts if redeem_usd else evs[-1]["timestamp"], "usd": redeem_usd, "slug": slug}
        if merge_sh:
            merge_sizes.append(merge_sh)
        if split_usd:
            splits_by_open[wstart] = {"ts": first_ts, "usd": split_usd, "slug": slug}

        # Cash PnL: money in - money out (split is outflow, redeem/merge/sell are inflow)
        cash_in = redeem_usd + merge_usd + sell_usd
        cash_out = split_usd + buy_usd
        pnl = cash_in - cash_out

        has_split = split_usd > 0
        has_redeem = redeem_usd > 0
        has_merge = merge_usd > 0
        has_sell = sell_usd > 0
        has_buy = buy_usd > 0

        markets.append({
            "slug": slug,
            "wstart": wstart,
            "split_usd": split_usd,
            "split_sh": split_sh,
            "redeem_usd": redeem_usd,
            "merge_usd": merge_usd,
            "sell_usd": sell_usd,
            "buy_usd": buy_usd,
            "pnl": pnl,
            "has_split": has_split,
            "has_redeem": has_redeem,
            "has_merge": has_merge,
            "has_sell": has_sell,
            "has_buy": has_buy,
            "n_events": len(evs),
            "buy_outcomes": list(buy_by_out.keys()),
            "sell_outcomes": list(sell_by_out.keys()),
            "penny_sell_usd": sum(x["usd"] for x in penny_sells if x["slug"] == slug),
        })

    # Rolling redeem -> next split timing
    sorted_opens = sorted(splits_by_open)
    for wstart in sorted_opens:
        prev_close = wstart  # 5m windows: close of N == open of N+1 for slug ts
        if prev_close in redeems_by_close:
            gap = splits_by_open[wstart]["ts"] - redeems_by_close[prev_close]["ts"]
            roll_gaps.append(gap)

    n_mkts = len(markets)
    split_mkts = sum(1 for m in markets if m["has_split"])
    redeem_mkts = sum(1 for m in markets if m["has_redeem"])
    merge_mkts = sum(1 for m in markets if m["has_merge"])
    sell_mkts = sum(1 for m in markets if m["has_sell"])
    buy_mkts = sum(1 for m in markets if m["has_buy"])

    print(f"\n## BTC 5m markets touched: {n_mkts}")
    print(f"  with SPLIT: {split_mkts}  REDEEM: {redeem_mkts}  MERGE: {merge_mkts}")
    print(f"  with TRADE buy: {buy_mkts}  TRADE sell: {sell_mkts}")

    if split_sizes:
        print(f"\n## Split size distribution (shares)")
        print(f"  n={len(split_sizes)} median={st.median(split_sizes):,.0f} "
              f"mode~={Counter(round(s) for s in split_sizes).most_common(1)[0]}")
    if redeem_sizes:
        print(f"## Redeem size distribution (shares)")
        print(f"  n={len(redeem_sizes)} median={st.median(redeem_sizes):,.0f}")

    if roll_gaps:
        print(f"\n## Redeem→next-Split latency (sec)")
        print(f"  n={len(roll_gaps)} median={st.median(roll_gaps):.0f} "
              f"p90={sorted(roll_gaps)[int(0.9*len(roll_gaps))]:.0f} "
              f"min={min(roll_gaps):.0f} max={max(roll_gaps):.0f}")

    total_pnl = sum(m["pnl"] for m in markets)
    total_split = sum(m["split_usd"] for m in markets)
    total_redeem = sum(m["redeem_usd"] for m in markets)
    total_merge = sum(m["merge_usd"] for m in markets)
    total_sell = sum(m["sell_usd"] for m in markets)
    total_buy = sum(m["buy_usd"] for m in markets)

    print(f"\n## Cash ledger (BTC 5m, all markets in sample)")
    print(f"  split out:  ${total_split:,.0f}")
    print(f"  buy out:    ${total_buy:,.0f}")
    print(f"  redeem in:  ${total_redeem:,.0f}")
    print(f"  merge in:   ${total_merge:,.0f}")
    print(f"  sell in:    ${total_sell:,.0f}")
    print(f"  net PnL:    ${total_pnl:+,.0f}")

    # Monthly
    print(f"\n## Monthly net PnL (BTC 5m)")
    by_month: dict[str, float] = defaultdict(float)
    by_month_mkts: dict[str, int] = defaultdict(int)
    for m in markets:
        mon = datetime.fromtimestamp(m["wstart"], tz=timezone.utc).strftime("%Y-%m")
        by_month[mon] += m["pnl"]
        by_month_mkts[mon] += 1
    for mon in sorted(by_month):
        print(f"  {mon}: ${by_month[mon]:+,.0f}  ({by_month_mkts[mon]} markets)")

    # Strategy archetype counts
    archetypes = Counter()
    for m in markets:
        if m["has_split"] and m["has_redeem"] and not m["has_buy"] and not m["has_sell"]:
            archetypes["pure_split_redeem_roll"] += 1
        elif m["has_split"] and m["has_sell"] and m["has_redeem"]:
            archetypes["split_sell_loser_redeem_winner"] += 1
        elif m["has_split"] and m["has_merge"]:
            archetypes["split_merge_exit"] += 1
        elif m["has_buy"]:
            archetypes["active_trading"] += 1
        elif m["has_split"]:
            archetypes["split_only_or_pending"] += 1
        else:
            archetypes["other"] += 1

    print(f"\n## Per-market archetypes")
    for k, v in archetypes.most_common():
        print(f"  {k}: {v}")

    if penny_sells:
        print(f"\n## Penny sells (≤2¢)")
        print(f"  count: {len(penny_sells)}  total_usd: ${sum(p['usd'] for p in penny_sells):,.2f}")
        out_ct = Counter(p["out"] for p in penny_sells)
        print(f"  outcomes: {dict(out_ct)}")
        sh_ct = Counter(round(p["sh"]) for p in penny_sells)
        print(f"  share clips: {sh_ct.most_common(5)}")

    # Win rate proxy: redeem ~1111 means held winning side from prior split
    # Markets where they redeemed without splitting in same window = capital from prev window
    redeem_only = [m for m in markets if m["has_redeem"] and not m["has_split"]]
    split_only = [m for m in markets if m["has_split"] and not m["has_redeem"]]
    both = [m for m in markets if m["has_split"] and m["has_redeem"]]
    print(f"\n## Same-window split+redeem: {len(both)} (should be ~0 for pure roller)")
    print(f"  redeem-only windows: {len(redeem_only)}  split-only: {len(split_only)}")

    # Capital efficiency: implied bankroll
    if split_sizes:
        typical_clip = st.median(split_sizes)
        print(f"\n## Implied operating clip")
        print(f"  median split: {typical_clip:,.0f} shares (${typical_clip:,.0f} USDC per window)")
        print(f"  windows/day (if 24h): ~288  gross roll vol/day: ${typical_clip * 288:,.0f}")

    # Top/bottom markets by pnl
    ranked = sorted(markets, key=lambda m: m["pnl"])
    print(f"\n## Worst 5 markets by net cash")
    for m in ranked[:5]:
        print(f"  {m['slug']}: pnl=${m['pnl']:+,.0f} split=${m['split_usd']:,.0f} "
              f"redeem=${m['redeem_usd']:,.0f} sell=${m['sell_usd']:,.0f}")
    print(f"## Best 5 markets by net cash")
    for m in ranked[-5:][::-1]:
        print(f"  {m['slug']}: pnl=${m['pnl']:+,.0f} split=${m['split_usd']:,.0f} "
              f"redeem=${m['redeem_usd']:,.0f} sell=${m['sell_usd']:,.0f}")

    # Write markdown report
    out = Path(f"data/runs/whales/{addr}_split_redeem.md")
    out.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        f"# Split/redeem analysis: `{addr}`",
        f"",
        f"- Events: {len(rows):,} over {span_d:.1f}d",
        f"- BTC 5m markets: {n_mkts}",
        f"- Net PnL (cash ledger): **${total_pnl:+,.0f}**",
        f"- Archetype: rolling **${st.median(split_sizes) if split_sizes else 0:,.0f}** split → hold → redeem next window",
        f"",
        f"## Event mix",
    ]
    for t, n in type_ct.most_common():
        lines.append(f"- {t}: {n:,}")
    lines.extend(["", "## Archetypes"] + [f"- {k}: {v}" for k, v in archetypes.most_common()])
    lines.extend(["", "## Monthly PnL"] + [f"- {m}: ${by_month[m]:+,.0f}" for m in sorted(by_month)])
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    print(f"\nwrote {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())