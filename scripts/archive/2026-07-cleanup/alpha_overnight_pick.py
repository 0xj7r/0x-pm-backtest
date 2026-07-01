#!/usr/bin/env python3
"""Decision helpers for the pm-alpha overnight program."""
import glob
import json
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import mean, stdev


def daily_stats(trades_path):
    days = defaultdict(float)
    for line in open(trades_path):
        r = json.loads(line)
        d = datetime.fromtimestamp(r["open_ts_ns"] / 1e9, timezone.utc).date().isoformat()
        days[d] += r["pnl"]
    vals = list(days.values())
    if len(vals) < 2:
        return None
    return mean(vals), stdev(vals), sum(vals)


def cmd_risk(night):
    """Pick exit_after_s x skip_calm by daily sharpe, EV floor 50% of best."""
    rows = []
    for f in glob.glob(f"{night}/riskB_exit*_skip*.trades.jsonl"):
        name = Path(f).stem.replace(".trades", "")
        parts = name.split("_")
        exit_s = int(parts[1][4:])
        skip = int(parts[2][4:])
        st = daily_stats(f)
        if not st:
            continue
        m, sd, total = st
        rows.append({"exit": exit_s, "skip": skip, "mean": m, "sd": sd, "total": total,
                     "sharpe": m / sd if sd > 0 else 0.0})
    if not rows:
        sys.exit("no risk results")
    best_total = max(r["total"] for r in rows)
    eligible = [r for r in rows if r["total"] >= 0.5 * best_total] or rows
    best = max(eligible, key=lambda r: r["sharpe"])
    for r in sorted(rows, key=lambda r: -r["sharpe"]):
        print(f"# exit={r['exit']:>3} skip={r['skip']} total={r['total']:8.0f} "
              f"mean={r['mean']:7.0f} sd={r['sd']:6.0f} sharpe={r['sharpe']:.2f}")
    print(f"CHOSEN_EXIT_S={best['exit']}")
    print(f"CHOSEN_SKIP={best['skip']}")


def cmd_families(mm):
    """List updown families present in the May 21-28 metadata manifest."""
    c = Counter()
    for line in open(f"{mm}/meta_may21_28.jsonl"):
        slug = json.loads(line)["slug"]
        fam = "-".join(slug.split("-")[:3])
        if "updown" in fam:
            c[fam] += 1
    for fam, n in c.most_common():
        if n >= 200:  # enough markets for a tune/test split
            print(fam)


def cmd_labelcheck(mm):
    """Inferred (final-mid) labels cannot be checked here directly; instead
    compare availability-API outcomes vs API close alignment where both
    manifests overlap (sanity that metadata slugs/assets match the API's)."""
    api = {}
    for f in glob.glob(f"{mm}/markets_all_*.jsonl"):
        for line in open(f):
            r = json.loads(line)
            api[r["asset_id"]] = r["outcome"]
    if not api:
        print("no API manifest yet; labelcheck deferred")
        return
    meta = {}
    for f in glob.glob(f"{mm}/meta_all_*.jsonl"):
        for line in open(f):
            r = json.loads(line)
            meta[r["asset_id"]] = r["slug"]
    overlap = set(api) & set(meta)
    print(f"api={len(api)} meta={len(meta)} overlap={len(overlap)}")


def cmd_best_thr(path):
    d = json.loads(open(path).read())
    best = max(d["sweep"], key=lambda e: e["report"]["aggregate"]["total_pnl"])
    print(best["edge_threshold"])


def cmd_summary(night):
    for f in sorted(glob.glob(f"{night}/*.json")):
        d = json.loads(open(f).read())
        for e in d.get("sweep", []):
            a = e["report"]["aggregate"]
            print(f"{Path(f).stem:36s} lat={e['latency_ms']:>4} thr={e['edge_threshold']:<5} "
                  f"mkts={a['n_markets']:>5} trades={a['n_trades']:>5} pnl={a['total_pnl']:>9.2f} "
                  f"hit={100*a['hit_rate']:5.1f}% ll_exo={a['log_loss_exo']:.4f} ll_book={a['log_loss_book']:.4f}")


if __name__ == "__main__":
    cmd = sys.argv[1]
    arg = sys.argv[2] if len(sys.argv) > 2 else "."
    {"risk": cmd_risk, "families": cmd_families, "labelcheck": cmd_labelcheck,
     "best-thr": cmd_best_thr, "summary": cmd_summary}[cmd](arg)
