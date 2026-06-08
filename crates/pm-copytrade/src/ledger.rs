use crate::model::{CopyEntry, CopyResult, LeaderFill, PriceTag, Resolution, Side};
use crate::equity::LeaderEquity;
use crate::sizing::proportional_stake;
use std::collections::HashMap;

pub struct LedgerConfig {
    pub our_bankroll: f64,
    pub max_clip_usdc: f64,
    pub latency_s: f64,
}

pub struct CopyOutcome {
    pub results: Vec<CopyResult>,
    pub open_unresolved: usize,
    pub final_equity: f64,
}

pub fn run_ledger(
    fills: &[LeaderFill],
    leader_eq: &LeaderEquity,
    res: &HashMap<String, Resolution>,
    priced: &HashMap<usize, (f64, PriceTag)>,
    cfg: &LedgerConfig,
) -> CopyOutcome {
    let mut equity = cfg.our_bankroll;
    let mut results = Vec::new();
    let mut open = 0usize;
    for (i, f) in fills.iter().enumerate() {
        let Some(&(entry_price, tag)) = priced.get(&i) else { continue; };
        if entry_price <= 0.0 || entry_price >= 1.0 { continue; }
        let stake = proportional_stake(f.usdc, leader_eq.equity_at(f.ts), equity, cfg.max_clip_usdc);
        if stake <= 0.0 { continue; }
        let shares = stake / entry_price;
        let entry = CopyEntry {
            leader: f.clone(), latency_s: cfg.latency_s, entry_price, priced_from: tag,
            our_stake_usdc: stake, our_shares: shares,
        };
        match res.get(&f.condition_id) {
            Some(r) if r.resolved => {
                let won = r.winning_index == Some(f.outcome_index);
                let result = CopyResult::settle(entry, won, r.end_ts.unwrap_or(f.ts));
                equity += result.pnl_usdc;
                results.push(result);
            }
            _ => { open += 1; }
        }
    }
    CopyOutcome { results, open_unresolved: open, final_equity: equity }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fill(ts: i64, cond: &str, oi: u8, usdc: f64) -> LeaderFill {
        LeaderFill { ts, token_id: format!("{cond}-{oi}"), condition_id: cond.into(),
            slug: "s".into(), outcome: "x".into(), outcome_index: oi, side: Side::Buy,
            price: 0.5, size: usdc / 0.5, usdc }
    }
    fn resolved(cond: &str, wi: u8, ets: i64) -> Resolution {
        Resolution { condition_id: cond.into(), resolved: true, winning_index: Some(wi), end_ts: Some(ets) }
    }
    #[test]
    fn winning_copy_grows_equity() {
        let fills = vec![fill(100, "c1", 0, 50.0)];
        let leader_eq = LeaderEquity::reconstruct(&fills, &HashMap::new(), 1000.0);
        let mut res = HashMap::new(); res.insert("c1".into(), resolved("c1", 0, 200));
        let mut priced = HashMap::new(); priced.insert(0usize, (0.40, PriceTag::MarketPrint));
        let cfg = LedgerConfig { our_bankroll: 100.0, max_clip_usdc: 1000.0, latency_s: 2.0 };
        let out = run_ledger(&fills, &leader_eq, &res, &priced, &cfg);
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].won);
        assert!((out.final_equity - 107.5).abs() < 1e-9);
    }
    #[test]
    fn unresolved_market_counts_as_open() {
        let fills = vec![fill(100, "c1", 0, 50.0)];
        let leader_eq = LeaderEquity::reconstruct(&fills, &HashMap::new(), 1000.0);
        let mut priced = HashMap::new(); priced.insert(0usize, (0.40, PriceTag::MarketPrint));
        let cfg = LedgerConfig { our_bankroll: 100.0, max_clip_usdc: 1000.0, latency_s: 2.0 };
        let out = run_ledger(&fills, &leader_eq, &HashMap::new(), &priced, &cfg);
        assert_eq!(out.open_unresolved, 1);
        assert!((out.final_equity - 100.0).abs() < 1e-9);
    }
}
