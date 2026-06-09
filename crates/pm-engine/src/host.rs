use crate::exposure::{ExposureKey, ExposureState};
use crate::portfolio::Portfolio;
use pm_strategy::Ctx;
use pm_types::MarketId;

/// Single source of `Ctx` construction for both the live driver and the
/// backtest driver. Cross-market exposure fields come from `ExposureState` so
/// both drivers see identical context without recomputing from positions.
///
/// Phase-1 fills only the portfolio/exposure/market-derivable fields; the
/// regime/model fields are populated by the Phase-2 recorded-Feed driver.
pub fn build_ctx(
    portfolio: &Portfolio,
    market: MarketId,
    events_seen: u64,
    close_ns: i64,
    btc_key: ExposureKey,
    eth_key: ExposureKey,
    exposure: &ExposureState,
    no_book: &pm_types::NoBook,
) -> Ctx {
    let pos = portfolio.position(market);
    let no_bid = no_book.bids[0].price;
    let no_ask = no_book.asks[0].price;
    let no_mid = if no_bid > 0.0 && no_ask > 0.0 { 0.5 * (no_bid + no_ask) } else { 0.0 };
    Ctx {
        events_seen,
        yes_shares: pos.yes_shares,
        no_shares: pos.no_shares,
        cash_usdc: portfolio.free_cash_usd(),
        market_close_ns: close_ns,
        btc_net_exposure_shares: exposure.net(btc_key),
        eth_net_exposure_shares: exposure.net(eth_key),
        no_bid,
        no_ask,
        no_mid,
        ..Ctx::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Token;
    use crate::seams::{FillLiquidity, FillReport, OrderId};
    use pm_strategy::Side;
    use pm_types::MarketId;

    #[test]
    fn ctx_reflects_position_and_exposure() {
        let m = MarketId(0);
        let mut pf = Portfolio::new(1_000.0);
        pf.apply_fill(&FillReport {
            order: OrderId(1),
            market: m,
            side: Side::BuyYes,
            shares: 100.0,
            price: 0.40,
            fee_usd: 0.0,
            liquidity: FillLiquidity::Taker,
            ts: 0,
        });
        let mut exp = ExposureState::default();
        let btc = ExposureKey { token: Token::Btc, window: 0 };
        let eth = ExposureKey { token: Token::Eth, window: 0 };
        exp.apply(btc, 100.0);
        let nb = pm_types::NoBook::default();
        let ctx = build_ctx(&pf, m, 5, 1_700_000_000_000_000_000, btc, eth, &exp, &nb);
        assert_eq!(ctx.yes_shares, 100.0);
        assert_eq!(ctx.btc_net_exposure_shares, 100.0);
        assert_eq!(ctx.eth_net_exposure_shares, 0.0);
        assert_eq!(ctx.events_seen, 5);
    }

    #[test]
    fn ctx_carries_real_no_top_of_book() {
        use pm_types::{BookLevel, NoBook};
        let pf = Portfolio::new(1_000.0);
        let exp = ExposureState::default();
        let btc = ExposureKey { token: Token::Btc, window: 0 };
        let eth = ExposureKey { token: Token::Eth, window: 0 };
        let mut nb = NoBook::default();
        nb.bids[0] = BookLevel { price: 0.18, size: 100.0 };
        nb.asks[0] = BookLevel { price: 0.21, size: 100.0 };
        let ctx = build_ctx(&pf, MarketId(0), 1, 0, btc, eth, &exp, &nb);
        assert!((ctx.no_bid - 0.18).abs() < 1e-6);
        assert!((ctx.no_ask - 0.21).abs() < 1e-6);
        assert!((ctx.no_mid - 0.195).abs() < 1e-6);
    }
}
