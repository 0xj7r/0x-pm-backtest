use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { Buy, Sell }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LeaderFill {
    pub ts: i64,
    pub token_id: String,
    pub condition_id: String,
    pub slug: String,
    pub outcome: String,
    pub outcome_index: u8,
    pub side: Side,
    pub price: f64,
    pub size: f64,   // shares
    pub usdc: f64,   // notional
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub condition_id: String,
    pub resolved: bool,
    pub winning_index: Option<u8>, // index into outcomes whose price == 1
    pub end_ts: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceTag { MarketPrint, PricesHistory, LeaderFill }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyEntry {
    pub leader: LeaderFill,
    pub latency_s: f64,
    pub entry_price: f64,
    pub priced_from: PriceTag,
    pub our_stake_usdc: f64,
    pub our_shares: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyResult {
    pub entry: CopyEntry,
    pub won: bool,
    pub payout_usdc: f64,
    pub pnl_usdc: f64,
    pub resolved_ts: i64,
}

impl CopyResult {
    /// Binary settlement: a winning share pays $1, a losing share pays $0.
    pub fn settle(entry: CopyEntry, won: bool, resolved_ts: i64) -> Self {
        let payout = if won { entry.our_shares } else { 0.0 };
        let pnl = payout - entry.our_stake_usdc;
        CopyResult { entry, won, payout_usdc: payout, pnl_usdc: pnl, resolved_ts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(stake: f64, price: f64) -> CopyEntry {
        CopyEntry {
            leader: LeaderFill { ts: 0, token_id: "t".into(), condition_id: "c".into(),
                slug: "s".into(), outcome: "Up".into(), outcome_index: 0, side: Side::Buy,
                price, size: stake / price, usdc: stake },
            latency_s: 0.0, entry_price: price, priced_from: PriceTag::MarketPrint,
            our_stake_usdc: stake, our_shares: stake / price,
        }
    }

    #[test]
    fn winning_share_pays_one_dollar() {
        let r = CopyResult::settle(entry(10.0, 0.40), true, 100);
        // 10 / 0.40 = 25 shares -> payout 25, pnl 15
        assert!((r.payout_usdc - 25.0).abs() < 1e-9);
        assert!((r.pnl_usdc - 15.0).abs() < 1e-9);
    }

    #[test]
    fn losing_bet_loses_stake() {
        let r = CopyResult::settle(entry(10.0, 0.40), false, 100);
        assert_eq!(r.payout_usdc, 0.0);
        assert!((r.pnl_usdc + 10.0).abs() < 1e-9);
    }
}
