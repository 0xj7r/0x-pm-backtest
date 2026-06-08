use crate::seams::FillReport;
use pm_strategy::Side;
use pm_types::MarketId;
use std::collections::HashMap;

#[derive(Debug, Default, Clone, Copy)]
pub struct Position {
    pub yes_shares: f64,
    pub no_shares: f64,
    /// Average cost basis per yes share (for realized P&L tracking).
    pub avg_yes_price: f64,
    /// Average cost basis per no share.
    pub avg_no_price: f64,
}

/// Shared capital + per-market positions. Ported from the live InventoryState:
/// cash, positions, realized P&L, mark-to-market. Keyed access only — never
/// iterated to make a risk decision.
///
/// Key adaptation from InventoryState: the live version is instrument-keyed
/// (YES and NO are separate PositionState entries under InstrumentId). Here
/// we collapse YES and NO into one Position per MarketId. The cash arithmetic
/// is identical: Buy debits (notional + fee), Sell credits (notional - fee).
#[derive(Debug, Clone)]
pub struct Portfolio {
    cash_usd: f64,
    realized_pnl_usd: f64,
    positions: HashMap<MarketId, Position>,
    open_orders_per_market: HashMap<MarketId, usize>,
}

impl Portfolio {
    pub fn new(starting_cash_usd: f64) -> Self {
        Self {
            cash_usd: starting_cash_usd,
            realized_pnl_usd: 0.0,
            positions: HashMap::new(),
            open_orders_per_market: HashMap::new(),
        }
    }

    pub fn position(&self, m: MarketId) -> Position {
        self.positions.get(&m).copied().unwrap_or_default()
    }

    /// Free cash available for new orders.
    /// Reservation lifecycle lands in Phase 2; for now free == total cash.
    pub fn free_cash_usd(&self) -> f64 {
        self.cash_usd
    }

    pub fn realized_pnl_usd(&self) -> f64 {
        self.realized_pnl_usd
    }

    pub fn open_orders_in_market(&self, m: MarketId) -> usize {
        self.open_orders_per_market.get(&m).copied().unwrap_or(0)
    }

    /// Total open orders across all markets.
    pub fn total_open_orders(&self) -> usize {
        self.open_orders_per_market.values().sum()
    }

    /// Sum of |notional| across all positions at current marks.
    /// Formula matches InventoryState::gross_exposure_usd:
    ///   Σ position.quantity.abs() * mark_or_cost()
    /// Adapted: yes_value = yes_shares * p; no_value = no_shares * (1 - p).
    pub fn gross_exposure_usd(&self, marks: &HashMap<MarketId, f32>) -> f64 {
        let exposure = self.positions.iter().map(|(m, pos)| {
            let p = *marks.get(m).unwrap_or(&0.5) as f64;
            pos.yes_shares.abs() * p + pos.no_shares.abs() * (1.0 - p)
        }).sum::<f64>();
        if exposure.abs() < 1e-9 { 0.0 } else { exposure }
    }

    /// Net signed notional for a single market at current mark.
    /// Formula matches InventoryState::net_exposure_for_market_usd:
    ///   Σ (position.quantity * mark_or_cost()) for positions in market
    /// Adapted: net = yes_shares * p - no_shares * (1 - p).
    /// (YES is long, NO is effectively short YES at price (1-p).)
    pub fn net_exposure_for_market_usd(&self, m: MarketId, marks: &HashMap<MarketId, f32>) -> f64 {
        if let Some(pos) = self.positions.get(&m) {
            let p = *marks.get(&m).unwrap_or(&0.5) as f64;
            let net = pos.yes_shares * p - pos.no_shares * (1.0 - p);
            if net.abs() < 1e-9 { 0.0 } else { net }
        } else {
            0.0
        }
    }

    /// Apply a fill: move cash, adjust the position.
    /// Sign conventions mirror InventoryState::apply_fill exactly:
    ///   Buy:  cash -= notional + fee   (debit full cost including fee)
    ///   Sell: cash += notional - fee   (credit proceeds net of fee)
    pub fn apply_fill(&mut self, f: &FillReport) {
        let pos = self.positions.entry(f.market).or_default();
        let notional = f.shares * f.price as f64;
        match f.side {
            Side::BuyYes => {
                let cost = notional + f.fee_usd;
                let new_qty = pos.yes_shares + f.shares;
                pos.avg_yes_price = if new_qty <= f64::EPSILON {
                    f.price as f64
                } else {
                    (pos.avg_yes_price * pos.yes_shares + notional) / new_qty
                };
                pos.yes_shares = new_qty;
                self.cash_usd -= cost;
            }
            Side::SellYes => {
                let proceeds = notional - f.fee_usd;
                let realized = (f.price as f64 - pos.avg_yes_price) * f.shares - f.fee_usd;
                pos.yes_shares -= f.shares;
                self.cash_usd += proceeds;
                self.realized_pnl_usd += realized;
                if pos.yes_shares.abs() <= 1e-9 && pos.no_shares.abs() <= 1e-9 {
                    self.positions.remove(&f.market);
                    return;
                }
            }
            Side::BuyNo => {
                let cost = notional + f.fee_usd;
                let new_qty = pos.no_shares + f.shares;
                pos.avg_no_price = if new_qty <= f64::EPSILON {
                    f.price as f64
                } else {
                    (pos.avg_no_price * pos.no_shares + notional) / new_qty
                };
                pos.no_shares = new_qty;
                self.cash_usd -= cost;
            }
            Side::SellNo => {
                let proceeds = notional - f.fee_usd;
                let realized = (f.price as f64 - pos.avg_no_price) * f.shares - f.fee_usd;
                pos.no_shares -= f.shares;
                self.cash_usd += proceeds;
                self.realized_pnl_usd += realized;
                if pos.yes_shares.abs() <= 1e-9 && pos.no_shares.abs() <= 1e-9 {
                    self.positions.remove(&f.market);
                    return;
                }
            }
        }
    }

    /// Mark-to-market equity: cash + Σ(yes_shares*p + no_shares*(1-p)).
    /// Formula matches InventoryState's mark-based valuation.
    pub fn equity_usd(&self, marks: &HashMap<MarketId, f32>) -> f64 {
        let mut eq = self.cash_usd;
        for (m, pos) in self.positions.iter() {
            let p = *marks.get(m).unwrap_or(&0.5) as f64;
            eq += pos.yes_shares * p + pos.no_shares * (1.0 - p);
        }
        eq
    }

    /// Settle a resolved market: winning shares pay 1.0, losing pay 0.0.
    /// Both legs are realized separately, matching InventoryState::apply_settlement
    /// which settles each instrument (YES and NO) independently.
    pub fn settle(&mut self, m: MarketId, resolved_yes: bool) {
        if let Some(pos) = self.positions.remove(&m) {
            let (winning_shares, winning_cost) = if resolved_yes {
                (pos.yes_shares, pos.avg_yes_price * pos.yes_shares)
            } else {
                (pos.no_shares, pos.avg_no_price * pos.no_shares)
            };
            let losing_cost = if resolved_yes {
                pos.avg_no_price * pos.no_shares
            } else {
                pos.avg_yes_price * pos.yes_shares
            };

            self.cash_usd += winning_shares;
            self.realized_pnl_usd += (winning_shares - winning_cost) + (0.0 - losing_cost);
        }
    }

    /// Stub: reserve cash on order submit. Phase 2 reservation lifecycle.
    #[allow(dead_code)]
    fn reserve(&mut self, _order_id: crate::seams::OrderId, _amount_usd: f64) {
        // TODO(Phase 2): debit free_cash_usd, credit reserved_cash_usd,
        // track reservation by order id. The live InventoryState::reserve_for_order
        // does this; we need the order lifecycle (cancel/expire path) before it
        // makes sense here. In Phase 1 all fills are instant so there is no
        // window where cash is reserved.
    }

    /// Stub: release cash reservation on fill/cancel. Phase 2 reservation lifecycle.
    #[allow(dead_code)]
    fn release(&mut self, _order_id: crate::seams::OrderId) {
        // TODO(Phase 2): move reserved amount back to free_cash_usd.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::{FillLiquidity, OrderId};
    use pm_types::MarketId;

    fn buy_yes(m: MarketId, shares: f64, price: f32) -> FillReport {
        FillReport {
            order: OrderId(1),
            market: m,
            side: Side::BuyYes,
            shares,
            price,
            fee_usd: 0.0,
            liquidity: FillLiquidity::Taker,
            ts: 0,
        }
    }

    #[test]
    fn buy_yes_moves_cash_and_position() {
        let m = MarketId(0);
        let mut pf = Portfolio::new(1_000.0);
        pf.apply_fill(&buy_yes(m, 100.0, 0.40));
        assert_eq!(pf.position(m).yes_shares, 100.0);
        // f32 price precision: 0.40f32 as f64 is ~40.0000006, so use 1e-6 tolerance
        assert!((pf.free_cash_usd() - 960.0).abs() < 1e-6); // 1000 - 100*0.40
    }

    #[test]
    fn settle_pays_winning_side() {
        let m = MarketId(0);
        let mut pf = Portfolio::new(1_000.0);
        pf.apply_fill(&buy_yes(m, 100.0, 0.40)); // cash ~960, 100 yes
        pf.settle(m, true); // +100 payout
        assert!((pf.free_cash_usd() - 1_060.0).abs() < 1e-6);
    }

    #[test]
    fn gross_exposure_usd_sums_absolute_notionals() {
        let m = MarketId(0);
        let mut pf = Portfolio::new(1_000.0);
        pf.apply_fill(&buy_yes(m, 100.0, 0.50));
        let marks: HashMap<MarketId, f32> = [(m, 0.50f32)].into_iter().collect();
        // 100 yes @ 0.50 = 50.0 gross
        assert!((pf.gross_exposure_usd(&marks) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn net_exposure_for_market_usd_nets_yes_vs_no() {
        let m = MarketId(0);
        let mut pf = Portfolio::new(1_000.0);
        // Buy 100 YES and 50 NO, both at price 0.50
        pf.apply_fill(&buy_yes(m, 100.0, 0.50));
        pf.apply_fill(&FillReport {
            order: OrderId(2),
            market: m,
            side: Side::BuyNo,
            shares: 50.0,
            price: 0.50,
            fee_usd: 0.0,
            liquidity: FillLiquidity::Taker,
            ts: 0,
        });
        let marks: HashMap<MarketId, f32> = [(m, 0.50f32)].into_iter().collect();
        // net = 100*0.50 - 50*(1-0.50) = 50 - 25 = 25
        assert!((pf.net_exposure_for_market_usd(m, &marks) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn settle_realizes_both_legs_pnl() {
        let m = MarketId(0);
        let mut pf = Portfolio::new(1_000.0);
        // Buy 100 YES @ 0.40 (cost 40) and 50 NO @ 0.30 (cost 15).
        pf.apply_fill(&buy_yes(m, 100.0, 0.40));
        pf.apply_fill(&FillReport {
            order: OrderId(2),
            market: m,
            side: Side::BuyNo,
            shares: 50.0,
            price: 0.30,
            fee_usd: 0.0,
            liquidity: FillLiquidity::Taker,
            ts: 0,
        });
        // cash should be 1000 - 40 - 15 = 945
        assert!((pf.free_cash_usd() - 945.0).abs() < 1e-4);

        // Settle YES wins: YES pays 100, NO pays 0.
        pf.settle(m, true);

        // cash: 945 + 100 = 1045
        assert!(
            (pf.free_cash_usd() - 1_045.0).abs() < 1e-4,
            "cash expected 1045, got {}",
            pf.free_cash_usd()
        );
        // realized: (100 - 40) + (0 - 15) = 60 - 15 = 45
        assert!(
            (pf.realized_pnl_usd() - 45.0).abs() < 1e-4,
            "realized_pnl expected 45, got {}",
            pf.realized_pnl_usd()
        );
    }

    /// Cash reservation lifecycle (reserve on submit, release on fill/cancel)
    /// is deferred to Phase 2 when the order lifecycle lands. In Phase 1 all
    /// fills are instant so there is no window where cash needs to be reserved.
    /// The reserve/release pair is stubbed above with a TODO comment.
    #[test]
    #[ignore = "reservation lifecycle lands in Phase 2"]
    fn reservation_lifecycle_reserve_then_fill_then_release() {
        let _m = MarketId(0);
        let mut _pf = Portfolio::new(1_000.0);
        // TODO(Phase 2): call pf.reserve(order_id, notional), assert free_cash drops,
        // apply fill (which should consume the reservation), assert reserved returns to 0.
        todo!("Phase 2: implement reserve/release lifecycle test");
    }
}
