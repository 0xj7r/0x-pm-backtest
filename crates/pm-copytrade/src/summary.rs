use crate::model::CopyResult;
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use anyhow::Result;

#[derive(Debug, Serialize, PartialEq)]
pub struct Summary {
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub roi: f64,
    pub max_drawdown: f64,
}

pub fn summarize(results: &[CopyResult], starting_bankroll: f64) -> Summary {
    let trades = results.len();
    let wins = results.iter().filter(|r| r.won).count();
    let total_pnl: f64 = results.iter().map(|r| r.pnl_usdc).sum();
    let total_staked: f64 = results.iter().map(|r| r.entry.our_stake_usdc).sum();
    let mut equity = starting_bankroll;
    let mut peak = starting_bankroll;
    let mut max_dd = 0.0f64;
    for r in results {
        equity += r.pnl_usdc;
        peak = peak.max(equity);
        if peak > 0.0 { max_dd = max_dd.max((peak - equity) / peak); }
    }
    Summary {
        trades,
        wins,
        win_rate: if trades > 0 { wins as f64 / trades as f64 } else { 0.0 },
        total_pnl,
        roi: if total_staked > 0.0 { total_pnl / total_staked } else { 0.0 },
        max_drawdown: max_dd,
    }
}

pub fn write_ledger_jsonl(path: &Path, results: &[CopyResult]) -> Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        for r in results { writeln!(f, "{}", serde_json::to_string(r)?)?; }
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn write_summary_json<T: Serialize>(path: &Path, summary: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(summary)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Derive (asset, duration) from a slug like "eth-updown-15m-1780918200".
pub fn slug_bucket(slug: &str) -> (String, String) {
    let parts: Vec<&str> = slug.split('-').collect();
    let asset = parts.get(0).copied().unwrap_or("unknown").to_string();
    let dur = parts.get(2).copied().unwrap_or("unknown").to_string();
    (asset, dur)
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Bucket { pub key: String, pub trades: usize, pub wins: usize, pub pnl: f64 }

pub fn breakdown(results: &[CopyResult]) -> Vec<Bucket> {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new();
    for r in results {
        let (asset, dur) = slug_bucket(&r.entry.leader.slug);
        let e = m.entry(format!("{asset}-{dur}")).or_default();
        e.0 += 1; e.1 += r.won as usize; e.2 += r.pnl_usdc;
    }
    m.into_iter().map(|(key, (t, w, p))| Bucket { key, trades: t, wins: w, pnl: p }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CopyEntry, LeaderFill, PriceTag, Side};

    fn result(stake: f64, price: f64, won: bool) -> CopyResult {
        let shares = stake / price;
        let entry = CopyEntry {
            leader: LeaderFill { ts: 0, token_id: "t".into(), condition_id: "c".into(),
                slug: "s".into(), outcome: "x".into(), outcome_index: 0, side: Side::Buy,
                price, size: shares, usdc: stake },
            latency_s: 0.0, entry_price: price, priced_from: PriceTag::MarketPrint,
            our_stake_usdc: stake, our_shares: shares,
        };
        CopyResult::settle(entry, won, 1)
    }

    #[test]
    fn computes_win_rate_and_roi() {
        let rs = vec![result(10.0, 0.5, true), result(10.0, 0.5, false)];
        let s = summarize(&rs, 100.0);
        assert_eq!(s.trades, 2);
        assert_eq!(s.wins, 1);
        assert!((s.win_rate - 0.5).abs() < 1e-9);
        assert!((s.total_pnl - 0.0).abs() < 1e-9);
        assert!((s.roi - 0.0).abs() < 1e-9);
    }

    #[test]
    fn writes_jsonl_one_line_per_result() {
        let rs = vec![result(10.0, 0.5, true), result(10.0, 0.5, false)];
        let dir = std::env::temp_dir().join(format!("pmct-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ledger.jsonl");
        write_ledger_jsonl(&p, &rs).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert_eq!(content.lines().count(), 2);
    }
}

#[cfg(test)]
mod breakdown_tests {
    use super::*;
    use crate::model::{CopyEntry, LeaderFill, PriceTag, Side};

    fn r(slug: &str, won: bool) -> CopyResult {
        let entry = CopyEntry {
            leader: LeaderFill { ts: 0, token_id: "t".into(), condition_id: "c".into(),
                slug: slug.into(), outcome: "x".into(), outcome_index: 0, side: Side::Buy,
                price: 0.5, size: 2.0, usdc: 1.0 },
            latency_s: 0.0, entry_price: 0.5, priced_from: PriceTag::MarketPrint,
            our_stake_usdc: 1.0, our_shares: 2.0,
        };
        CopyResult::settle(entry, won, 1)
    }

    #[test]
    fn groups_by_asset_and_duration() {
        let rs = vec![r("eth-updown-15m-1", true), r("eth-updown-15m-2", false), r("sol-updown-15m-3", true)];
        let b = breakdown(&rs);
        let eth = b.iter().find(|x| x.key == "eth-15m").unwrap();
        assert_eq!(eth.trades, 2);
        assert_eq!(eth.wins, 1);
        assert!(b.iter().any(|x| x.key == "sol-15m"));
    }
}
