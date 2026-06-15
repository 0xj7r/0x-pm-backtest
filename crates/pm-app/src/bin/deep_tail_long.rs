//! Deep cheap-tail min-ask scan for LONGER horizons (4h primarily).
//!
//! f11 proved BTC-5m/15m W3 have NO sub-0.26 ask near close (no deep tail).
//! The open question (GOAL A): on a 1h/4h window a losing side has time to
//! decay to <=0.05. Polymarket has no 1h market, so 4h is the longest horizon.
//! This scans the merged BookTick tape cache for the MIN ask seen late in each
//! window and reports whether a <=0.10 / <=0.05 deep-tail level actually exists.
//!
//! Usage: deep_tail_long <manifest.jsonl> <duration_s> <label>
//! e.g.   deep_tail_long data/manifests/canonical/btc-updown-4h_up.jsonl 14400 BTC-4h

use pm_alpha::harness::BookTick;
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"PTC2";
const CACHE_DIR: &str = "data/cache/ticks";
const W3_DATES: &[&str] = &[
    "2026-05-07", "2026-05-08", "2026-05-09", "2026-05-10", "2026-05-11",
    "2026-05-12", "2026-05-13", "2026-05-14", "2026-05-15", "2026-05-16",
    "2026-05-17", "2026-05-18",
];

struct Market {
    asset_id: String,
    close_ts: i64,
    duration_s: i64,
    up_won: bool,
    date: String,
}

fn read_tape(path: &Path) -> Option<Vec<BookTick>> {
    let raw = std::fs::read(path).ok()?;
    if raw.len() < 4 || &raw[..4] != MAGIC {
        return None;
    }
    let body = zstd::stream::decode_all(&raw[4..]).ok()?;
    bincode::deserialize(&body).ok()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: deep_tail_long <manifest> <duration_s> <label>");
        std::process::exit(2);
    }
    let manifest = &args[1];
    let duration_s: i64 = args[2].parse().expect("duration_s");
    let label = &args[3];

    let mut markets: Vec<Market> = Vec::new();
    let text = std::fs::read_to_string(manifest).expect("read manifest");
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
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
        markets.push(Market {
            asset_id,
            close_ts,
            duration_s,
            up_won: outcome == "Up",
            date,
        });
    }
    eprintln!("{label}: {} W3 markets in manifest", markets.len());

    let mut with_tape = 0u64;
    let mut real_no = 0u64;
    // min winning-side ask in final 20% per market (the cheap-tail candidate is
    // the LOSING side, so we also track the losing side's min ask separately).
    let mut global_min_ya = 1.0f64;
    let mut global_min_na = 1.0f64;
    // sanity: how high does the WINNER side's book get near close?
    let mut global_max_yes_bid = 0.0f64;
    let mut global_max_yes_ask = 0.0f64;
    // histogram of per-market min ask (over BOTH sides) in final 20%
    let mut hist: [u64; 6] = [0; 6]; // <=.05,<=.10,<=.20,<=.35,<=.50,>.50
    // deep-tail existence on the LOSING side (the side that should decay):
    let mut lose_min_le10 = 0u64;
    let mut lose_min_le05 = 0u64;
    let mut lose_n = 0u64;
    // by final-fraction min ask (final40/20/10) on the losing side
    let mut lose_min_by_ttc: [f64; 3] = [1.0, 1.0, 1.0]; // global min losing ask per ttc

    for m in &markets {
        let dir = PathBuf::from(CACHE_DIR).join(&m.date);
        let p2 = dir.join(format!("{}.2s.btc", m.asset_id));
        let p1 = dir.join(format!("{}.1s.btc", m.asset_id));
        let ticks = if p2.exists() { read_tape(&p2) } else { read_tape(&p1) };
        let Some(ticks) = ticks else { continue };
        if ticks.is_empty() {
            continue;
        }
        with_tape += 1;
        let has_real_no = ticks.iter().any(|t| t.has_real_no());
        if has_real_no {
            real_no += 1;
        }

        let start_ns = (m.close_ts - m.duration_s) * 1_000_000_000;
        let dur_ns = m.duration_s * 1_000_000_000;

        // The LOSING side is the one whose ask should decay toward 0.
        // up_won => Down lost => the NO/Down ask (no_ask) is the decaying side
        //           AND equivalently the YES ask stays high; the cheap tail is
        //           "buy the loser". If up_won, the loser is Down => its buy
        //           price = no_ask (real) else 1 - yes_bid.
        //   if !up_won (Down won), loser is Up => buy price = yes_ask.
        let mut min_ya = 1.0f64; // Up ask in final20
        let mut min_na = 1.0f64; // Down ask in final20
        let mut min_lose = [1.0f64; 3]; // losing-side buy price by ttc idx (40/20/10)

        for t in &ticks {
            let frac = (t.ts_ns - start_ns) as f64 / dur_ns as f64;
            if !(0.6..=1.0).contains(&frac) {
                continue;
            }
            let ya = t.yes_ask as f64;
            let na = t.no_ask as f64;
            // losing-side buy price
            let lose_price = if m.up_won {
                // Down lost: prefer real no_ask, else synthetic 1 - yes_bid
                if na > 0.0 && na < 1.0 {
                    Some(na)
                } else if t.yes_bid > 0.0 && (t.yes_bid as f64) < 1.0 {
                    Some(1.0 - t.yes_bid as f64)
                } else {
                    None
                }
            } else {
                // Up lost: yes_ask
                if ya > 0.0 && ya < 1.0 {
                    Some(ya)
                } else {
                    None
                }
            };
            // ttc buckets: final40 (>=0.6), final20 (>=0.8), final10 (>=0.9)
            if frac >= 0.8 {
                if ya > 0.0 && ya < 1.0 {
                    min_ya = min_ya.min(ya);
                    global_min_ya = global_min_ya.min(ya);
                    global_max_yes_ask = global_max_yes_ask.max(ya);
                }
                if na > 0.0 && na < 1.0 {
                    min_na = min_na.min(na);
                    global_min_na = global_min_na.min(na);
                }
                let yb = t.yes_bid as f64;
                if yb > 0.0 && yb < 1.0 {
                    global_max_yes_bid = global_max_yes_bid.max(yb);
                }
            }
            if let Some(lp) = lose_price {
                if frac >= 0.6 {
                    min_lose[0] = min_lose[0].min(lp);
                }
                if frac >= 0.8 {
                    min_lose[1] = min_lose[1].min(lp);
                }
                if frac >= 0.9 {
                    min_lose[2] = min_lose[2].min(lp);
                }
            }
        }

        // per-market min over both sides (final20) histogram
        let mn = min_ya.min(min_na);
        let b = if mn <= 0.05 { 0 }
            else if mn <= 0.10 { 1 }
            else if mn <= 0.20 { 2 }
            else if mn <= 0.35 { 3 }
            else if mn <= 0.50 { 4 }
            else { 5 };
        hist[b] += 1;

        // losing-side deep-tail existence (final20)
        if min_lose[1] < 1.0 {
            lose_n += 1;
            if min_lose[1] <= 0.10 {
                lose_min_le10 += 1;
            }
            if min_lose[1] <= 0.05 {
                lose_min_le05 += 1;
            }
        }
        for i in 0..3 {
            if min_lose[i] < 1.0 {
                lose_min_by_ttc[i] = lose_min_by_ttc[i].min(min_lose[i]);
            }
        }
    }

    println!("==== {label} deep-tail scan (W3, final-window) ====");
    println!("markets with tape: {with_tape} / {} ({} real-NO)", markets.len(), real_no);
    println!("global min yes_ask (final20): {:.3}", global_min_ya);
    println!("global min no_ask  (final20): {:.3}", global_min_na);
    println!("SANITY winner-side max yes_bid (final20): {:.3}  max yes_ask: {:.3}",
        global_max_yes_bid, global_max_yes_ask);
    println!("per-market min-ask (both sides, final20) histogram:");
    let labs = ["<=0.05", "<=0.10", "<=0.20", "<=0.35", "<=0.50", ">0.50"];
    for (i, l) in labs.iter().enumerate() {
        println!("  {l}: {}", hist[i]);
    }
    println!("LOSING-side deep tail (final20, n={lose_n}):");
    println!("  markets where losing-side buy <= 0.10: {lose_min_le10}");
    println!("  markets where losing-side buy <= 0.05: {lose_min_le05}");
    println!("global min losing-side buy price by window: final40={:.3} final20={:.3} final10={:.3}",
        lose_min_by_ttc[0], lose_min_by_ttc[1], lose_min_by_ttc[2]);
}
