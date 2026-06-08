use pm_engine::engine::Engine;
use pm_engine::event::{EngineEvent, Token};
use pm_engine::portfolio::Portfolio;
use pm_engine::risk::{RiskGate, RiskLimits};
use pm_engine::testkit::{InstantExchange, ScriptedFeed, SimClock};
use pm_strategy::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{MarketId, NoBook, ReplayEvent, ReplayFlags, SpotHistory, TradeHistory};
use std::cell::Cell;
use std::rc::Rc;

/// Test strategy: buy 100 YES on the first event it sees, then hold.
struct BuyOnce {
    fired: bool,
}

impl Strategy for BuyOnce {
    fn on_event(
        &mut self,
        _e: &ReplayEvent,
        _c: &Ctx,
        _s: &SpotHistory,
        _t: &TradeHistory,
    ) -> StrategyOutput {
        if self.fired {
            return StrategyOutput::hold();
        }
        self.fired = true;
        StrategyOutput::one(OrderRequest {
            side: Side::BuyYes,
            shares: 100.0,
            max_depth: 1,
            limit_price: None,
            tag: "buy",
        })
    }
}

fn limits() -> RiskLimits {
    RiskLimits {
        max_order_notional_usd: 1e9,
        max_gross_notional_usd: 1e9,
        max_net_notional_per_market_usd: 1e9,
        max_position_quantity_per_instrument: 1e9,
        min_free_cash_usd: 0.0,
        min_portfolio_equity_usd: 0.0,
        max_open_orders_total: 99,
        max_open_orders_per_market: 99,
        max_correlated_net_shares: 1e9,
    }
}

fn ev(ts: i64, m: MarketId, close: bool) -> EngineEvent {
    let flags = if close {
        ReplayFlags::MARKET_CLOSE
    } else {
        ReplayFlags::BOOK_UPDATE
    };
    let replay = ReplayEvent {
        ts_ns: ts,
        market_id: m,
        yes_mid: 0.6,
        yes_bid: 0.59,
        yes_ask: 0.61,
        volume: 0.0,
        bids: Default::default(),
        asks: Default::default(),
        spot_price: 0.0,
        flags,
    };
    EngineEvent::Market { replay, no_book: NoBook::default() }
}

#[test]
fn engine_buys_once_then_settles_yes() {
    let m = MarketId(0);
    let clock_cell = Rc::new(Cell::new(0i64));
    let mut feed = ScriptedFeed::new(
        vec![ev(10, m, false), ev(20, m, false), ev(30, m, true)],
        clock_cell.clone(),
    );
    let mut ex = InstantExchange::new(0.60, 0.0);
    let clock = SimClock { ts: clock_cell };

    let mut engine = Engine::new(
        BuyOnce { fired: false },
        Portfolio::new(1_000.0),
        RiskGate { limits: limits() },
        |_m| (Token::Btc, 0),
    );
    engine.run(&mut feed, &mut ex, &clock);

    // One order submitted; bought 100 YES @0.60 => cash 940; resolved YES => +100 => 1040.
    assert_eq!(ex.submitted.len(), 1);
    assert!(
        (engine.portfolio.free_cash_usd() - 1_040.0).abs() < 1e-4,
        "expected cash ~1040, got {}",
        engine.portfolio.free_cash_usd()
    );
}
