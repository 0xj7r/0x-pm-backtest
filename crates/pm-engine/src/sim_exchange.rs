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
    /// Maker rebate in basis points credited on notional.
    pub maker_rebate_bps: f64,
}

// Both fields are Copy so MarketBook can be Copy too; no heap allocation, no accidental clone.
#[derive(Copy, Clone)]
struct MarketBook {
    replay: ReplayEvent,
    no_book: NoBook,
}

/// A taker order waiting for `realize_ts` before pricing against the book.
/// Price is NOT locked at submit time; it re-reads the current book at realize time,
/// matching the `PendingTakerOrder` / `process_pending_takers` semantics in runner.rs.
struct PendingTaker {
    order: OrderIntent,
    realize_ts: Ts,
}

struct PendingFill {
    report: FillReport,
    realize_ts: Ts,
}

/// A resting (maker) limit order.
#[derive(Clone, Copy)]
struct RestingOrder {
    id: OrderId,
    market: MarketId,
    side: Side,
    shares: f64,
    limit_price: f32,
    submit_ts: Ts,
}

pub struct SimExchange {
    cfg: SimExchangeConfig,
    books: HashMap<MarketId, MarketBook>,
    pending_takers: Vec<PendingTaker>,
    pending_fills: Vec<PendingFill>,
    resting: Vec<RestingOrder>,
}

impl SimExchange {
    pub fn new(cfg: SimExchangeConfig) -> Self {
        Self {
            cfg,
            books: HashMap::new(),
            pending_takers: Vec::new(),
            pending_fills: Vec::new(),
            resting: Vec::new(),
        }
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

    /// Drain pending takers whose `realize_ts <= now`, pricing each against the
    /// current book (re-read at realize time, not locked at submit time).
    fn realize_pending_takers(&mut self, now: Ts) {
        let mut i = 0;
        while i < self.pending_takers.len() {
            if self.pending_takers[i].realize_ts > now {
                i += 1;
                continue;
            }
            let pt = self.pending_takers.swap_remove(i);
            let Some(book) = self.books.get(&pt.order.market) else {
                continue;
            };
            let replay = book.replay;
            let no_book = book.no_book;
            let Some((price, filled_shares)) = Self::depth_sweep(
                pt.order.side,
                &replay,
                &no_book,
                pt.order.shares,
                pt.order.max_depth,
                pt.order.limit_price,
            ) else {
                continue;
            };
            let notional = filled_shares * price as f64;
            let fee_usd = notional * self.cfg.taker_fee_bps / 10_000.0;
            self.pending_fills.push(PendingFill {
                report: FillReport {
                    order: pt.order.id,
                    market: pt.order.market,
                    side: pt.order.side,
                    shares: filled_shares,
                    price,
                    fee_usd,
                    liquidity: FillLiquidity::Taker,
                    ts: pt.realize_ts,
                },
                realize_ts: pt.realize_ts,
            });
        }
    }

    /// Check resting orders for a book-cross after an `on_book` update.
    /// Port of `check_resting_fills` from runner.rs: BuyYes fills when
    /// `yes_ask < limit` (strict cross), SellYes fills when `yes_bid > limit`.
    fn check_book_cross_fills(&mut self, market: MarketId, replay: &ReplayEvent, now: Ts) {
        let mut i = 0;
        while i < self.resting.len() {
            let r = self.resting[i];
            if r.market != market {
                i += 1;
                continue;
            }
            let crossed = match r.side {
                Side::BuyYes | Side::SellNo => {
                    replay.yes_ask > 0.0 && replay.yes_ask < r.limit_price
                }
                Side::SellYes | Side::BuyNo => {
                    replay.yes_bid > 0.0 && replay.yes_bid > r.limit_price
                }
            };
            if !crossed {
                i += 1;
                continue;
            }
            let fill_price = maker_fill_price(r.side, r.limit_price);
            let notional = r.shares * fill_price as f64;
            let rebate = notional * self.cfg.maker_rebate_bps / 10_000.0;
            let fee_usd = -rebate;
            self.pending_fills.push(PendingFill {
                report: FillReport {
                    order: r.id,
                    market: r.market,
                    side: r.side,
                    shares: r.shares,
                    price: fill_price,
                    fee_usd,
                    liquidity: FillLiquidity::Maker,
                    ts: now,
                },
                realize_ts: now,
            });
            self.resting.swap_remove(i);
        }
    }

    /// Check resting orders against a real trade print (trade-tape maker fill).
    /// Port of `check_trade_driven_resting_fills` from runner.rs:
    /// - `trade.ts_ns > r.submit_ts` (queue priority: no fills for orders placed after the trade)
    /// - aggressor direction must cross the resting level
    fn check_trade_tape_fills(&mut self, market: MarketId, tick: &TradeTick, now: Ts) {
        let mut remaining = tick.size as f64;
        let mut i = 0;
        while remaining > 0.0 && i < self.resting.len() {
            let r = self.resting[i];
            if r.market != market {
                i += 1;
                continue;
            }
            // Queue priority: trade must arrive AFTER the order was submitted.
            if tick.ts_ns <= r.submit_ts {
                i += 1;
                continue;
            }
            let fills_order = match r.side {
                // Resting buy: a sell aggressor (aggressor_buy=false) at <= limit fills us.
                Side::BuyYes | Side::SellNo => {
                    !tick.aggressor_buy && tick.price <= r.limit_price
                }
                // Resting sell: a buy aggressor (aggressor_buy=true) at >= limit fills us.
                Side::SellYes | Side::BuyNo => {
                    tick.aggressor_buy && tick.price >= r.limit_price
                }
            };
            if !fills_order {
                i += 1;
                continue;
            }
            let fill_shares = remaining.min(r.shares);
            let fill_price = maker_fill_price(r.side, r.limit_price);
            let notional = fill_shares * fill_price as f64;
            let rebate = notional * self.cfg.maker_rebate_bps / 10_000.0;
            let fee_usd = -rebate;
            self.pending_fills.push(PendingFill {
                report: FillReport {
                    order: r.id,
                    market: r.market,
                    side: r.side,
                    shares: fill_shares,
                    price: fill_price,
                    fee_usd,
                    liquidity: FillLiquidity::Maker,
                    ts: now,
                },
                realize_ts: now,
            });
            remaining -= fill_shares;
            if fill_shares >= self.resting[i].shares {
                self.resting.swap_remove(i);
            } else {
                self.resting[i].shares -= fill_shares;
                i += 1;
            }
        }
    }
}

/// Translate the resting limit price to the native fill price for each side.
/// BuyYes/SellYes fill at the limit in YES terms.
/// BuyNo/SellNo: the limit was expressed as a YES price; native NO price = 1 - limit.
fn maker_fill_price(side: Side, limit_price: f32) -> f32 {
    match side {
        Side::BuyYes | Side::SellYes => limit_price,
        Side::BuyNo | Side::SellNo => 1.0 - limit_price,
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
        now: Ts,
    ) {
        self.books.insert(market, MarketBook { replay: *replay, no_book: *no_book });
        self.check_book_cross_fills(market, replay, now);
    }

    fn on_trade(&mut self, market: pm_types::MarketId, tick: &TradeTick, now: Ts) {
        self.check_trade_tape_fills(market, tick, now);
    }

    fn submit(&mut self, order: OrderIntent, now: Ts) -> SubmitAck {
        if let Some(limit_price) = order.limit_price {
            self.resting.push(RestingOrder {
                id: order.id,
                market: order.market,
                side: order.side,
                shares: order.shares,
                limit_price,
                submit_ts: now,
            });
            return SubmitAck::Accepted;
        }

        let Some(book) = self.books.get(&order.market) else {
            return SubmitAck::Rejected;
        };
        let replay = book.replay;
        let no_book = book.no_book;

        if self.cfg.taker_latency_ms == 0 {
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
            self.pending_fills.push(PendingFill {
                report: FillReport {
                    order: order.id,
                    market: order.market,
                    side: order.side,
                    shares: filled_shares,
                    price,
                    fee_usd,
                    liquidity: FillLiquidity::Taker,
                    ts: now,
                },
                realize_ts: now,
            });
        } else {
            let realize_ts = now + self.cfg.taker_latency_ms as i64 * 1_000_000;
            self.pending_takers.push(PendingTaker { order, realize_ts });
        }

        SubmitAck::Accepted
    }

    fn cancel(&mut self, _id: OrderId, _now: Ts) -> CancelAck {
        CancelAck::Unknown
    }

    fn poll_fills(&mut self, now: Ts) -> Vec<FillReport> {
        self.realize_pending_takers(now);
        let mut ready = Vec::new();
        let mut remaining = Vec::new();
        for p in self.pending_fills.drain(..) {
            if p.realize_ts <= now {
                ready.push(p.report);
            } else {
                remaining.push(p);
            }
        }
        self.pending_fills = remaining;
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seams::{IntentKind, OrderId, OrderIntent};
    use pm_types::{BookLevel, MarketId, NoBook, ReplayEvent, ReplayFlags, TradeTick};

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

    fn ev_with_bid(yes_bid0: f32, yes_bid_sz: f32) -> ReplayEvent {
        let mut bids = [BookLevel::default(); 5];
        bids[0] = BookLevel { price: yes_bid0, size: yes_bid_sz };
        ReplayEvent {
            ts_ns: 1000,
            market_id: MarketId(0),
            yes_mid: 0.5,
            yes_bid: yes_bid0,
            yes_ask: 0.60,
            volume: 0.0,
            bids,
            asks: Default::default(),
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

    // --- Task 3 tests ---

    #[test]
    fn taker_fill_is_deferred_by_latency() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 500,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        let e = ev(0.60, 1000.0);
        ex.on_book(MarketId(0), &e, &NoBook::default(), 1_000_000_000);
        ex.submit(
            OrderIntent {
                id: OrderId(1),
                market: MarketId(0),
                side: Side::BuyYes,
                shares: 100.0,
                max_depth: 1,
                limit_price: None,
                tag: "t",
                kind: IntentKind::Entry,
            },
            1_000_000_000,
        );
        assert!(ex.poll_fills(1_000_000_000).is_empty(), "should not fill at t+0");
        assert_eq!(
            ex.poll_fills(1_000_000_000 + 500_000_000).len(),
            1,
            "should fill at t+500ms"
        );
    }

    #[test]
    fn taker_partial_fills_when_depth_insufficient() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        let e = ev(0.60, 40.0); // only 40 shares at top of book
        ex.on_book(MarketId(0), &e, &NoBook::default(), 1000);
        ex.submit(
            OrderIntent {
                id: OrderId(1),
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
        assert!(
            (fills[0].shares - 40.0).abs() < 1e-6,
            "expected 40 filled shares, got {}",
            fills[0].shares
        );
    }

    // --- Task 4 tests ---

    #[test]
    fn maker_fills_on_book_cross() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        // resting BuyYes limit at 0.55; fills when yes_ask drops strictly below 0.55
        ex.on_book(MarketId(0), &ev(0.60, 1000.0), &NoBook::default(), 1000);
        ex.submit(
            OrderIntent {
                id: OrderId(1),
                market: MarketId(0),
                side: Side::BuyYes,
                shares: 50.0,
                max_depth: 1,
                limit_price: Some(0.55),
                tag: "m",
                kind: IntentKind::Entry,
            },
            1000,
        );
        assert!(ex.poll_fills(1000).is_empty(), "ask 0.60 > 0.55, no fill yet");
        // ask crosses below limit
        ex.on_book(MarketId(0), &ev(0.54, 1000.0), &NoBook::default(), 2000);
        let fills = ex.poll_fills(2000);
        assert_eq!(fills.len(), 1);
        assert!(
            (fills[0].shares - 50.0).abs() < 1e-6,
            "expected 50 shares filled, got {}",
            fills[0].shares
        );
    }

    #[test]
    fn maker_fills_on_real_trade_tape_cross_with_queue_priority() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        ex.on_book(MarketId(0), &ev(0.60, 1000.0), &NoBook::default(), 1000);
        ex.submit(
            OrderIntent {
                id: OrderId(1),
                market: MarketId(0),
                side: Side::BuyYes,
                shares: 50.0,
                max_depth: 1,
                limit_price: Some(0.55),
                tag: "m",
                kind: IntentKind::Entry,
            },
            1000,
        );
        // a real sell aggressor at 0.55 AFTER submit_ts crosses the resting buy
        ex.on_trade(
            MarketId(0),
            &TradeTick { ts_ns: 1500, price: 0.55, size: 50.0, aggressor_buy: false },
            1500,
        );
        let fills = ex.poll_fills(1500);
        assert_eq!(fills.len(), 1, "should have one maker fill from trade tape");
    }

    // --- Review fold-in: SellYes and SellNo taker coverage ---

    #[test]
    fn sell_yes_taker_sweeps_yes_bids() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        let e = ev_with_bid(0.48, 200.0);
        ex.on_book(MarketId(0), &e, &NoBook::default(), 1000);
        ex.submit(
            OrderIntent {
                id: OrderId(3),
                market: MarketId(0),
                side: Side::SellYes,
                shares: 100.0,
                max_depth: 1,
                limit_price: None,
                tag: "t",
                kind: IntentKind::Close,
            },
            1000,
        );
        let fills = ex.poll_fills(1000);
        assert_eq!(fills.len(), 1);
        assert!(
            (fills[0].price - 0.48).abs() < 1e-6,
            "SellYes should fill at YES bid 0.48, got {}",
            fills[0].price
        );
        assert!(
            (fills[0].shares - 100.0).abs() < 1e-6,
            "expected 100 shares, got {}",
            fills[0].shares
        );
    }

    #[test]
    fn sell_no_taker_sweeps_no_bids() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        let e = ev(0.60, 1000.0);
        let mut nb = NoBook::default();
        nb.bids[0] = BookLevel { price: 0.38, size: 500.0 }; // real NO bid
        ex.on_book(MarketId(0), &e, &nb, 1000);
        ex.submit(
            OrderIntent {
                id: OrderId(4),
                market: MarketId(0),
                side: Side::SellNo,
                shares: 80.0,
                max_depth: 1,
                limit_price: None,
                tag: "t",
                kind: IntentKind::Close,
            },
            1000,
        );
        let fills = ex.poll_fills(1000);
        assert_eq!(fills.len(), 1);
        assert!(
            (fills[0].price - 0.38).abs() < 1e-6,
            "SellNo should fill at real NO bid 0.38, got {}",
            fills[0].price
        );
    }

    // --- Review fold-in: multi-level VWAP sweep ---

    #[test]
    fn multi_level_buy_yes_sweep_vwap() {
        let mut ex = SimExchange::new(SimExchangeConfig {
            taker_latency_ms: 0,
            taker_fee_bps: 0.0,
            maker_rebate_bps: 0.0,
        });
        let mut asks = [BookLevel::default(); 5];
        asks[0] = BookLevel { price: 0.60, size: 50.0 }; // 50 @ 0.60
        asks[1] = BookLevel { price: 0.62, size: 100.0 }; // 100 @ 0.62
        let e = ReplayEvent {
            ts_ns: 1000,
            market_id: MarketId(0),
            yes_mid: 0.5,
            yes_bid: 0.59,
            yes_ask: 0.60,
            volume: 0.0,
            bids: Default::default(),
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        };
        ex.on_book(MarketId(0), &e, &NoBook::default(), 1000);
        ex.submit(
            OrderIntent {
                id: OrderId(5),
                market: MarketId(0),
                side: Side::BuyYes,
                shares: 100.0, // needs both levels: 50 @ 0.60 + 50 @ 0.62
                max_depth: 2,
                limit_price: None,
                tag: "t",
                kind: IntentKind::Entry,
            },
            1000,
        );
        let fills = ex.poll_fills(1000);
        assert_eq!(fills.len(), 1);
        // VWAP: (50*0.60 + 50*0.62) / 100 = (30 + 31) / 100 = 0.61
        let expected_vwap = (50.0 * 0.60 + 50.0 * 0.62) / 100.0;
        assert!(
            (fills[0].price - expected_vwap as f32).abs() < 1e-5,
            "expected VWAP {:.4}, got {:.6}",
            expected_vwap,
            fills[0].price
        );
        assert!((fills[0].shares - 100.0).abs() < 1e-6);
    }
}
