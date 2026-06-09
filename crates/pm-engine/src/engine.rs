use crate::enrich::{CtxEnricher, PriorRanges};
use crate::event::{EngineEvent, Token};
use crate::exposure::{ExposureKey, ExposureState};
use crate::host::build_ctx;
use crate::portfolio::Portfolio;
use crate::risk::{RiskDecision, RiskGate};
use crate::seams::{Clock, Exchange, Feed, IntentKind, OrderId, OrderIntent};
use pm_strategy::{Side, Strategy};
use pm_types::{MarketId, ReplayEvent, ReplayFlags, SpotHistory, TradeHistory};
use std::collections::HashMap;

/// Per-market open/close/resolution supplied by the driver from discovery
/// metadata. Empty in Phase-1 tests, where the engine falls back to the
/// first-seen ts for timing and `yes_mid >= 0.5` for resolution.
#[derive(Debug, Clone, Copy)]
pub struct MarketMeta {
    pub open_ns: i64,
    pub close_ns: i64,
    /// Real resolved outcome from discovery; `None` falls back to `yes_mid >= 0.5`.
    pub resolved_yes: Option<bool>,
}

struct MarketCtx {
    open_ns: i64,
    close_ns: i64,
    events_seen: u64,
}

pub struct Engine<S: Strategy> {
    strategy: S,
    pub portfolio: Portfolio,
    pub exposure: ExposureState,
    risk: RiskGate,
    markets: HashMap<MarketId, MarketCtx>,
    /// Driver-supplied open/close/resolution per market. Empty → Phase-1 fallback.
    market_meta: HashMap<MarketId, MarketMeta>,
    next_order_id: u64,
    /// Empty histories in Phase 1; Phase 2 maintains rolling windows.
    spot: SpotHistory,
    trades: TradeHistory,
    marks: HashMap<MarketId, f32>,
    /// Maps a market to its (token, window) for exposure keying.
    classify: fn(MarketId) -> (Token, i64),
    /// Optional enricher that populates regime/model/prior-range Ctx fields.
    /// None in Phase-1 tests; set via `with_enricher` for the backtest driver.
    enricher: Option<CtxEnricher>,
    /// Prior-range values supplied by the driver (one set per engine instance).
    prior_ranges: PriorRanges,
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
            market_meta: HashMap::new(),
            next_order_id: 1,
            spot: SpotHistory::default(),
            trades: TradeHistory::default(),
            marks: HashMap::new(),
            classify,
            enricher: None,
            prior_ranges: PriorRanges::default(),
            trace: Vec::new(),
        }
    }

    /// Attach a `CtxEnricher` plus precomputed spot and trade histories for
    /// the Phase-2 backtest driver. The enricher populates regime/model/
    /// prior-range fields on `Ctx` before `on_event_scored` is called.
    ///
    /// Phase-1 tests use `Engine::new` and see `enricher = None`, so they are
    /// unaffected by this addition.
    pub fn with_enricher(
        mut self,
        enricher: CtxEnricher,
        spot: SpotHistory,
        trades: TradeHistory,
        prior_ranges: PriorRanges,
    ) -> Self {
        self.enricher = Some(enricher);
        self.spot = spot;
        self.trades = trades;
        self.prior_ranges = prior_ranges;
        self
    }

    /// Attach real per-market open/close/resolution from discovery metadata.
    /// Without it the engine uses the first-seen ts for timing and infers
    /// resolution from `yes_mid` (the Phase-1 behavior).
    pub fn with_market_meta(mut self, meta: HashMap<MarketId, MarketMeta>) -> Self {
        self.market_meta = meta;
        self
    }

    pub fn run<F: Feed, X: Exchange, C: Clock>(&mut self, feed: &mut F, ex: &mut X, clock: &C) {
        while let Some(ev) = feed.next() {
            match ev {
                EngineEvent::Market { replay, no_book } => {
                    ex.on_book(replay.market_id, &replay, &no_book, clock.now());
                    self.on_market(&replay, ex, clock);
                }
                EngineEvent::Trade { market, tick } => {
                    ex.on_trade(market, &tick, clock.now());
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

        let meta = self.market_meta.get(&e.market_id).copied();
        let mc = self.markets.entry(e.market_id).or_insert_with(|| MarketCtx {
            open_ns: meta.map(|m| m.open_ns).unwrap_or(e.ts_ns),
            close_ns: meta.map(|m| m.close_ns).unwrap_or(e.ts_ns),
            events_seen: 0,
        });

        if e.flags.contains(ReplayFlags::MARKET_CLOSE) {
            // Real resolution from discovery metadata; fall back to yes_mid inference
            // when no metadata was supplied (Phase-1 tests).
            let resolved_yes = meta.and_then(|m| m.resolved_yes).unwrap_or(e.yes_mid >= 0.5);
            self.portfolio.settle(e.market_id, resolved_yes);
            self.strategy.on_market_resolved(e.yes_mid, resolved_yes);
            return;
        }

        mc.events_seen += 1;
        let open_ns = mc.open_ns;
        let close_ns = mc.close_ns;
        let events_seen = mc.events_seen;
        let btc_key = ExposureKey { token: Token::Btc, window };
        let eth_key = ExposureKey { token: Token::Eth, window };

        let mut ctx = build_ctx(
            &self.portfolio,
            e.market_id,
            events_seen,
            close_ns,
            btc_key,
            eth_key,
            &self.exposure,
        );

        if let Some(enricher) = &mut self.enricher {
            enricher.fill_regime(&mut ctx, e.ts_ns, &self.spot);

            // Real market open time from discovery metadata (first-seen ts when
            // no metadata supplied). For a 5m market the driver sets open = close - 300s.
            let secs_since_open = ((e.ts_ns - open_ns).max(0) as f64 / 1e9) as i64;
            enricher.fill_model(&mut ctx, e, e.ts_ns, secs_since_open, &self.spot);

            enricher.fill_prior_range(&mut ctx, self.prior_ranges);
        }

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
