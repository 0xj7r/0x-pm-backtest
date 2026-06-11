//! Offline validation harness: latency-modeled, cost-aware edge measurement.
//!
//! Reality rules (spec section 4.5): decisions observe data at T but fill at
//! T + latency against the book as it actually was then; fills cross the
//! spread and walk depth; fees apply; labels are the market's resolution.

mod metrics;
mod replay;
mod types;

pub use metrics::{CellReport, HuntReport, aggregate};
pub use replay::{run_market, run_market_grid};
pub use types::{
    BookTick, EntryMode, HarnessConfig, MarketRunOutput, MarketSeries, ProbSample, Side,
    TradeRecord,
};

use crate::model::AlphaModel;
use pm_types::SpotHistory;

/// Run a set of markets (each paired with its day's spot history) under one
/// config and aggregate.
pub fn run_set<'a>(
    items: impl IntoIterator<Item = (&'a MarketSeries, &'a SpotHistory)>,
    model: &AlphaModel,
    cfg: &HarnessConfig,
) -> HuntReport {
    let results: Vec<(&MarketSeries, MarketRunOutput)> = items
        .into_iter()
        .map(|(series, spot)| (series, run_market(series, spot, model, cfg)))
        .collect();
    aggregate(results.iter().map(|(s, o)| (&s.meta, o)))
}

/// The edge-vs-latency curve: identical config swept over entry latencies.
pub fn run_sweep<'a>(
    items: &[(&'a MarketSeries, &'a SpotHistory)],
    model: &AlphaModel,
    base_cfg: &HarnessConfig,
    latencies_ms: &[u64],
) -> Vec<(u64, HuntReport)> {
    latencies_ms
        .iter()
        .map(|&latency_ms| {
            let cfg = HarnessConfig {
                latency_ms,
                ..*base_cfg
            };
            (latency_ms, run_set(items.iter().copied(), model, &cfg))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{MarketMeta, Token};
    use pm_types::SpotTick;
    use pm_types::tape::{BookLevel, TAPE_DEPTH};

    fn tick_spot(ts_s: i64, price: f64) -> SpotTick {
        SpotTick {
            ts_ns: ts_s * 1_000_000_000,
            price,
            quantity: 1.0,
            is_buyer_maker: false,
        }
    }

    /// Wavy spot history (nonzero vol) for `secs` seconds, then a +1% jump at
    /// `jump_at_s` sustained to the end.
    fn spot_with_jump(secs: i64, jump_at_s: i64) -> SpotHistory {
        let mut ticks = Vec::new();
        let base = 100_000.0;
        for s in 0..secs {
            let wave = if s % 2 == 0 { 1.00005 } else { 0.99995 };
            let level = if s >= jump_at_s { base * 1.01 } else { base };
            ticks.push(tick_spot(s, level * wave));
        }
        SpotHistory::new(ticks)
    }

    fn book_levels(price: f32) -> [BookLevel; TAPE_DEPTH] {
        let mut levels = [BookLevel::default(); TAPE_DEPTH];
        levels[0] = BookLevel { price, size: 10_000.0 };
        levels
    }

    fn book_tick(ts_s: i64, bid: f32, ask: f32) -> BookTick {
        BookTick {
            ts_ns: ts_s * 1_000_000_000,
            yes_bid: bid,
            yes_ask: ask,
            bids: book_levels(bid),
            asks: book_levels(ask),
            no_bid: 0.0,
            no_ask: 0.0,
            no_bids: [BookLevel::default(); TAPE_DEPTH],
            no_asks: [BookLevel::default(); TAPE_DEPTH],
        }
    }

    #[test]
    fn real_no_ladder_preferred_over_synthetic_and_pair_cost_reported() {
        // YES book 0.50/0.52; real NO book asks at 0.45 (cheaper than the
        // synthetic 1 - 0.50 = 0.50). Belief is bearish after a -1% jump...
        // simpler: force entry with threshold -1 and a flat-ish belief; the
        // NO side should fill at 0.45, and pair cost = 0.52 + 0.45 = 0.97.
        let spot = spot_with_jump(2400, i64::MAX);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let mut ticks: Vec<BookTick> = Vec::new();
        for s in open_s..close_s {
            let mut t = book_tick(s, 0.60, 0.62); // YES rich => NO side wins edge
            t.no_bid = 0.43;
            t.no_ask = 0.45;
            t.no_bids = book_levels(0.43);
            t.no_asks = book_levels(0.45);
            ticks.push(t);
        }
        let series = MarketSeries {
            meta: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: open_s * 1_000_000_000,
                close_ts_ns: close_s * 1_000_000_000,
                strike: 100_000.0,
            },
            resolved_yes: false,
            ticks,
            date: "2026-05-01".into(),
        };
        let out = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                edge_threshold: -1.0,
                latency_ms: 0,
                exit_after_s: 0,
                ..HarnessConfig::default()
            },
        );
        let t = &out.trades[0];
        assert_eq!(t.side, Side::No);
        assert!((t.avg_price - 0.45).abs() < 1e-6, "real NO ask, got {}", t.avg_price);
        assert!((out.min_pair_cost.unwrap() - 1.07).abs() < 1e-6);
        assert!(out.real_no_coverage > 0.99);
    }

    /// Market opens at t=2000s (so vol warmup exists), closes at t=2300s.
    /// Spot jumps +1% at t=2050s; the book is slow: it stays at 0.50/0.52
    /// until `reprice_at_s`, then jumps to 0.93/0.95. Resolves YES.
    fn dislocation_market(reprice_at_s: i64) -> (MarketSeries, SpotHistory) {
        let spot = spot_with_jump(2400, 2050);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let strike = 100_000.0;
        let mut ticks = Vec::new();
        for s in open_s..close_s {
            if s < reprice_at_s {
                ticks.push(book_tick(s, 0.50, 0.52));
            } else {
                ticks.push(book_tick(s, 0.93, 0.95));
            }
        }
        (
            MarketSeries {
                meta: MarketMeta {
                    token: Token::Btc,
                    window_secs: 300,
                    open_ts_ns: open_s * 1_000_000_000,
                    close_ts_ns: close_s * 1_000_000_000,
                    strike,
                },
                resolved_yes: true,
                ticks,
                date: "2026-05-01".into(),
            },
            spot,
        )
    }

    #[test]
    fn edge_is_non_increasing_in_latency_on_a_dislocation() {
        // Book reprices 3 seconds after the spot jump: low latency catches
        // the stale 0.52 ask, high latency pays 0.95.
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let base = HarnessConfig {
            edge_threshold: 0.05,
            taker_fee_bps: 0.0,
            ..HarnessConfig::default()
        };
        let items = [(&series, &spot)];
        let sweep = run_sweep(&items, &model, &base, &[0, 1_000, 5_000, 30_000]);
        let pnls: Vec<f64> = sweep.iter().map(|(_, r)| r.aggregate.total_pnl).collect();
        for w in pnls.windows(2) {
            assert!(
                w[0] >= w[1] - 1e-9,
                "EV must be non-increasing in latency: {pnls:?}"
            );
        }
        assert!(
            pnls[0] > pnls[pnls.len() - 1],
            "latency must bite on a dislocation: {pnls:?}"
        );
    }

    #[test]
    fn zero_edge_with_fees_loses_money() {
        // Book == truth == 0.5, two markets resolving opposite ways, fees on.
        // Any entries net to -fees; with threshold 0 both sides trigger at
        // edge >= 0 only if p_exo > ask, which a fair book prevents — so we
        // force entries by setting threshold to -1 (always trade).
        let spot = spot_with_jump(2400, i64::MAX); // no jump, wavy around 100k
        let mk = |resolved_yes: bool| {
            let open_s = 2000_i64;
            let close_s = 2300_i64;
            let ticks = (open_s..close_s).map(|s| book_tick(s, 0.50, 0.50001)).collect();
            MarketSeries {
                meta: MarketMeta {
                    token: Token::Btc,
                    window_secs: 300,
                    open_ts_ns: open_s * 1_000_000_000,
                    close_ts_ns: close_s * 1_000_000_000,
                    strike: 100_000.0,
                },
                resolved_yes,
                ticks,
                date: "2026-05-01".into(),
            }
        };
        let m_yes = mk(true);
        let m_no = mk(false);
        let cfg = HarnessConfig {
            edge_threshold: -1.0,
            taker_fee_bps: 100.0,
            latency_ms: 0,
            ..HarnessConfig::default()
        };
        let report = run_set([(&m_yes, &spot), (&m_no, &spot)], &AlphaModel::default(), &cfg);
        assert_eq!(report.aggregate.n_trades, 2);
        assert!(
            report.aggregate.total_pnl < 0.0,
            "zero-edge + fees must be net negative, got {}",
            report.aggregate.total_pnl
        );
        assert!(report.aggregate.total_fees > 0.0);
    }

    #[test]
    fn identical_inputs_produce_identical_reports() {
        let (series, spot) = dislocation_market(2053);
        let items = [(&series, &spot)];
        let cfg = HarnessConfig::default();
        let model = AlphaModel::default();
        let a = serde_json::to_string(&run_set(items.iter().copied(), &model, &cfg)).unwrap();
        let b = serde_json::to_string(&run_set(items.iter().copied(), &model, &cfg)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn log_loss_checkpoints_score_exo_vs_book_at_same_instants() {
        let (series, spot) = dislocation_market(2053);
        let out = run_market(&series, &spot, &AlphaModel::default(), &HarnessConfig::default());
        assert!(!out.samples.is_empty());
        // After the jump the exogenous belief is sharp and right while the
        // (post-reprice) book is also right — but at the 60s checkpoint the
        // book has repriced too, so just assert structural sanity here.
        for s in &out.samples {
            assert!(s.p_exo > 0.0 && s.p_exo < 1.0);
            assert!(s.p_book > 0.0 && s.p_book < 1.0);
            assert!(s.resolved_yes);
        }
    }

    #[test]
    fn exit_rule_caps_losses_at_spread_not_binary() {
        // Book never reprices (0.50/0.52), belief is wrong-confident after a
        // spot jump the book ignores, market resolves AGAINST the position.
        let spot = spot_with_jump(2400, 2050);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let ticks = (open_s..close_s).map(|s| book_tick(s, 0.50, 0.52)).collect();
        let series = MarketSeries {
            meta: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: open_s * 1_000_000_000,
                close_ts_ns: close_s * 1_000_000_000,
                strike: 100_000.0,
            },
            resolved_yes: false, // position will be YES and lose at resolution
            ticks,
            date: "2026-05-01".into(),
        };
        let hold = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                ..HarnessConfig::default()
            },
        );
        let exit = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                exit_after_s: 30,
                ..HarnessConfig::default()
            },
        );
        let hold_pnl = hold.trades[0].pnl;
        let exit_pnl = exit.trades[0].pnl;
        // Hold loses the full stake; exit loses only the spread.
        assert!(hold_pnl < -40.0, "hold should lose ~full stake: {hold_pnl}");
        assert!(
            exit_pnl > -5.0 && exit_pnl < 0.0,
            "exit should lose ~the spread: {exit_pnl}"
        );
        assert!(exit.trades[0].exit_price.is_some());
    }

    #[test]
    fn skip_calm_blocks_entries_on_flat_tape() {
        // Genuinely quiet tape (sub-bp wiggle) => calm_low_vol regime;
        // threshold -1 would otherwise always enter.
        let spot = SpotHistory::new(
            (0..2400)
                .map(|s| {
                    let wave = if s % 2 == 0 { 1.000_000_5 } else { 0.999_999_5 };
                    tick_spot(s, 100_000.0 * wave)
                })
                .collect(),
        );
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let ticks = (open_s..close_s).map(|s| book_tick(s, 0.50, 0.52)).collect();
        let series = MarketSeries {
            meta: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: open_s * 1_000_000_000,
                close_ts_ns: close_s * 1_000_000_000,
                strike: 100_000.0,
            },
            resolved_yes: true,
            ticks,
            date: "2026-05-01".into(),
        };
        let blocked = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                edge_threshold: -1.0,
                skip_calm: true,
                latency_ms: 0,
                ..HarnessConfig::default()
            },
        );
        let open = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                edge_threshold: -1.0,
                skip_calm: false,
                latency_ms: 0,
                ..HarnessConfig::default()
            },
        );
        assert!(blocked.trades.is_empty());
        assert!(!open.trades.is_empty());
    }

    #[test]
    fn aligned_mode_requires_book_agreement() {
        // Spot jumped +1% (belief very bullish) but the book still prices
        // YES at 0.50/0.52: a fade fires; an aligned entry must NOT.
        let (series, spot) = dislocation_market(i64::MAX); // book never reprices
        let fade = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                entry_mode: EntryMode::Fade,
                ..HarnessConfig::default()
            },
        );
        let aligned = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                entry_mode: EntryMode::Aligned,
                align_min_mid: 0.55,
                ..HarnessConfig::default()
            },
        );
        assert!(!fade.trades.is_empty());
        assert!(aligned.trades.is_empty());

        // Once the book agrees (reprices to 0.93/0.95), aligned fires when
        // the belief still clears the ask.
        let (series2, spot2) = dislocation_market(2001); // repriced immediately
        let aligned2 = run_market(
            &series2,
            &spot2,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                entry_mode: EntryMode::Aligned,
                align_min_mid: 0.55,
                edge_threshold: 0.01,
                ..HarnessConfig::default()
            },
        );
        assert!(!aligned2.trades.is_empty());
        assert_eq!(aligned2.trades[0].side, Side::Yes);
    }

    #[test]
    fn tail_hedge_buys_the_cheap_opposite_side() {
        // Aligned YES entry at 0.95 ask; NO tail available at 0.04 via the
        // real NO ladder => hedge leg should fill.
        let spot = spot_with_jump(2400, 2050);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let mut ticks: Vec<BookTick> = Vec::new();
        for s in open_s..close_s {
            let mut t = book_tick(s, 0.93, 0.95);
            t.no_bid = 0.03;
            t.no_ask = 0.04;
            t.no_bids = book_levels(0.03);
            t.no_asks = book_levels(0.04);
            ticks.push(t);
        }
        let series = MarketSeries {
            meta: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: open_s * 1_000_000_000,
                close_ts_ns: close_s * 1_000_000_000,
                strike: 100_000.0,
            },
            resolved_yes: true,
            ticks,
            date: "2026-05-01".into(),
        };
        let out = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                entry_mode: EntryMode::Aligned,
                align_min_mid: 0.55,
                edge_threshold: 0.01,
                tail_max_price: 0.05,
                tail_frac: 0.25,
                ..HarnessConfig::default()
            },
        );
        assert_eq!(out.trades.len(), 2, "main + tail");
        assert_eq!(out.trades[1].side, Side::No);
        assert!((out.trades[1].avg_price - 0.04).abs() < 1e-6);
        assert!(!out.trades[1].won); // resolved YES, tail loses its premium
    }

    #[test]
    fn kelly_sizing_shrinks_lottery_entries() {
        // Same dislocation; flat vs kelly sizing. The fade entry here is a
        // confident convergence trade (p high, cost ~0.52), so kelly keeps
        // most of the clip; a synthetic cheap entry must shrink hard.
        let (series, spot) = dislocation_market(i64::MAX);
        let flat = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig { latency_ms: 0, ..HarnessConfig::default() },
        );
        let kelly = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                kelly_sizing: true,
                ..HarnessConfig::default()
            },
        );
        let flat_notional = flat.trades[0].avg_price * flat.trades[0].shares;
        let kelly_notional = kelly.trades[0].avg_price * kelly.trades[0].shares;
        assert!(flat_notional > 49.0, "flat fills the clip: {flat_notional}");
        assert!(
            kelly_notional > 15.0 && kelly_notional <= flat_notional,
            "confident trade keeps substantial size: {kelly_notional}"
        );

        // Cheap lottery: book at 0.10/0.12 with a mildly-bullish belief.
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let ticks = (open_s..close_s).map(|s| book_tick(s, 0.10, 0.12)).collect();
        let lottery = MarketSeries {
            meta: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: open_s * 1_000_000_000,
                close_ts_ns: close_s * 1_000_000_000,
                strike: 100_000.0,
            },
            resolved_yes: false,
            ticks,
            date: "2026-05-01".into(),
        };
        let spot2 = spot_with_jump(2400, i64::MAX);
        let kelly_lottery = run_market(
            &lottery,
            &spot2,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                edge_threshold: 0.16,
                kelly_sizing: true,
                ..HarnessConfig::default()
            },
        );
        for t in &kelly_lottery.trades {
            let notional = t.avg_price * t.shares;
            assert!(
                notional < 15.0,
                "lottery entries must size small: {notional}"
            );
        }
    }

    #[test]
    fn exit_at_mid_conditional_fill_requires_a_crossing_bid() {
        // Belief is wrong-confident (spot jump the book ignores), entry YES
        // at 0.52, exit horizon 30s, resting ask at the 0.51 mid.
        let spot = spot_with_jump(2400, 2050);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let meta = MarketMeta {
            token: Token::Btc,
            window_secs: 300,
            open_ts_ns: open_s * 1_000_000_000,
            close_ts_ns: close_s * 1_000_000_000,
            strike: 100_000.0,
        };
        let cfg = HarnessConfig {
            latency_ms: 0,
            exit_after_s: 30,
            exit_at_mid: true,
            ..HarnessConfig::default()
        };

        // Case 1: the bid never reaches the resting level -> no fill, the
        // position settles at resolution (full loss), while the optimistic
        // bound prices the exit at the mid.
        let never = MarketSeries {
            meta,
            resolved_yes: false,
            ticks: (open_s..close_s).map(|s| book_tick(s, 0.50, 0.52)).collect(),
            date: "2026-05-01".into(),
        };
        let out = run_market(&never, &spot, &AlphaModel::default(), &cfg);
        let t = &out.trades[0];
        assert!(t.exit_price.is_none(), "resting ask must not fill");
        assert!(t.pnl < -40.0, "unfilled exit settles at resolution: {}", t.pnl);
        let opt = t.pnl_exit_mid_optimistic.unwrap();
        assert!(
            opt > -5.0 && opt < 0.0,
            "optimistic bound loses ~half the spread: {opt}"
        );

        // Case 2: the book lifts to 0.60/0.62 after the horizon -> the bid
        // crosses 0.51 and the resting ask fills at OUR level (0.51).
        let crossed = MarketSeries {
            meta,
            resolved_yes: false,
            ticks: (open_s..close_s)
                .map(|s| {
                    if s < 2120 {
                        book_tick(s, 0.50, 0.52)
                    } else {
                        book_tick(s, 0.60, 0.62)
                    }
                })
                .collect(),
            date: "2026-05-01".into(),
        };
        let out = run_market(&crossed, &spot, &AlphaModel::default(), &cfg);
        let t = &out.trades[0];
        assert!((t.exit_price.unwrap() - 0.51).abs() < 1e-6, "fills at the resting level");
        assert!(
            t.pnl > -5.0 && t.pnl < 0.0,
            "filled passive exit loses ~half the spread: {}",
            t.pnl
        );
    }

    #[test]
    fn pair_completion_locks_profit_for_either_outcome() {
        // Entry YES at 0.52; the book then lifts so the synthetic NO ask
        // (1 - yes_bid) drops to 0.40 <= 1 - 0.52 - 0.02: completion fires,
        // matching leg 1's shares; both legs settle, locking ~0.08/share
        // regardless of which side resolves.
        let spot = spot_with_jump(2400, 2050);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let meta = MarketMeta {
            token: Token::Btc,
            window_secs: 300,
            open_ts_ns: open_s * 1_000_000_000,
            close_ts_ns: close_s * 1_000_000_000,
            strike: 100_000.0,
        };
        let mk = |resolved_yes: bool| MarketSeries {
            meta,
            resolved_yes,
            ticks: (open_s..close_s)
                .map(|s| {
                    if s < 2120 {
                        book_tick(s, 0.50, 0.52)
                    } else {
                        book_tick(s, 0.60, 0.62)
                    }
                })
                .collect(),
            date: "2026-05-01".into(),
        };
        let cfg = HarnessConfig {
            latency_ms: 0,
            pair_completion_margin: 0.02,
            ..HarnessConfig::default()
        };
        let mut totals = Vec::new();
        for resolved_yes in [true, false] {
            let out = run_market(&mk(resolved_yes), &spot, &AlphaModel::default(), &cfg);
            assert_eq!(out.trades.len(), 2, "leg 1 + completion");
            let (l1, l2) = (&out.trades[0], &out.trades[1]);
            assert!(!l1.is_completion);
            assert!(l2.is_completion);
            assert_eq!(l2.side, l1.side.opposite());
            assert!((l2.shares - l1.shares).abs() < 1e-6, "completion matches leg 1");
            assert!((l2.avg_price - 0.40).abs() < 1e-6, "synthetic NO ask = 1 - bid");
            assert!(l1.exit_price.is_none(), "completed leg 1 holds to resolution");
            totals.push(l1.pnl + l2.pnl);
        }
        let expected = (1.0 - 0.52 - 0.40) * (50.0 / 0.52);
        for total in &totals {
            assert!(
                (total - expected).abs() < 0.5,
                "locked profit ~{expected}: got {total}"
            );
        }
        assert!((totals[0] - totals[1]).abs() < 1e-6, "outcome-invariant lock");
    }

    #[test]
    fn pair_completion_respects_leg1_exit_window() {
        // Same setup but the book only lifts AFTER leg 1's 30s exit horizon:
        // no completion may fire; leg 1 exits normally.
        let spot = spot_with_jump(2400, 2050);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let series = MarketSeries {
            meta: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: open_s * 1_000_000_000,
                close_ts_ns: close_s * 1_000_000_000,
                strike: 100_000.0,
            },
            resolved_yes: true,
            ticks: (open_s..close_s)
                .map(|s| {
                    if s < 2150 {
                        book_tick(s, 0.50, 0.52)
                    } else {
                        book_tick(s, 0.60, 0.62)
                    }
                })
                .collect(),
            date: "2026-05-01".into(),
        };
        let out = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                exit_after_s: 30,
                pair_completion_margin: 0.02,
                ..HarnessConfig::default()
            },
        );
        assert_eq!(out.trades.len(), 1, "no completion after the exit window");
        assert!(!out.trades[0].is_completion);
        assert!(out.trades[0].exit_price.is_some(), "leg 1 exits normally");
    }

    #[test]
    fn new_mechanics_default_off_leave_trades_identical() {
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let base = HarnessConfig { exit_after_s: 30, ..HarnessConfig::default() };
        let a = serde_json::to_string(&run_market(&series, &spot, &model, &base).trades).unwrap();
        let b = serde_json::to_string(
            &run_market(
                &series,
                &spot,
                &model,
                &HarnessConfig { exit_at_mid: false, pair_completion_margin: 0.0, ..base },
            )
            .trades,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn no_entries_inside_stop_window() {
        let (mut series, spot) = dislocation_market(2053);
        // Keep only ticks in the final 10 seconds.
        series.ticks.retain(|t| t.ts_ns >= 2_290 * 1_000_000_000);
        let out = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                stop_before_close_s: 10,
                edge_threshold: -1.0,
                ..HarnessConfig::default()
            },
        );
        assert!(out.trades.is_empty());
    }
}
