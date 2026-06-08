use crate::exposure::{ExposureKey, ExposureState};
use crate::portfolio::Portfolio;
use crate::seams::{IntentKind, OrderIntent};
use pm_types::MarketId;
use std::collections::HashMap;

/// Hard caps. Field names and semantics mirror the live `RiskLimits`
/// (core/risk.rs) so live and backtest gate identically.
#[derive(Debug, Clone, Copy)]
pub struct RiskLimits {
    pub max_order_notional_usd: f64,
    pub max_gross_notional_usd: f64,
    pub max_net_notional_per_market_usd: f64,
    pub max_position_quantity_per_instrument: f64,
    pub min_free_cash_usd: f64,
    pub min_portfolio_equity_usd: f64,
    pub max_open_orders_total: usize,
    pub max_open_orders_per_market: usize,
    /// Correlated-exposure cap (absolute net shares) per (token, window).
    /// 0.0 = disabled.
    pub max_correlated_net_shares: f64,
}

impl Default for RiskLimits {
    fn default() -> Self {
        Self {
            max_order_notional_usd: 250.0,
            max_gross_notional_usd: 1_000.0,
            max_net_notional_per_market_usd: 500.0,
            max_position_quantity_per_instrument: 10_000.0,
            min_free_cash_usd: 0.0,
            min_portfolio_equity_usd: 0.0,
            max_open_orders_total: 32,
            max_open_orders_per_market: 8,
            max_correlated_net_shares: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RiskDecision {
    Approve,
    Reject(&'static str),
}

pub struct RiskGate {
    pub limits: RiskLimits,
}

impl RiskGate {
    /// Port of `RiskEngine::evaluate` (core/risk.rs:158). Check order in the
    /// same order as the live engine:
    ///
    /// 1. Invalid order (line 164)
    /// 2. Portfolio equity floor (line 196) — entry only
    /// 3. Per-order notional cap (line 213) — entry only
    /// 4. Total open-order count cap (line 226) — entry only
    /// 5. Per-market open-order count cap (line 239) — entry only
    /// 6. Position-quantity cap (line 255) — entry only
    /// 7. Sell-vs-inventory check (line 270) — always
    /// 8. Free-cash floor (line 313) — always (close uses strict >= 0 check)
    /// 9. Gross notional cap (line 341) — entry only
    /// 10. Market net notional cap (line 359) — entry only
    /// 11. Correlated-exposure cap — entry only (new gate added here)
    ///
    /// `Close` intents bypass checks 2–6, 9–11 (mirrors the live rescue branch).
    pub fn check(
        &self,
        order: &OrderIntent,
        portfolio: &Portfolio,
        marks: &HashMap<MarketId, f32>,
        exposure: &ExposureState,
        exposure_key: ExposureKey,
        signed_shares_delta: f64,
    ) -> RiskDecision {
        let is_close = order.kind == IntentKind::Close;

        // 1. Invalid order (line 164): shares and price must be positive.
        let current_mark = marks.get(&order.market).copied().unwrap_or(0.5);
        let price = order.limit_price.unwrap_or(current_mark) as f64;
        if price <= 0.0 || order.shares <= 0.0 {
            return RiskDecision::Reject("invalid_order");
        }

        // 2. Portfolio equity floor (line 196) — entry only.
        if !is_close && self.limits.min_portfolio_equity_usd > 0.0 {
            let eq = portfolio.equity_usd(marks);
            if eq < self.limits.min_portfolio_equity_usd {
                return RiskDecision::Reject("portfolio_equity_too_low");
            }
        }

        let notional = order.shares * price;

        // 3. Per-order notional cap (line 213) — entry only.
        if !is_close && notional > self.limits.max_order_notional_usd {
            return RiskDecision::Reject("order_notional_too_large");
        }

        // 4. Total open-order count cap (line 226) — entry only.
        if !is_close && portfolio.total_open_orders() >= self.limits.max_open_orders_total {
            return RiskDecision::Reject("too_many_open_orders");
        }

        // 5. Per-market open-order count cap (line 239) — entry only.
        if !is_close
            && portfolio.open_orders_in_market(order.market) >= self.limits.max_open_orders_per_market
        {
            return RiskDecision::Reject("too_many_open_orders_for_market");
        }

        // 6. Position-quantity cap (line 255) — entry only.
        if !is_close {
            let pos = portfolio.position(order.market);
            let current_qty = match order.side {
                pm_strategy::Side::BuyYes | pm_strategy::Side::SellYes => pos.yes_shares,
                pm_strategy::Side::BuyNo | pm_strategy::Side::SellNo => pos.no_shares,
            };
            let sign = match order.side {
                pm_strategy::Side::BuyYes | pm_strategy::Side::BuyNo => 1.0,
                pm_strategy::Side::SellYes | pm_strategy::Side::SellNo => -1.0,
            };
            let projected_qty = current_qty + order.shares * sign;
            if projected_qty.abs() > self.limits.max_position_quantity_per_instrument {
                return RiskDecision::Reject("position_quantity_too_large");
            }
        }

        // 7. Sell-vs-inventory: can't sell more than held (line 270) — always.
        {
            let pos = portfolio.position(order.market);
            let (is_sell, inventory) = match order.side {
                pm_strategy::Side::SellYes => (true, pos.yes_shares),
                pm_strategy::Side::SellNo => (true, pos.no_shares),
                _ => (false, 0.0),
            };
            if is_sell && inventory + 1e-9 < order.shares {
                return RiskDecision::Reject("sell_exceeds_inventory");
            }
        }

        // 8. Free-cash floor (line 287 close branch / line 313 entry branch).
        let is_buy = matches!(
            order.side,
            pm_strategy::Side::BuyYes | pm_strategy::Side::BuyNo
        );
        let projected_free_cash = if is_buy {
            portfolio.free_cash_usd() - notional
        } else {
            portfolio.free_cash_usd()
        };
        if is_close {
            // Close: strict >= 0 check (line 288).
            if projected_free_cash < -1e-9 {
                return RiskDecision::Reject("free_cash_too_low");
            }
            // Close bypasses all remaining caps — return approve (line 300).
            return RiskDecision::Approve;
        }
        if projected_free_cash < self.limits.min_free_cash_usd {
            return RiskDecision::Reject("free_cash_too_low");
        }

        // 9. Gross notional cap (line 341) — entry only.
        let current_gross = portfolio.gross_exposure_usd(marks);
        let projected_gross = if is_buy {
            current_gross + notional
        } else {
            let pos = portfolio.position(order.market);
            let held = match order.side {
                pm_strategy::Side::SellYes => pos.yes_shares,
                pm_strategy::Side::SellNo => pos.no_shares,
                _ => 0.0,
            };
            let reducible = held.min(order.shares) * price;
            (current_gross - reducible).max(0.0)
        };
        if projected_gross > self.limits.max_gross_notional_usd {
            return RiskDecision::Reject("gross_exposure_too_large");
        }

        // 10. Market net notional cap (line 354) — entry only.
        let current_net = portfolio.net_exposure_for_market_usd(order.market, marks);
        let signed_notional = match order.side {
            pm_strategy::Side::BuyYes | pm_strategy::Side::BuyNo => notional,
            pm_strategy::Side::SellYes | pm_strategy::Side::SellNo => -notional,
        };
        let projected_net = (current_net + signed_notional).abs();
        if projected_net > self.limits.max_net_notional_per_market_usd {
            return RiskDecision::Reject("market_net_exposure_too_large");
        }

        // 11. Correlated-exposure cap — entry only.
        if self.limits.max_correlated_net_shares > 0.0
            && exposure.would_exceed(exposure_key, signed_shares_delta, self.limits.max_correlated_net_shares)
        {
            return RiskDecision::Reject("correlated_exposure_cap");
        }

        RiskDecision::Approve
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Token;
    use crate::exposure::ExposureState;
    use crate::portfolio::Portfolio;
    use crate::seams::{FillLiquidity, FillReport, IntentKind, OrderId, OrderIntent};
    use pm_strategy::Side;
    use pm_types::MarketId;

    fn limits() -> RiskLimits {
        RiskLimits {
            max_order_notional_usd: 1_000.0,
            max_gross_notional_usd: 10_000.0,
            max_net_notional_per_market_usd: 1_000.0,
            max_position_quantity_per_instrument: 5_000.0,
            min_free_cash_usd: 0.0,
            min_portfolio_equity_usd: 0.0,
            max_open_orders_total: 32,
            max_open_orders_per_market: 8,
            max_correlated_net_shares: 1_000.0,
        }
    }

    fn gate() -> RiskGate {
        RiskGate { limits: limits() }
    }

    fn order_entry() -> OrderIntent {
        OrderIntent {
            id: OrderId(1),
            market: MarketId(0),
            side: Side::BuyYes,
            shares: 200.0,
            max_depth: 1,
            limit_price: None,
            tag: "t",
            kind: IntentKind::Entry,
        }
    }

    fn marks(m: MarketId, p: f32) -> HashMap<MarketId, f32> {
        [(m, p)].into_iter().collect()
    }

    fn no_exposure() -> (ExposureState, ExposureKey) {
        let exp = ExposureState::default();
        let key = ExposureKey { token: Token::Btc, window: 0 };
        (exp, key)
    }

    // ---- plan's 2 cap tests ----

    #[test]
    fn rejects_when_correlated_cap_would_be_exceeded() {
        let gate = gate();
        let pf = Portfolio::new(10_000.0);
        let mut exp = ExposureState::default();
        let key = ExposureKey { token: Token::Btc, window: 0 };
        exp.apply(key, 900.0);
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("correlated_exposure_cap"));
    }

    #[test]
    fn approves_within_cap() {
        let gate = gate();
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Approve);
    }

    // ---- characterization tests per ported check ----

    #[test]
    fn rejects_invalid_order_zero_shares() {
        let gate = gate();
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let mut order = order_entry();
        order.shares = 0.0;
        let result = gate.check(&order, &pf, &marks(m, 0.50), &exp, key, 0.0);
        assert_eq!(result, RiskDecision::Reject("invalid_order"));
    }

    #[test]
    fn rejects_invalid_order_zero_price() {
        let gate = gate();
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let mut order = order_entry();
        // Force price=0 by setting limit_price and removing any mark (marks empty)
        order.limit_price = Some(0.0);
        let result = gate.check(&order, &pf, &HashMap::new(), &exp, key, 0.0);
        assert_eq!(result, RiskDecision::Reject("invalid_order"));
    }

    #[test]
    fn rejects_below_portfolio_equity_floor() {
        let mut lim = limits();
        lim.min_portfolio_equity_usd = 9_999.0;
        let gate = RiskGate { limits: lim };
        // Portfolio with cash 5_000 and no positions => equity = 5_000 < 9_999
        let pf = Portfolio::new(5_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("portfolio_equity_too_low"));
    }

    #[test]
    fn rejects_over_per_order_notional() {
        let mut lim = limits();
        lim.max_order_notional_usd = 50.0; // 200 shares * 0.50 = 100 > 50
        let gate = RiskGate { limits: lim };
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("order_notional_too_large"));
    }

    #[test]
    fn rejects_too_many_open_orders_total() {
        // Use max_open_orders_total = 0 to guarantee failure without needing real
        // open-order state manipulation.
        let mut lim = limits();
        lim.max_open_orders_total = 0;
        let gate = RiskGate { limits: lim };
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("too_many_open_orders"));
    }

    #[test]
    fn rejects_too_many_open_orders_for_market() {
        let mut lim = limits();
        lim.max_open_orders_per_market = 0; // force per-market cap hit
        let gate = RiskGate { limits: lim };
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("too_many_open_orders_for_market"));
    }

    #[test]
    fn rejects_position_quantity_cap() {
        let mut lim = limits();
        lim.max_position_quantity_per_instrument = 100.0; // 200 shares would exceed
        let gate = RiskGate { limits: lim };
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("position_quantity_too_large"));
    }

    #[test]
    fn rejects_sell_exceeds_inventory() {
        let gate = gate();
        let pf = Portfolio::new(10_000.0); // no positions
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let mut order = order_entry();
        order.side = Side::SellYes;
        order.shares = 100.0;
        // portfolio has 0 yes shares, trying to sell 100
        let result = gate.check(&order, &pf, &marks(m, 0.50), &exp, key, -100.0);
        assert_eq!(result, RiskDecision::Reject("sell_exceeds_inventory"));
    }

    #[test]
    fn rejects_below_free_cash_floor() {
        let mut lim = limits();
        lim.min_free_cash_usd = 9_999.0; // cash is 10_000, buying 200@0.50=100 => 9900 < 9999
        let gate = RiskGate { limits: lim };
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("free_cash_too_low"));
    }

    #[test]
    fn rejects_over_gross_notional() {
        let mut lim = limits();
        lim.max_gross_notional_usd = 50.0; // 200*0.50=100 > 50
        let gate = RiskGate { limits: lim };
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("gross_exposure_too_large"));
    }

    #[test]
    fn rejects_over_market_net_notional() {
        let mut lim = limits();
        lim.max_net_notional_per_market_usd = 50.0; // 200*0.50=100 > 50
        let gate = RiskGate { limits: lim };
        let pf = Portfolio::new(10_000.0);
        let (exp, key) = no_exposure();
        let m = MarketId(0);
        let result = gate.check(&order_entry(), &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("market_net_exposure_too_large"));
    }

    #[test]
    fn bypasses_entry_caps_for_close_intent() {
        // Set all entry-only caps to values that would trigger for Entry,
        // but a Close order on a market where the portfolio holds shares
        // should still approve (only free-cash floor applies).
        let mut lim = limits();
        lim.max_order_notional_usd = 1.0;     // 200*0.50=100 >> 1 — entry would reject
        lim.max_gross_notional_usd = 1.0;      // would reject entry
        lim.max_net_notional_per_market_usd = 1.0;
        lim.max_position_quantity_per_instrument = 1.0;
        lim.max_open_orders_total = 0;
        lim.max_open_orders_per_market = 0;
        lim.max_correlated_net_shares = 1.0;
        let gate = RiskGate { limits: lim };

        // Portfolio holds 200 YES so the sell is within inventory.
        let mut pf = Portfolio::new(10_000.0);
        let m = MarketId(0);
        pf.apply_fill(&FillReport {
            order: OrderId(99),
            market: m,
            side: Side::BuyYes,
            shares: 200.0,
            price: 0.50,
            fee_usd: 0.0,
            liquidity: FillLiquidity::Taker,
            ts: 0,
        });

        let close_order = OrderIntent {
            id: OrderId(2),
            market: m,
            side: Side::SellYes,
            shares: 200.0,
            max_depth: 1,
            limit_price: Some(0.50),
            tag: "close",
            kind: IntentKind::Close,
        };

        let mut exp = ExposureState::default();
        let key = ExposureKey { token: Token::Btc, window: 0 };
        exp.apply(key, 900.0); // would exceed the 1-share correlated cap

        let result = gate.check(&close_order, &pf, &marks(m, 0.50), &exp, key, -200.0);
        assert_eq!(result, RiskDecision::Approve);
    }

    #[test]
    fn close_rejects_when_insufficient_cash_for_buy_close() {
        let gate = gate();
        // Portfolio with only $10 cash, trying to buy 200@0.50=$100 as a close.
        let pf = Portfolio::new(10.0);
        let m = MarketId(0);
        let mut exp = ExposureState::default();
        let key = ExposureKey { token: Token::Btc, window: 0 };
        exp.apply(key, 0.0);

        let close_order = OrderIntent {
            id: OrderId(3),
            market: m,
            side: Side::BuyYes,
            shares: 200.0,
            max_depth: 1,
            limit_price: Some(0.50),
            tag: "rescue",
            kind: IntentKind::Close,
        };

        let result = gate.check(&close_order, &pf, &marks(m, 0.50), &exp, key, 200.0);
        assert_eq!(result, RiskDecision::Reject("free_cash_too_low"));
    }
}
