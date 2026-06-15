//! f11: deep cheap-tail taker EV scan.
//!
//! Offline analysis from the merged BookTick tape cache. For each BTC-5m and
//! BTC-15m window in W3 (2026-05-07..05-18), scan the final 40/20/10% of the
//! window for any side whose ask <= 0.05. Record the realized win rate of that
//! cheap side versus the fee-inclusive priced rate, bucketed by price and
//! time-to-close, then sim a small-clip taker P&L held to redemption.
//!
//! Tape model: the YES tape carries the Up-token book (yes_bid/yes_ask) and,
//! when the .2s. (has_down) file exists, the real Down-token book
//! (no_bid/no_ask). A cheap YES ask means "Up nearly dead"; a cheap NO ask
//! means "Down nearly dead" (Up nearly certain). The manifest outcome (Up/Down)
//! tells us which side actually won.

use pm_alpha::harness::BookTick;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"PTC2";
const CACHE_DIR: &str = "data/cache/ticks";
const W3_DATES: &[&str] = &[
    "2026-05-07",
    "2026-05-08",
    "2026-05-09",
    "2026-05-10",
    "2026-05-11",
    "2026-05-12",
    "2026-05-13",
    "2026-05-14",
    "2026-05-15",
    "2026-05-16",
    "2026-05-17",
    "2026-05-18",
];
const CHEAP_MAX: f64 = 0.05;
const FEE_RATE: f64 = 0.07;
const NOTIONAL: f64 = 7.50; // mid of $5-10 small clip

#[derive(Clone)]
struct Market {
    asset_id: String,
    close_ts: i64,
    duration_s: i64,
    up_won: bool,
    date: String,
    horizon: &'static str, // "5m" | "15m"
}

fn read_tape(path: &Path) -> Option<Vec<BookTick>> {
    let raw = std::fs::read(path).ok()?;
    if raw.len() < 4 || &raw[..4] != MAGIC {
        return None;
    }
    let body = zstd::stream::decode_all(&raw[4..]).ok()?;
    bincode::deserialize(&body).ok()
}

fn load_manifest(path: &str, horizon: &'static str, duration_s: i64, out: &mut Vec<Market>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("missing manifest {path}");
        return;
    };
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let date = v["date"].as_str().unwrap_or("").to_string();
        if !W3_DATES.contains(&date.as_str()) {
            continue;
        }
        let asset_id = v["asset_id"].as_str().unwrap_or("").to_string();
        let close_ts = v["close_ts"].as_i64().unwrap_or(0);
        let outcome = v["outcome"].as_str().unwrap_or("");
        if asset_id.is_empty() || close_ts == 0 || outcome.is_empty() {
            continue;
        }
        out.push(Market {
            asset_id,
            close_ts,
            duration_s,
            up_won: outcome == "Up",
            date,
            horizon,
        });
    }
}

fn price_bucket(p: f64) -> Option<usize> {
    // 0: 0.01-0.02, 1: 0.02-0.03, 2: 0.03-0.05
    if p > 0.0 && p < 0.02 {
        Some(0)
    } else if p < 0.03 {
        Some(1)
    } else if p <= CHEAP_MAX {
        Some(2)
    } else {
        None
    }
}
const PB_LABEL: [&str; 3] = ["0.01-0.02", "0.02-0.03", "0.03-0.05"];

// time-to-close window: 0 = final 40%, 1 = final 20%, 2 = final 10%
const TTC_LABEL: [&str; 3] = ["final40", "final20", "final10"];

#[derive(Default, Clone, Copy)]
struct Cell {
    n: u64,
    wins: u64,
    sum_priced: f64, // sum of (ask + fee) breakeven
}
impl Cell {
    fn add(&mut self, won: bool, priced: f64) {
        self.n += 1;
        if won {
            self.wins += 1;
        }
        self.sum_priced += priced;
    }
}

fn fee_per_share(p: f64) -> f64 {
    FEE_RATE * p * (1.0 - p)
}

fn main() {
    let mut markets: Vec<Market> = Vec::new();
    load_manifest(
        "data/manifests/canonical/btc-updown-5m_up.jsonl",
        "5m",
        300,
        &mut markets,
    );
    load_manifest(
        "data/manifests/canonical/btc-updown-15m_up.jsonl",
        "15m",
        900,
        &mut markets,
    );
    eprintln!("W3 markets in manifest: {}", markets.len());

    // Observation grid keyed by (horizon_idx, ttc, price_bucket).
    // We count ONE observation per (market, side, ttc-window): the first cheap
    // ask seen in that window, classified at that price. This avoids
    // over-counting the same dead tail across thousands of ticks.
    let mut grid: BTreeMap<(&str, usize, usize), Cell> = BTreeMap::new();

    // Stale-vs-true classification (final20 only): a cheap ask is "revived"
    // (stale-cheap candidate) if, later in the same window, that side's ask
    // climbs back above 0.10. Otherwise "stayed dead" (true cheap).
    #[derive(Default, Clone, Copy)]
    struct Revival {
        n: u64,
        wins: u64,
    }
    let mut revived: Revival = Revival::default();
    let mut stayed_dead: Revival = Revival::default();

    // P&L sim: final-20% window, one entry per side per market at first cheap
    // ask, held to redemption. Per-day net.
    #[derive(Default, Clone, Copy)]
    struct DayPnl {
        net: f64,
        n: u64,
        wins: u64,
    }
    let mut by_day: BTreeMap<String, DayPnl> = BTreeMap::new();
    let mut total_net = 0.0f64;
    let mut total_n = 0u64;
    let mut total_wins = 0u64;
    let mut total_fees = 0.0f64;
    let mut markets_with_tape = 0u64;
    let mut real_no_count = 0u64;

    // diagnostic: global min in-window yes_ask + histogram of min-per-market
    let mut dbg_global_min_ya = 1.0f64;
    let mut dbg_global_min_na = 1.0f64;
    let mut dbg_hist: [u64; 6] = [0; 6]; // <=.05, <=.10, <=.20, <=.35, <=.50, >.50 (min yes_ask per market in-window)

    // ---- MAKER MODEL ----
    // A resting bid at level L on the underdog side fills when a seller crosses
    // down to L: the side's best ask reaches <= L (a marketable sell offering at
    // or below our bid). Levels and windows form a grid. fill RATE = fraction of
    // (market,side) opportunities where a <=L ask appeared in the window;
    // capacity = max size offered at ask levels priced <= L (depth down to us).
    const MAKER_LEVELS: [f64; 3] = [0.02, 0.03, 0.05];
    const MAKER_REBATE_FRAC: f64 = 0.20; // ~20% pool-rebate share of the fee curve
    #[derive(Default, Clone, Copy)]
    struct MakerCell {
        opps: u64,    // (market,side) opportunities considered (side had a valid book in window)
        fills: u64,   // opportunities where a <=L ask appeared
        wins: u64,    // of fills, side ultimately won
        cap_sum: f64, // sum of capacity (shares) offered <= L across fills
    }
    // key: (horizon, window_idx, level_idx)
    let mut maker: BTreeMap<(&str, usize, usize), MakerCell> = BTreeMap::new();

    // Stale-vs-true on FILLED maker tickets (final20, level 0.03 primary):
    // revived = side ask later climbs > 0.10 in window (a move revived it = edge);
    // stayed dead = adverse (we bought a real zero).
    #[derive(Default, Clone, Copy)]
    struct MakerRevival {
        n: u64,
        wins: u64,
    }
    let mut mk_revived = MakerRevival::default();
    let mut mk_stayed = MakerRevival::default();

    // Maker P&L at primary config (final20, level 0.03), $7.50 clip, hold to
    // redemption, capacity-capped. Per-day.
    const MK_PRIMARY_LEVEL: f64 = 0.03;
    let mut mk_by_day: BTreeMap<String, DayPnl> = BTreeMap::new();
    let mut mk_net = 0.0f64;
    let mut mk_n = 0u64;
    let mut mk_wins = 0u64;
    let mut mk_rebate = 0.0f64;
    let mut mk_capacity_usd = 0.0f64;

    for m in &markets {
        // prefer the two-sided (.2s.) tape for the real NO ladder.
        let dir = PathBuf::from(CACHE_DIR).join(&m.date);
        let p2 = dir.join(format!("{}.2s.btc", m.asset_id));
        let p1 = dir.join(format!("{}.1s.btc", m.asset_id));
        let ticks = if p2.exists() {
            read_tape(&p2)
        } else {
            read_tape(&p1)
        };
        let Some(ticks) = ticks else { continue };
        if ticks.is_empty() {
            continue;
        }
        markets_with_tape += 1;

        let start_ns = (m.close_ts - m.duration_s) as i64 * 1_000_000_000;
        let dur_ns = (m.duration_s as i64) * 1_000_000_000;
        let close_ns = m.close_ts as i64 * 1_000_000_000;

        // Per (side, ttc) first-cheap tracking for the observation grid.
        // side: 0 = YES/Up, 1 = NO/Down.
        let mut first_cheap_seen = [[false; 3]; 2];
        // For P&L (final20) first-cheap entry per side.
        let mut pnl_entered = [false; 2];
        // For revival classification (final20, per side): track if cheap seen
        // and whether ask later recovered > 0.10.
        let mut rev_cheap_seen = [false; 2];
        let mut rev_recovered = [false; 2];
        let mut rev_won = [false; 2];
        let mut rev_price = [0.0f64; 2];

        // Maker per (side, window_idx, level_idx): had a valid book (opp),
        // filled (a <=L ask appeared), and max capacity offered <= L.
        let mut mk_opp = [[[false; 3]; 3]; 2];
        let mut mk_fill = [[[false; 3]; 3]; 2];
        let mut mk_cap = [[[0.0f64; 3]; 3]; 2];
        // Maker primary-config (final20, 0.03) revival + pnl-entry per side.
        let mut mk_rev_seen = [false; 2];
        let mut mk_rev_recovered = [false; 2];
        let mut mk_rev_won = [false; 2];
        let mut mk_pnl_entered = [false; 2];
        let mut mk_pnl_cap = [0.0f64; 2];

        let has_real_no = ticks.iter().any(|t| t.has_real_no());
        if has_real_no {
            real_no_count += 1;
        }

        // global in-window min ask diagnostic (final20)
        {
            let mut mn_ya = 1.0f64;
            for t in &ticks {
                let frac = (t.ts_ns - start_ns) as f64 / dur_ns as f64;
                if (0.8..=1.0).contains(&frac) {
                    let ya = t.yes_ask as f64;
                    if ya > 0.0 && ya < 1.0 {
                        mn_ya = mn_ya.min(ya);
                        dbg_global_min_ya = dbg_global_min_ya.min(ya);
                    }
                    let na = t.no_ask as f64;
                    if na > 0.0 && na < 1.0 {
                        dbg_global_min_na = dbg_global_min_na.min(na);
                    }
                }
            }
            let b = if mn_ya <= 0.05 {
                0
            } else if mn_ya <= 0.10 {
                1
            } else if mn_ya <= 0.20 {
                2
            } else if mn_ya <= 0.35 {
                3
            } else if mn_ya <= 0.50 {
                4
            } else {
                5
            };
            dbg_hist[b] += 1;
        }

        if std::env::var("F11_DEBUG").is_ok() && markets_with_tape <= 6 {
            let (mut min_ya, mut min_na) = (1.0f64, 1.0f64);
            let mut in_win = 0u64;
            let (mut t0, mut tn) = (i64::MAX, i64::MIN);
            for t in &ticks {
                t0 = t0.min(t.ts_ns);
                tn = tn.max(t.ts_ns);
                let frac = (t.ts_ns - start_ns) as f64 / dur_ns as f64;
                if (0.8..=1.0).contains(&frac) {
                    in_win += 1;
                    let ya = t.yes_ask as f64;
                    let na = t.no_ask as f64;
                    if ya > 0.0 && ya < 1.0 {
                        min_ya = min_ya.min(ya);
                    }
                    if na > 0.0 && na < 1.0 {
                        min_na = min_na.min(na);
                    }
                }
            }
            eprintln!(
                "DBG {} {} hz={} up_won={} ticks={} final20_ticks={} min_yes_ask={:.4} min_no_ask={:.4} tape_span_s={} start_s={} close_s={}",
                m.date, &m.asset_id[..8], m.horizon, m.up_won, ticks.len(), in_win,
                min_ya, min_na, (tn - t0) / 1_000_000_000, (start_ns)/1_000_000_000, (close_ns)/1_000_000_000
            );
            // dump last 8 ticks (closest to close) with their frac and asks
            let mut tail: Vec<&BookTick> = ticks.iter().filter(|t| t.ts_ns <= close_ns).collect();
            tail.sort_by_key(|t| t.ts_ns);
            for t in tail.iter().rev().take(8).rev() {
                let frac = (t.ts_ns - start_ns) as f64 / dur_ns as f64;
                eprintln!(
                    "    t_s={} frac={:.3} yes_bid={:.3} yes_ask={:.3} no_ask={:.3}",
                    t.ts_ns / 1_000_000_000, frac, t.yes_bid, t.yes_ask, t.no_ask
                );
            }
        }

        for t in &ticks {
            if t.ts_ns < start_ns || t.ts_ns > close_ns {
                continue;
            }
            let frac = (t.ts_ns - start_ns) as f64 / dur_ns as f64; // 0..1
            let ttc_set: &[usize] = if frac >= 0.90 {
                &[0, 1, 2]
            } else if frac >= 0.80 {
                &[0, 1]
            } else if frac >= 0.60 {
                &[0]
            } else {
                &[]
            };
            if ttc_set.is_empty() {
                continue;
            }

            // side asks
            let yes_ask = t.yes_ask as f64;
            let no_ask = t.no_ask as f64;
            let sides: [(usize, f64, bool); 2] = [
                // (side_idx, ask, this_side_won)
                (0, yes_ask, m.up_won),
                (1, no_ask, !m.up_won),
            ];

            // ---- MAKER fill scan ----
            // For each side, level and active window: a resting bid at L fills
            // if the side's best ask <= L (seller crossing down to us). Capacity
            // at L = total size on ask levels priced <= L (depth offered to us).
            for (side, ask, won) in sides {
                let valid = ask > 0.0 && ask < 1.0;
                if !valid {
                    continue;
                }
                let asks_ladder = if side == 0 { &t.asks } else { &t.no_asks };
                for (li, &lvl) in MAKER_LEVELS.iter().enumerate() {
                    let cap: f64 = asks_ladder
                        .iter()
                        .filter(|bl| (bl.price as f64) > 0.0 && (bl.price as f64) <= lvl && bl.size > 0.0)
                        .map(|bl| bl.size as f64)
                        .sum();
                    // touch capacity fallback: if ladder empty but best ask<=L,
                    // assume at least the notional is available.
                    let touch_fill = ask <= lvl;
                    let cap = if cap > 0.0 { cap } else if touch_fill { NOTIONAL / lvl } else { 0.0 };
                    for &w in ttc_set {
                        mk_opp[side][w][li] = true;
                        if touch_fill {
                            mk_fill[side][w][li] = true;
                            if cap > mk_cap[side][w][li] {
                                mk_cap[side][w][li] = cap;
                            }
                        }
                    }
                }

                // primary-config maker (final20, 0.03): revival + pnl entry
                if frac >= 0.80 {
                    if mk_rev_seen[side] && ask > 0.10 {
                        mk_rev_recovered[side] = true;
                    }
                    if ask <= MK_PRIMARY_LEVEL {
                        let asks_ladder = if side == 0 { &t.asks } else { &t.no_asks };
                        let cap: f64 = asks_ladder
                            .iter()
                            .filter(|bl| (bl.price as f64) > 0.0 && (bl.price as f64) <= MK_PRIMARY_LEVEL && bl.size > 0.0)
                            .map(|bl| bl.size as f64)
                            .sum();
                        let cap = if cap > 0.0 { cap } else { NOTIONAL / MK_PRIMARY_LEVEL };
                        if !mk_rev_seen[side] {
                            mk_rev_seen[side] = true;
                            mk_rev_won[side] = won;
                        }
                        if !mk_pnl_entered[side] {
                            mk_pnl_entered[side] = true;
                            mk_pnl_cap[side] = cap;
                        }
                    }
                }
            }

            for (side, ask, won) in sides {
                let valid_ask = ask > 0.0 && ask < 1.0;
                // revival recovery check uses ANY ask in final20 above 0.10
                if valid_ask && (frac >= 0.80) && ask > 0.10 && rev_cheap_seen[side] {
                    rev_recovered[side] = true;
                }
                if !valid_ask || ask > CHEAP_MAX {
                    continue;
                }
                let Some(pb) = price_bucket(ask) else { continue };

                for &ttc in ttc_set {
                    if !first_cheap_seen[side][ttc] {
                        first_cheap_seen[side][ttc] = true;
                        let priced = ask + fee_per_share(ask);
                        grid.entry((m.horizon, ttc, pb)).or_default().add(won, priced);
                    }
                }

                // revival tracking (final20 window)
                if frac >= 0.80 && !rev_cheap_seen[side] {
                    rev_cheap_seen[side] = true;
                    rev_won[side] = won;
                    rev_price[side] = ask;
                }

                // P&L: enter once per side in final20 at first cheap ask
                if frac >= 0.80 && !pnl_entered[side] {
                    pnl_entered[side] = true;
                    let shares = NOTIONAL / ask;
                    let fee = shares * fee_per_share(ask);
                    let payout = if won { shares * 1.0 } else { 0.0 };
                    let pnl = payout - NOTIONAL - fee;
                    total_net += pnl;
                    total_n += 1;
                    total_fees += fee;
                    if won {
                        total_wins += 1;
                    }
                    let d = by_day.entry(m.date.clone()).or_default();
                    d.net += pnl;
                    d.n += 1;
                    if won {
                        d.wins += 1;
                    }
                }
            }
        }

        // finalize revival classification per side
        for side in 0..2 {
            if rev_cheap_seen[side] {
                if rev_recovered[side] {
                    revived.n += 1;
                    if rev_won[side] {
                        revived.wins += 1;
                    }
                } else {
                    stayed_dead.n += 1;
                    if rev_won[side] {
                        stayed_dead.wins += 1;
                    }
                }
            }
        }

        // finalize maker grid (one opp/fill per market-side-window-level)
        for side in 0..2 {
            let won = if side == 0 { m.up_won } else { !m.up_won };
            for w in 0..3 {
                for li in 0..3 {
                    if mk_opp[side][w][li] {
                        let c = maker.entry((m.horizon, w, li)).or_default();
                        c.opps += 1;
                        if mk_fill[side][w][li] {
                            c.fills += 1;
                            c.cap_sum += mk_cap[side][w][li];
                            if won {
                                c.wins += 1;
                            }
                        }
                    }
                }
            }
        }

        // finalize maker primary (final20, 0.03): revival + P&L
        for side in 0..2 {
            let won = if side == 0 { m.up_won } else { !m.up_won };
            if mk_rev_seen[side] {
                if mk_rev_recovered[side] {
                    mk_revived.n += 1;
                    if mk_rev_won[side] {
                        mk_revived.wins += 1;
                    }
                } else {
                    mk_stayed.n += 1;
                    if mk_rev_won[side] {
                        mk_stayed.wins += 1;
                    }
                }
            }
            if mk_pnl_entered[side] {
                // capacity-capped clip: shares = min(notional/L, available cap)
                let want = NOTIONAL / MK_PRIMARY_LEVEL;
                let shares = want.min(mk_pnl_cap[side].max(1.0));
                let cost = shares * MK_PRIMARY_LEVEL;
                let rebate = shares * MAKER_REBATE_FRAC * fee_per_share(MK_PRIMARY_LEVEL);
                let payout = if won { shares * 1.0 } else { 0.0 };
                let pnl = payout - cost + rebate;
                mk_net += pnl;
                mk_n += 1;
                mk_rebate += rebate;
                mk_capacity_usd += cost;
                if won {
                    mk_wins += 1;
                }
                let d = mk_by_day.entry(m.date.clone()).or_default();
                d.net += pnl;
                d.n += 1;
                if won {
                    d.wins += 1;
                }
            }
        }
    }

    // ---- Report ----
    println!("# f11 deep cheap-tail taker scan (W3 2026-05-07..05-18)\n");
    println!("markets in manifest (W3): {}", markets.len());
    println!("markets with tape loaded: {markets_with_tape}");
    println!("markets with real NO ladder: {real_no_count}\n");

    println!("## DIAGNOSTIC: in-window (final20) yes_ask depth\n");
    println!("global min yes_ask: {:.4}", dbg_global_min_ya);
    println!("global min no_ask:  {:.4}", dbg_global_min_na);
    println!("per-market min yes_ask histogram (final20):");
    println!("  <=0.05: {}", dbg_hist[0]);
    println!("  <=0.10: {}", dbg_hist[1]);
    println!("  <=0.20: {}", dbg_hist[2]);
    println!("  <=0.35: {}", dbg_hist[3]);
    println!("  <=0.50: {}", dbg_hist[4]);
    println!("  > 0.50: {}\n", dbg_hist[5]);

    println!("## Realized win rate vs fee-inclusive priced rate (per first-cheap observation)\n");
    println!("horizon | ttc      | bucket    |    n | wins | realized | avg_priced | edge(real-priced)");
    println!("--------|----------|-----------|------|------|----------|------------|------------------");
    for ((hz, ttc, pb), c) in &grid {
        if c.n == 0 {
            continue;
        }
        let realized = c.wins as f64 / c.n as f64;
        let priced = c.sum_priced / c.n as f64;
        let edge = realized - priced;
        println!(
            "{:>6} | {:<8} | {:<9} | {:>4} | {:>4} | {:>8.4} | {:>10.4} | {:>+8.4}",
            hz, TTC_LABEL[*ttc], PB_LABEL[*pb], c.n, c.wins, realized, priced, edge
        );
    }

    println!("\n## Stale-vs-true cheap (final20, per cheap side; revived = ask later > 0.10)\n");
    let rr = if revived.n > 0 {
        revived.wins as f64 / revived.n as f64
    } else {
        0.0
    };
    let sr = if stayed_dead.n > 0 {
        stayed_dead.wins as f64 / stayed_dead.n as f64
    } else {
        0.0
    };
    println!("revived (stale candidate): n={:>5} win_rate={:.4}", revived.n, rr);
    println!("stayed dead (true cheap):  n={:>5} win_rate={:.4}", stayed_dead.n, sr);

    println!("\n## Taker P&L sim (final20, one entry/side/market, ${NOTIONAL} clip, hold to redemption)\n");
    let wr = if total_n > 0 {
        total_wins as f64 / total_n as f64
    } else {
        0.0
    };
    let n_days = by_day.len().max(1) as f64;
    println!("total entries: {total_n}");
    println!("total wins:    {total_wins} (win rate {:.4})", wr);
    println!("total fees:    ${:.2}", total_fees);
    println!("NET P&L:       ${:.2}", total_net);
    println!("days:          {}", by_day.len());
    println!("EV/day:        ${:.2}", total_net / n_days);
    println!("entries/day:   {:.1}", total_n as f64 / n_days);
    println!("\nper-day:");
    println!("date       |   net   |  n  | wins | win%");
    println!("-----------|---------|-----|------|------");
    let mut worst_day = ("", f64::INFINITY);
    for (d, p) in &by_day {
        let w = if p.n > 0 {
            p.wins as f64 / p.n as f64
        } else {
            0.0
        };
        println!(
            "{} | {:>+7.2} | {:>3} | {:>4} | {:.3}",
            d, p.net, p.n, p.wins, w
        );
        if p.net < worst_day.1 {
            worst_day = (d.as_str(), p.net);
        }
    }
    println!("\nworst day: {} (${:+.2})", worst_day.0, worst_day.1);

    // ================= MAKER ANALYSIS =================
    let n_days_all = {
        let mut s = std::collections::BTreeSet::new();
        for m in &markets {
            s.insert(m.date.clone());
        }
        s.len().max(1) as f64
    };

    println!("\n\n# ===== MAKER EXPRESSION (resting bid on underdog, fills held to redemption) =====\n");
    println!("## Fill rate + capacity + ADVERSE SELECTION (filled-ticket win rate vs priced)\n");
    println!("A resting bid at L fills when the side ask crosses <= L. Priced breakeven");
    println!("for a maker = L (no taker fee; rebate makes true breakeven slightly < L).");
    println!("If filled-win-rate >> L: filled by noise sellers (EDGE). If ~= or < L: adverse.\n");
    println!("horizon | window   | level | opps | fills | fill% | filled_win | priced(L) | edge | avg_cap(sh) | cap$/fill");
    println!("--------|----------|-------|------|-------|-------|------------|-----------|------|-------------|----------");
    let lvl_lbl = ["0.02", "0.03", "0.05"];
    for ((hz, w, li), c) in &maker {
        if c.opps == 0 {
            continue;
        }
        let fill_rate = c.fills as f64 / c.opps as f64;
        let lvl = MAKER_LEVELS[*li];
        let fw = if c.fills > 0 {
            c.wins as f64 / c.fills as f64
        } else {
            0.0
        };
        let edge = fw - lvl;
        let avg_cap = if c.fills > 0 {
            c.cap_sum / c.fills as f64
        } else {
            0.0
        };
        let cap_usd = avg_cap * lvl;
        println!(
            "{:>6} | {:<8} | {:<5} | {:>4} | {:>5} | {:>5.3} | {:>10.4} | {:>9.3} | {:>+5.3} | {:>11.1} | {:>8.2}",
            hz, TTC_LABEL[*w], lvl_lbl[*li], c.opps, c.fills, fill_rate, fw, lvl, edge, avg_cap, cap_usd
        );
    }

    println!("\n## Stale-vs-true on FILLED maker tickets (final20, L=0.03)\n");
    let mrr = if mk_revived.n > 0 {
        mk_revived.wins as f64 / mk_revived.n as f64
    } else {
        0.0
    };
    let msr = if mk_stayed.n > 0 {
        mk_stayed.wins as f64 / mk_stayed.n as f64
    } else {
        0.0
    };
    println!("revived (move revived underdog = our edge): n={:>5} win_rate={:.4}", mk_revived.n, mrr);
    println!("stayed dead (adverse, real zero):           n={:>5} win_rate={:.4}", mk_stayed.n, msr);

    println!("\n## Maker P&L (final20, L=0.03, ${NOTIONAL} clip cap, hold to redemption, +rebate, no taker fee)\n");
    let mwr = if mk_n > 0 {
        mk_wins as f64 / mk_n as f64
    } else {
        0.0
    };
    println!("filled tickets: {mk_n}");
    println!("wins:           {mk_wins} (filled win rate {:.4}; priced breakeven L=0.03)", mwr);
    println!("rebate accrued: ${:.2}", mk_rebate);
    println!("capacity used:  ${:.2} ({:.2}/day)", mk_capacity_usd, mk_capacity_usd / n_days_all);
    println!("NET P&L:        ${:.2}", mk_net);
    println!("EV/day:         ${:.2}", mk_net / n_days_all);
    println!("fills/day:      {:.1}", mk_n as f64 / n_days_all);
    println!("\nper-day:");
    println!("date       |   net   |  n  | wins | win%");
    println!("-----------|---------|-----|------|------");
    let mut mk_worst = ("", f64::INFINITY);
    for (d, p) in &mk_by_day {
        let w = if p.n > 0 {
            p.wins as f64 / p.n as f64
        } else {
            0.0
        };
        println!(
            "{} | {:>+7.2} | {:>3} | {:>4} | {:.3}",
            d, p.net, p.n, p.wins, w
        );
        if p.net < mk_worst.1 {
            mk_worst = (d.as_str(), p.net);
        }
    }
    println!("\nmaker worst day: {} (${:+.2})", mk_worst.0, mk_worst.1);
}
