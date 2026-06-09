use crate::event::Ts;
use crate::seams::{
    CancelAck, Exchange, FillLiquidity, FillReport, OrderId, OrderIntent, SubmitAck,
};
use pm_strategy::Side;
use pm_types::{MarketId, NoBook, ReplayEvent, TradeTick, TAPE_DEPTH};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct SimExchangeConfig {
    /// Taker fill latency in milliseconds (0 = instant).
    pub taker_latency_ms: u64,
    /// Taker fee in basis points charged on notional.
    pub taker_fee_bps: f64,
    /// Maker rebate in basis points (unused until Task 4).
    pub maker_rebate_bps: f64,
}

#[derive(Clone)]
struct MarketBook {
    replay: ReplayEvent,
    no_book: NoBook,
}

struct PendingFill {
    report: FillReport,
    realize_ts: Ts,
}

pub struct SimExchange {
    cfg: SimExchangeConfig,
    books: HashMap<MarketId, MarketBook>,
    pending: Vec<PendingFill>,
}

impl SimExchange {
    pub fn new(cfg: SimExchangeConfig) -> Self {
        Self { cfg, books: HashMap::new(), pending: Vec::new() }
    }

    fn depth_sweep(
        side: Side,
        replay: &ReplayEvent,
        no_book: &NoBook,
        shares: f64,
        max_depth: usize,
        limit_price: Option<f32>,
    ) -> Option<(f32, f64)> {
        let depth = max_depth.clamp(1, TAPE_DEPTH);
        let mut remaining = shares.max(0.0);
        let mut filled = 0.0f64;
        let mut notional = 0.0f64;

        for level in 0..depth {
            let (price, size) = match side {
                Side::BuyYes => (replay.asks[level].price, replay.asks[level].size),
                Side::SellYes => (replay.bids[level].price, replay.bids[level].size),
                Side::BuyNo => (no_book.asks[level].price, no_book.asks[level].size),
                Side::SellNo => (no_book.bids[level].price, no_book.bids[level].size),
            };
            if price <= 0.0 || price >= 1.0 || size <= 0.0 {
                continue;
            }
            if !fill_respects_limit(side, price, limit_price) {
                continue;
            }
            let take = remaining.min(size as f64);
            if take <= 0.0 {
                break;
            }
            filled += take;
            notional += take * price as f64;
            remaining -= take;
            if remaining <= 1e-9 {
                break;
            }
        }

        if filled <= 0.0 {
            return None;
        }
        Some(((notional / filled) as f32, filled))
    }
}

fn fill_respects_limit(side: Side, price: f32, limit_price: Option<f32>) -> bool {
    let Some(limit) = limit_price else {
        return true;
    };
    match side {
        Side::BuyYes | Side::BuyNo => price <= limit,
        Side::SellYes | Side::SellNo => price >= limit,
    }
}

impl Exchange for SimExchange {
    fn on_book(
        &mut self,
        market: pm_types::MarketId,
        replay: &pm_types::ReplayEvent,
        no_book: &pm_types::NoBook,
        _now: Ts,
    ) {
        self.books.insert(market, MarketBook { replay: *replay, no_book: *no_book });
    }

    fn on_trade(&mut self, _market: pm_types::MarketId, _tick: &TradeTick, _now: Ts) {
        // Task 4 wires trade-tape maker fills here.
    }

    fn submit(&mut self, order: OrderIntent, now: Ts) -> SubmitAck {
        if order.limit_price.is_some() {
            // Maker orders: resting. Task 4 implements book-cross + trade-tape fills.
            return SubmitAck::Accepted;
        }

        let Some(book) = self.books.get(&order.market) else {
            return SubmitAck::Rejected;
        };
        let replay = book.replay;
        let no_book = book.no_book;

        let Some((price, filled_shares)) = Self::depth_sweep(
            order.side,
            &replay,
            &no_book,
            order.shares,
            order.max_depth,
            order.limit_price,
        ) else {
            return SubmitAck::Rejected;
        };

        let notional = filled_shares * price as f64;
        let fee_usd = notional * self.cfg.taker_fee_bps / 10_000.0;
        let realize_ts = now + self.cfg.taker_latency_ms as i64 * 1_000_000;

        self.pending.push(PendingFill {
            report: FillReport {
                order: order.id,
                market: order.market,
                side: order.side,
                shares: filled_shares,
                price,
                fee_usd,
                liquidity: FillLiquidity::Taker,
                ts: realize_ts,
            },
            realize_ts,
        });

        SubmitAck::Accepted
    }

    fn cancel(&mut self, _id: OrderId, _now: Ts) -> CancelAck {
        CancelAck::Unknown
    }

    fn poll_fills(&mut self, now: Ts) -> Vec<FillReport> {
        let mut ready = Vec::new();
        let mut remaining = Vec::new();
        for p in self.pending.drain(..) {
            if p.realize_ts <= now {
                ready.push(p.report);
            } else {
                remaining.push(p);
            }
        }
        self.pending = remaining;
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::{IntentKind, OrderId, OrderIntent};
    use pm_types::{BookLevel, MarketId, NoBook, ReplayEvent, ReplayFlags};

    fn ev(yes_ask0: f32, yes_ask_sz: f32) -> ReplayEvent {
        let mut asks = [BookLevel::default(); 5];
        asks[0] = BookLevel { price: yes_ask0, size: yes_ask_sz };
        ReplayEvent {
            ts_ns: 1000,
            market_id: MarketId(0),
            yes_mid: 0.5,
            yes_bid: 0.49,
            yes_ask: yes_ask0,
            volume: 0.0,
            bids: Default::default(),
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    #[test]
    fn buy_yes_taker_fills_at_real_ask_vwap_with_fee() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 100.0,
            maker_rebate_bps: 0.0,
        });
        let e = ev(0.60, 1000.0);
        ex.on_book(MarketId(0), &e, &NoBook::default(), 1000);
        let id = OrderId(1);
        ex.submit(
            OrderIntent {
                id,
                market: MarketId(0),
                side: Side::BuyYes,
                shares: 100.0,
                max_depth: 1,
                limit_price: None,
                tag: "t",
                kind: IntentKind::Entry,
            },
            1000,
        );
        let fills = ex.poll_fills(1000);
        assert_eq!(fills.len(), 1);
        assert!((fills[0].price - 0.60).abs() < 1e-6, "price was {}", fills[0].price);
        // 100bps on notional: 100 shares * 0.60 * 0.01 = 0.60
        assert!(
            (fills[0].fee_usd - 100.0 * 0.60 * 0.01).abs() < 1e-6,
            "fee_usd was {}",
            fills[0].fee_usd
        );
    }

    #[test]
    fn buy_no_taker_fills_against_real_no_ask_not_one_minus_yes() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        let e = ev(0.60, 1000.0);
        let mut nb = NoBook::default();
        nb.asks[0] = BookLevel { price: 0.45, size: 1000.0 }; // real NO ask 0.45, NOT 1-0.60=0.40
        ex.on_book(MarketId(0), &e, &nb, 1000);
        ex.submit(
            OrderIntent {
                id: OrderId(2),
                market: MarketId(0),
                side: Side::BuyNo,
                shares: 100.0,
                max_depth: 1,
                limit_price: None,
                tag: "t",
                kind: IntentKind::Entry,
            },
            1000,
        );
        let fills = ex.poll_fills(1000);
        assert_eq!(fills.len(), 1);
        assert!((fills[0].price - 0.45).abs() < 1e-6, "price was {}", fills[0].price);
    }
}
