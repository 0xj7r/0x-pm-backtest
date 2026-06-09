use pm_engine::engine::Engine;
use pm_engine::event::{EngineEvent, Token};
use pm_engine::exposure::ExposureKey;
use pm_engine::portfolio::Portfolio;
use pm_engine::risk::{RiskGate, RiskLimits};
use pm_engine::testkit::{InstantExchange, ScriptedFeed, SimClock};
use pm_strategy::{BonereaperV2, BonereaperV2Config, Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{MarketId, NoBook, ReplayEvent, ReplayFlags, SpotHistory, TradeHistory};
use std::cell::Cell;
use std::rc::Rc;

/// Test strategy: buy 100 YES on the first event it sees, then hold.
#[derive(Clone)]
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

fn run_once() -> Vec<(i64, &'static str, MarketId, f64)> {
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
    engine.trace
}

#[test]
fn golden_trace_is_deterministic() {
    assert_eq!(run_once(), run_once());
}

/// Buy 100 YES the first time the engine sees each market (when position is flat).
#[derive(Clone, Default)]
struct BuyOnceEach;

impl Strategy for BuyOnceEach {
    fn on_event(
        &mut self,
        _e: &ReplayEvent,
        c: &Ctx,
        _s: &SpotHistory,
        _t: &TradeHistory,
    ) -> StrategyOutput {
        if c.yes_shares == 0.0 && c.no_shares == 0.0 {
            StrategyOutput::one(OrderRequest {
                side: Side::BuyYes,
                shares: 100.0,
                max_depth: 1,
                limit_price: None,
                tag: "buy_each",
            })
        } else {
            StrategyOutput::hold()
        }
    }
}

#[test]
fn interleaves_two_markets_in_ts_order_sharing_capital() {
    let m1 = MarketId(0);
    let m2 = MarketId(1);
    let clock_cell = Rc::new(Cell::new(0i64));
    let mut feed = ScriptedFeed::new(
        vec![
            ev(10, m1, false),
            ev(15, m2, false),
            ev(30, m1, true),
            ev(35, m2, true),
        ],
        clock_cell.clone(),
    );
    let mut ex = InstantExchange::new(0.50, 0.0);
    let clock = SimClock { ts: clock_cell };
    let mut engine = Engine::new(
        BuyOnceEach,
        Portfolio::new(1_000.0),
        RiskGate { limits: limits() },
        |_m| (Token::Btc, 0),
    );
    engine.run(&mut feed, &mut ex, &clock);
    // Both markets bought 100 @0.50 from the shared pool: 1000 - 50 - 50 = 900,
    // then both resolve YES: +100 +100 => 1100.
    assert_eq!(ex.submitted.len(), 2);
    assert!(
        (engine.portfolio.free_cash_usd() - 1_100.0).abs() < 1e-4,
        "expected cash ~1100, got {}",
        engine.portfolio.free_cash_usd()
    );
}

#[test]
fn exposure_is_released_on_settlement() {
    let m = MarketId(0);
    let clock_cell = Rc::new(Cell::new(0i64));
    let mut feed = ScriptedFeed::new(
        vec![ev(10, m, false), ev(30, m, true)],
        clock_cell.clone(),
    );
    let mut ex = InstantExchange::new(0.50, 0.0);
    let clock = SimClock { ts: clock_cell };
    let mut engine = Engine::new(
        BuyOnce { fired: false },
        Portfolio::new(1_000.0),
        RiskGate { limits: limits() },
        |_m| (Token::Btc, 0),
    );
    engine.run(&mut feed, &mut ex, &clock);
    // BuyOnce bought 100 YES (+100 signed). After the market settles, the
    // (Btc,0) exposure must be released back to ~0, not stuck at +100.
    let net = engine.exposure.net(ExposureKey { token: Token::Btc, window: 0 });
    assert!(net.abs() < 1e-9, "exposure must be released on settle, got {net}");
}

/// Fires at most twice, tracked in per-instance state. Used to prove the engine
/// gives each market its own strategy instance (budgets are not shared).
#[derive(Clone, Default)]
struct FireTwice {
    fires: u32,
}
impl Strategy for FireTwice {
    fn on_event(
        &mut self,
        _e: &ReplayEvent,
        _c: &Ctx,
        _s: &SpotHistory,
        _t: &TradeHistory,
    ) -> StrategyOutput {
        if self.fires >= 2 {
            return StrategyOutput::hold();
        }
        self.fires += 1;
        StrategyOutput::one(OrderRequest {
            side: Side::BuyYes,
            shares: 1.0,
            max_depth: 1,
            limit_price: None,
            tag: "fire",
        })
    }
}

#[test]
fn each_market_gets_its_own_strategy_instance() {
    let m1 = MarketId(0);
    let m2 = MarketId(1);
    let clock_cell = Rc::new(Cell::new(0i64));
    // 3 book events per market (so FireTwice could fire up to twice each) then close.
    let mut feed = ScriptedFeed::new(
        vec![
            ev(10, m1, false), ev(11, m1, false), ev(12, m1, false),
            ev(20, m2, false), ev(21, m2, false), ev(22, m2, false),
            ev(30, m1, true), ev(31, m2, true),
        ],
        clock_cell.clone(),
    );
    let mut ex = InstantExchange::new(0.50, 0.0);
    let clock = SimClock { ts: clock_cell };
    let mut engine = Engine::new(
        FireTwice::default(),
        Portfolio::new(1_000_000.0),
        RiskGate { limits: limits() },
        |_m| (Token::Btc, 0),
    );
    engine.run(&mut feed, &mut ex, &clock);
    // 2 fires per market * 2 markets = 4 submits. A single shared instance caps at 2.
    assert_eq!(ex.submitted.len(), 4, "expected 2 fires per market (4 total)");
}

#[test]
fn hosts_real_br2_deterministically() {
    let m = MarketId(0);
    let make = || {
        let clock_cell = Rc::new(Cell::new(0i64));
        // 50 book updates then a close; br2 is selective and may not trade on flat
        // synthetic data — we assert determinism + no panic, not a fill.
        let evs: Vec<EngineEvent> = (0..50i64)
            .map(|i| ev(1_000 + i * 1_000, m, false))
            .chain(std::iter::once(ev(1_000 + 50 * 1_000, m, true)))
            .collect();
        let mut feed = ScriptedFeed::new(evs, clock_cell.clone());
        let mut ex = InstantExchange::new(0.50, 0.0);
        let clock = SimClock { ts: clock_cell };
        let strat = BonereaperV2::new(BonereaperV2Config::default());
        let mut engine = Engine::new(
            strat,
            Portfolio::new(1_000.0),
            RiskGate { limits: limits() },
            |_m| (Token::Btc, 0),
        );
        engine.run(&mut feed, &mut ex, &clock);
        engine.trace
    };
    assert_eq!(make(), make());
}
