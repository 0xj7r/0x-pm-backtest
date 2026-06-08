use crate::event::{EngineEvent, Token};
use crate::exposure::{ExposureKey, ExposureState};
use crate::host::build_ctx;
use crate::portfolio::Portfolio;
use crate::risk::{RiskDecision, RiskGate};
use crate::seams::{Clock, Exchange, Feed, IntentKind, OrderId, OrderIntent};
use pm_strategy::{Side, Strategy};
use pm_types::{MarketId, ReplayEvent, ReplayFlags, SpotHistory, TradeHistory};
use std::collections::HashMap;

struct MarketCtx {
    close_ns: i64,
    events_seen: u64,
}

pub struct Engine<S: Strategy> {
    strategy: S,
    pub portfolio: Portfolio,
    pub exposure: ExposureState,
    risk: RiskGate,
    markets: HashMap<MarketId, MarketCtx>,
    next_order_id: u64,
    /// Empty histories in Phase 1; Phase 2 maintains rolling windows.
    spot: SpotHistory,
    trades: TradeHistory,
    marks: HashMap<MarketId, f32>,
    /// Maps a market to its (token, window) for exposure keying.
    classify: fn(MarketId) -> (Token, i64),
    /// Order/fill trace for determinism testing and diagnostics.
    pub trace: Vec<(i64, &'static str, MarketId, f64)>,
}

impl<S: Strategy> Engine<S> {
    pub fn new(
        strategy: S,
        portfolio: Portfolio,
        risk: RiskGate,
        classify: fn(MarketId) -> (Token, i64),
    ) -> Self {
        Self {
            strategy,
            portfolio,
            exposure: ExposureState::default(),
            risk,
            markets: HashMap::new(),
            next_order_id: 1,
            spot: SpotHistory::default(),
            trades: TradeHistory::default(),
            marks: HashMap::new(),
            classify,
            trace: Vec::new(),
        }
    }

    pub fn run<F: Feed, X: Exchange, C: Clock>(&mut self, feed: &mut F, ex: &mut X, clock: &C) {
        while let Some(ev) = feed.next() {
            match ev {
                EngineEvent::Market { replay, no_book: _ } => {
                    self.on_market(&replay, ex, clock);
                }
            }
            for fill in ex.poll_fills(clock.now()) {
                self.trace.push((fill.ts, "fill", fill.market, fill.shares));
                self.portfolio.apply_fill(&fill);
                let (token, window) = (self.classify)(fill.market);
                let signed = signed_shares(fill.side, fill.shares);
                self.exposure.apply(ExposureKey { token, window }, signed);
            }
        }
    }

    fn on_market<X: Exchange, C: Clock>(&mut self, e: &ReplayEvent, ex: &mut X, clock: &C) {
        let (token, window) = (self.classify)(e.market_id);

        self.marks.insert(e.market_id, e.yes_mid);

        let mc = self.markets.entry(e.market_id).or_insert(MarketCtx {
            close_ns: e.ts_ns,
            events_seen: 0,
        });

        if e.flags.contains(ReplayFlags::MARKET_CLOSE) {
            // Phase 1: infer resolution from yes_mid. Phase 2 supplies an explicit event.
            let resolved_yes = e.yes_mid >= 0.5;
            self.portfolio.settle(e.market_id, resolved_yes);
            self.strategy.on_market_resolved(e.yes_mid, resolved_yes);
            return;
        }

        mc.events_seen += 1;
        let close_ns = mc.close_ns;
        let events_seen = mc.events_seen;
        let btc_key = ExposureKey { token: Token::Btc, window };
        let eth_key = ExposureKey { token: Token::Eth, window };

        let ctx = build_ctx(
            &self.portfolio,
            e.market_id,
            events_seen,
            close_ns,
            btc_key,
            eth_key,
            &self.exposure,
        );

        let (out, _model) = self.strategy.on_event_scored(e, &ctx, &self.spot, &self.trades);

        let market_key = ExposureKey { token, window };
        for req in out.orders {
            let id = OrderId(self.next_order_id);
            self.next_order_id += 1;

            let signed = signed_shares(req.side, req.shares);
            let current_net = self.exposure.net(market_key);
            // Close if the delta reduces |net exposure| (moves toward zero).
            let kind = if (current_net + signed).abs() < current_net.abs() {
                IntentKind::Close
            } else {
                IntentKind::Entry
            };

            let intent = OrderIntent {
                id,
                market: e.market_id,
                side: req.side,
                shares: req.shares,
                max_depth: req.max_depth,
                limit_price: req.limit_price,
                tag: req.tag,
                kind,
            };

            if self.risk.check(&intent, &self.portfolio, &self.marks, &self.exposure, market_key, signed)
                == RiskDecision::Approve
            {
                self.trace.push((clock.now(), "submit", intent.market, intent.shares));
                ex.submit(intent, clock.now());
            }
        }
    }
}

/// Signed share delta: YES-long and NO-short (SellNo) are positive;
/// YES-short (SellYes) and NO-long (BuyNo) are negative.
fn signed_shares(side: Side, shares: f64) -> f64 {
    match side {
        Side::BuyYes | Side::SellNo => shares,
        Side::SellYes | Side::BuyNo => -shares,
    }
}
