#!/usr/bin/env python3
"""Anatomy of consecutive-loss clusters and leading signals on harness trade tapes.

Identifies when losses cluster, what the blocked (loss2-gate) trades look like,
and which entry-time features precede streak starts.

Usage:
  python3 scripts/loss_cluster_analyze.py data/runs/strategy_validation/HOLDOUT_baseline.trades.jsonl
  python3 scripts/loss_cluster_analyze.py data/runs/strategy_validation/VERIFY_baseline.trades.jsonl
"""

from __future__ import annotations

import json
import sys
from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from statistics import median


def load(path: Path) -> list[dict]:
    rows = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
    return sorted(rows, key=lambda t: int(t.get("decision_ts_ns") or 0))


def p_side(t: dict) -> float:
    p = float(t["p_exo"])
    return p if t["side"] == "Yes" else 1.0 - p


def side_label(t: dict) -> str:
    return "up" if t["side"] == "Yes" else "down"


def model_book_gap(t: dict) -> float:
    ask = float(t.get("side_ask_at_decision") or t.get("avg_price") or 0)
    return p_side(t) - ask


def won(t: dict) -> bool:
    return float(t["pnl"]) > 0


def fmt_usd(x: float) -> str:
    return f"${x:+,.0f}"


def summarize(trades: list[dict], label: str) -> dict:
    n = len(trades)
    pnl = sum(float(t["pnl"]) for t in trades)
    hit = sum(1 for t in trades if won(t)) / n if n else 0.0
    print(
        f"  {label:38s}  n={n:5d}  NET={fmt_usd(pnl):>10s}  "
        f"hit={hit * 100:5.1f}%  $/tr={pnl / n if n else 0:6.2f}"
    )
    return {"n": n, "net": pnl, "hit": hit}


@dataclass
class AnnotatedTrade:
    trade: dict
    idx: int
    consec_before: int = 0
    consec_after: int = 0
    cluster_id: int | None = None
    pos_in_cluster: int | None = None
    blocked_by_loss2: bool = False
    starts_cluster: bool = False
    is_second_loss: bool = False


@dataclass
class Cluster:
    id: int
    trades: list[AnnotatedTrade] = field(default_factory=list)

    @property
    def length(self) -> int:
        return len(self.trades)

    @property
    def net(self) -> float:
        return sum(float(a.trade["pnl"]) for a in self.trades)


def annotate(trades: list[dict]) -> tuple[list[AnnotatedTrade], list[Cluster]]:
    ann: list[AnnotatedTrade] = []
    clusters: list[Cluster] = []
    consec = 0
    cluster_id = 0
    current: Cluster | None = None

    for i, t in enumerate(trades):
        blocked = consec >= 2
        a = AnnotatedTrade(
            trade=t,
            idx=i,
            consec_before=consec,
            blocked_by_loss2=blocked,
        )
        if blocked:
            # Gate skips this trade; streak still advances on baseline path.
            pass
        else:
            if not won(t):
                if consec == 0:
                    a.starts_cluster = True
                    cluster_id += 1
                    current = Cluster(id=cluster_id)
                    clusters.append(current)
                a.is_second_loss = consec == 1
                if current is not None:
                    a.cluster_id = current.id
                    a.pos_in_cluster = len(current.trades) + 1
                    current.trades.append(a)
            else:
                current = None

        ann.append(a)
        consec = 0 if won(t) else consec + 1
        a.consec_after = consec

    return ann, clusters


def prior_trade(ann: list[AnnotatedTrade], i: int) -> AnnotatedTrade | None:
    return ann[i - 1] if i > 0 else None


def gap_minutes(a: AnnotatedTrade, b: AnnotatedTrade) -> float:
    ta = int(a.trade.get("decision_ts_ns") or 0)
    tb = int(b.trade.get("decision_ts_ns") or 0)
    return (tb - ta) / 1e9 / 60.0


def bucket_sigma(t: dict) -> str:
    s = float(t.get("sigma_bar_bps") or 0)
    if s < 3:
        return "sigma_<3"
    if s < 5:
        return "sigma_3-5"
    if s < 8:
        return "sigma_5-8"
    return "sigma_>=8"


def bucket_spot(t: dict) -> str:
    v = t.get("spot_ret_30s_bps")
    if v is None:
        return "spot_na"
    v = float(v)
    if v < -2:
        return "spot_adverse_<-2"
    if v < 2:
        return "spot_flat_-2_2"
    return "spot_with_>=2"


def bucket_gap(t: dict) -> str:
    g = model_book_gap(t)
    if g < 0.10:
        return "gap_<0.10"
    if g < 0.25:
        return "gap_0.10-0.25"
    return "gap_>=0.25"


def bucket_secs(t: dict) -> str:
    s = t.get("secs_from_open")
    if s is None:
        return "open_na"
    s = int(s)
    if s <= 30:
        return "open_0-30s"
    if s <= 90:
        return "open_31-90s"
    return "open_91s+"


def ask_drop_from_trail(t: dict) -> float | None:
    ask = float(t.get("side_ask_at_decision") or 0)
    trail = t.get("trail_min_ask_20s")
    if not ask or trail is None:
        return None
    return ask - float(trail)


def section(title: str) -> None:
    print(f"\n## {title}")


def cluster_lengths(clusters: list[Cluster]) -> None:
    section("Loss cluster sizes (consecutive losses)")
    hist: dict[int, int] = defaultdict(int)
    for c in clusters:
        hist[c.length] += 1
    total = len(clusters)
    for k in sorted(hist):
        print(f"  length {k}: {hist[k]} clusters ({100 * hist[k] / total:.1f}%)")
    nets = [c.net for c in clusters]
    if nets:
        print(
            f"  cluster NET: median={fmt_usd(median(nets))}  "
            f"worst={fmt_usd(min(nets))}  total={fmt_usd(sum(nets))}"
        )


def blocked_trades(ann: list[AnnotatedTrade]) -> None:
    section("Trades blocked by pause-after-2-losses")
    blocked = [a.trade for a in ann if a.blocked_by_loss2]
    taken = [a.trade for a in ann if not a.blocked_by_loss2]
    summarize(taken, "taken (baseline)")
    summarize(blocked, "blocked (would skip)")
    if blocked:
        bl = sum(1 for t in blocked if not won(t))
        print(f"  blocked loss rate: {100 * bl / len(blocked):.1f}%  (gate removes mostly losers)")


def cluster_regime_mix(clusters: list[Cluster]) -> None:
    section("Regime mix inside loss clusters (by loss leg)")
    by: dict[str, list[dict]] = defaultdict(list)
    for c in clusters:
        for a in c.trades:
            by[str(a.trade.get("regime") or "unknown")].append(a.trade)
    for k in sorted(by):
        summarize(by[k], k)


def side_flip_in_clusters(clusters: list[Cluster]) -> None:
    section("Side flips within clusters")
    flips = 0
    same = 0
    for c in clusters:
        for i in range(1, len(c.trades)):
            if side_label(c.trades[i].trade) != side_label(c.trades[i - 1].trade):
                flips += 1
            else:
                same += 1
    n = flips + same
    if n:
        print(f"  adjacent loss pairs: {n}  flip={flips} ({100 * flips / n:.1f}%)  same={same}")


def timing_in_clusters(clusters: list[Cluster]) -> None:
    section("Minutes between adjacent losses in a cluster")
    gaps: list[float] = []
    for c in clusters:
        for i in range(1, len(c.trades)):
            gaps.append(gap_minutes(c.trades[i - 1], c.trades[i]))
    if not gaps:
        return
    gaps.sort()
    print(
        f"  n={len(gaps)}  median={median(gaps):.1f}m  "
        f"p25={gaps[len(gaps) // 4]:.1f}m  p75={gaps[3 * len(gaps) // 4]:.1f}m  "
        f"<=10m={sum(1 for g in gaps if g <= 10)} ({100 * sum(1 for g in gaps if g <= 10) / len(gaps):.1f}%)"
    )


def compare_roles(ann: list[AnnotatedTrade]) -> None:
    section("Feature compare: 1st loss vs 2nd loss in cluster")
    first = [a.trade for a in ann if a.starts_cluster]
    second = [a.trade for a in ann if a.is_second_loss]
    winners = [a.trade for a in ann if won(a.trade)]
    summarize(winners, "all winners (baseline)")
    summarize(first, "starts loss cluster (1st loss)")
    summarize(second, "2nd consecutive loss")
    for label, subset in [
        ("1st loss", first),
        ("2nd loss", second),
    ]:
        if not subset:
            continue
        print(f"\n  {label} breakdown:")
        for name, fn in [
            ("regime", lambda t: str(t.get("regime") or "?")),
            ("sigma", bucket_sigma),
            ("spot_30s", bucket_spot),
            ("gap", bucket_gap),
            ("open_secs", bucket_secs),
        ]:
            by: dict[str, list[dict]] = defaultdict(list)
            for t in subset:
                by[fn(t)].append(t)
            top = sorted(by.items(), key=lambda kv: -len(kv[1]))[:4]
            parts = ", ".join(f"{k}:{len(v)}" for k, v in top)
            print(f"    {name}: {parts}")


def leading_signals(ann: list[AnnotatedTrade]) -> None:
    """Signals on the trade *before* a cluster-starting loss."""
    section("Leading signals: prior trade before cluster-starting loss")
    pairs: list[tuple[AnnotatedTrade, AnnotatedTrade]] = []
    for a in ann:
        if not a.starts_cluster:
            continue
        prev = prior_trade(ann, a.idx)
        if prev is not None:
            pairs.append((prev, a))

    if not pairs:
        print("  (no prior-trade pairs)")
        return

    # Prior winner that precedes a loss cluster
    prev_won = [p for p, loss in pairs if won(p.trade)]
    prev_lost = [p for p, loss in pairs if not won(p.trade)]
    print(f"  cluster starters with prior trade: {len(pairs)}")
    print(f"    prior was win: {len(prev_won)}  prior was loss: {len(prev_lost)}")

    def prior_mark_adverse(p: AnnotatedTrade) -> bool:
        m = p.trade.get("mark_60s")
        if m is None:
            return False
        # mark_60s is side-oriented mid; if we bought Yes, low mark = adverse
        side = p.trade["side"]
        mid_dec = float(p.trade.get("mid_at_decision") or 0)
        mark = float(m)
        if side == "Yes":
            return mark < mid_dec - 0.03
        return mark > mid_dec + 0.03

    adverse_mark = sum(1 for p, _ in pairs if prior_mark_adverse(p))
    print(
        f"    prior book moved against within 60s: {adverse_mark} "
        f"({100 * adverse_mark / len(pairs):.1f}%)"
    )

    opp_after_prev_win = 0
    for p, loss in pairs:
        if won(p.trade) and side_label(p.trade) != side_label(loss.trade):
            opp_after_prev_win += 1
    print(
        f"    opp-side entry after prior win: {opp_after_prev_win} "
        f"({100 * opp_after_prev_win / len(pairs):.1f}%)"
    )

    section("Next-trade outcome after prior win (leading indicator)")
    after_win: list[dict] = []
    after_win_mark_adverse: list[dict] = []
    after_win_mark_favorable: list[dict] = []
    after_win_opp_side: list[dict] = []
    after_win_same_side: list[dict] = []

    def mark_adverse(p: dict) -> bool:
        m = p.get("mark_60s")
        if m is None:
            return False
        mid_dec = float(p.get("mid_at_decision") or 0)
        mark = float(m)
        if p["side"] == "Yes":
            return mark < mid_dec - 0.03
        return mark > mid_dec + 0.03

    for i, a in enumerate(ann):
        if i == 0:
            continue
        prev = ann[i - 1].trade
        if not won(prev):
            continue
        after_win.append(a.trade)
        if mark_adverse(prev):
            after_win_mark_adverse.append(a.trade)
        else:
            after_win_mark_favorable.append(a.trade)
        if side_label(prev) != side_label(a.trade):
            after_win_opp_side.append(a.trade)
        else:
            after_win_same_side.append(a.trade)

    summarize(after_win, "after any prior win")
    summarize(after_win_mark_adverse, "after win + mark_60s adverse")
    summarize(after_win_mark_favorable, "after win + mark_60s ok")
    summarize(after_win_opp_side, "after win + opp-side entry")
    summarize(after_win_same_side, "after win + same-side entry")

    section("Cluster-start loss rate by feature on *entry* (all trades)")
    all_trades = [a.trade for a in ann]
    starter_idxs = {a.idx for a in ann if a.starts_cluster}

    def loss_rate(subset: list[dict]) -> float:
        if not subset:
            return 0.0
        return sum(1 for t in subset if not won(t)) / len(subset)

    def cluster_start_rate(indices: list[int]) -> float:
        if not indices:
            return 0.0
        return sum(1 for i in indices if i in starter_idxs) / len(indices)

    base_lr = loss_rate(all_trades)
    base_csr = cluster_start_rate(list(range(len(ann))))
    print(f"  baseline loss rate: {100 * base_lr:.1f}%  cluster-start rate: {100 * base_csr:.1f}%")
    print(f"  {'feature':28s}  {'n':>6s}  {'loss%':>6s}  {'start%':>7s}  {'lift':>5s}")

    def scan(name: str, fn) -> None:
        by: dict[str, list[int]] = defaultdict(list)
        for i, t in enumerate(all_trades):
            by[fn(t)].append(i)
        for k in sorted(by):
            idxs = by[k]
            sub = [all_trades[i] for i in idxs]
            lr = loss_rate(sub)
            csr = cluster_start_rate(idxs)
            lift = (lr / base_lr) if base_lr else 0
            print(
                f"  {name + ':' + k:28s}  {len(sub):6d}  "
                f"{100 * lr:5.1f}%  {100 * csr:6.1f}%  {lift:5.2f}x"
            )

    scan("regime", lambda t: str(t.get("regime") or "?"))
    scan("sigma", bucket_sigma)
    scan("spot30", bucket_spot)
    scan("gap", bucket_gap)
    scan("open", bucket_secs)

    section("Second-loss rate conditional on first loss features")
    first_losses = [a for a in ann if a.starts_cluster]
    second_losses = {a.idx for a in ann if a.is_second_loss}
    # map first loss idx -> whether next taken trade is second loss
    by_first: dict[str, list[bool]] = defaultdict(list)
    for a in first_losses:
        nxt = ann[a.idx + 1] if a.idx + 1 < len(ann) else None
        became_second = nxt is not None and nxt.idx in second_losses
        for name, fn in [
            ("regime", lambda t: str(t.get("regime") or "?")),
            ("sigma", bucket_sigma),
            ("spot30", bucket_spot),
            ("gap", bucket_gap),
            ("flip", lambda t: "flip" if prior_trade(ann, a.idx) and side_label(t) != side_label(prior_trade(ann, a.idx).trade) else "same"),
        ]:
            by_first[f"{name}:{fn(a.trade)}"].append(became_second)
    rows = []
    for k, vals in by_first.items():
        if len(vals) < 30:
            continue
        rate = sum(vals) / len(vals)
        rows.append((rate, k, len(vals)))
    rows.sort(reverse=True)
    print(f"  {'condition':32s}  {'n':>5s}  {'->2nd loss%':>11s}")
    for rate, k, n in rows[:12]:
        print(f"  {k:32s}  {n:5d}  {100 * rate:10.1f}%")
    print("  (bottom — lower 2nd-loss continuation)")
    for rate, k, n in rows[-6:]:
        print(f"  {k:32s}  {n:5d}  {100 * rate:10.1f}%")


def hour_of_day_clusters(ann: list[AnnotatedTrade]) -> None:
    section("UTC hour — cluster-starting losses vs all losses")
    all_losses_by_h: dict[int, int] = defaultdict(int)
    starter_by_h: dict[int, int] = defaultdict(int)
    for a in ann:
        if won(a.trade):
            continue
        ts = int(a.trade.get("decision_ts_ns") or 0) / 1e9
        h = datetime.fromtimestamp(ts, tz=timezone.utc).hour
        all_losses_by_h[h] += 1
        if a.starts_cluster:
            starter_by_h[h] += 1
    print(f"  {'hour':>4s}  {'losses':>6s}  {'starters':>8s}  {'starter%':>8s}")
    for h in range(24):
        if all_losses_by_h[h]:
            print(
                f"  {h:4d}  {all_losses_by_h[h]:6d}  {starter_by_h[h]:8d}  "
                f"{100 * starter_by_h[h] / all_losses_by_h[h]:7.1f}%"
            )


def main() -> None:
    path = Path(sys.argv[1])
    trades = load(path)
    ann, clusters = annotate(trades)
    print(f"# Loss cluster analysis — {path.name}\n")
    summarize(trades, "ALL TRADES")
    print(f"  loss clusters: {len(clusters)}  legs in clusters: {sum(c.length for c in clusters)}")
    cluster_lengths(clusters)
    blocked_trades(ann)
    cluster_regime_mix(clusters)
    side_flip_in_clusters(clusters)
    timing_in_clusters(clusters)
    compare_roles(ann)
    leading_signals(ann)
    hour_of_day_clusters(ann)
    print("\n## Interpretation hints")
    print("- High flip% + short gaps → whipsaw session; loss2 pause is a circuit breaker.")
    print("- Leading signal = feature on entry where loss% and cluster-start% lift vs baseline.")
    print("- Prior mark_60s adverse after a win → book already fading before next entry.")


if __name__ == "__main__":
    main()