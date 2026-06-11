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
    fn enter_within_close_zero_is_parity() {
        // 0 = disabled must produce trades identical to a window so wide the
        // gate never binds (and to the pre-feature default behaviour).
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let base = HarnessConfig::default();
        assert_eq!(base.enter_within_close_s, 0);
        let off = run_market(&series, &spot, &model, &base);
        let wide = run_market(
            &series,
            &spot,
            &model,
            &HarnessConfig { enter_within_close_s: 100_000, ..base },
        );
        assert!(!off.trades.is_empty());
        assert_eq!(
            serde_json::to_string(&off.trades).unwrap(),
            serde_json::to_string(&wide.trades).unwrap()
        );
    }

    #[test]
    fn enter_within_close_window_gates_entries() {
        // Threshold -1 forces an entry at the first eligible decision: with
        // the window off that is near open; with a 60s window every entry
        // lands in [close - 60s, close - stop_before_close_s].
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let base = HarnessConfig {
            edge_threshold: -1.0,
            latency_ms: 0,
            ..HarnessConfig::default()
        };
        let close_ns = series.meta.close_ts_ns;
        let window_ns = 60 * 1_000_000_000;
        let off = run_market(&series, &spot, &model, &base);
        assert!(!off.trades.is_empty());
        assert!(off.trades[0].decision_ts_ns < close_ns - window_ns);

        let gated = run_market(
            &series,
            &spot,
            &model,
            &HarnessConfig { enter_within_close_s: 60, ..base },
        );
        assert!(!gated.trades.is_empty());
        for t in &gated.trades {
            assert!(t.decision_ts_ns >= close_ns - window_ns);
            assert!(
                t.decision_ts_ns
                    < close_ns - base.stop_before_close_s as i64 * 1_000_000_000
            );
        }
    }

    #[test]
    fn entry_stability_zero_is_parity() {
        // 0 = disabled must produce trades identical to a gate so loose it
        // never binds (eps = 1.0 covers any ask move in (0, 1)).
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let base = HarnessConfig::default();
        assert_eq!(base.entry_stability_s, 0);
        let off = run_market(&series, &spot, &model, &base);
        let loose = run_market(
            &series,
            &spot,
            &model,
            &HarnessConfig { entry_stability_s: 40, stability_eps: 1.0, ..base },
        );
        assert!(!off.trades.is_empty());
        assert!(off.trades.iter().all(|t| t.stable_entry));
        assert_eq!(
            serde_json::to_string(&off.trades).unwrap(),
            serde_json::to_string(&loose.trades).unwrap()
        );
    }

    #[test]
    fn entry_stability_blocks_until_selldown_leaves_window() {
        // The gate blocks only prints BELOW (current ask - eps), i.e. a
        // dip-and-recover: the ask dips 0.90 -> 0.80 during [2060, 2070),
        // then recovers. Entries open at t=2080 (post-recovery): decisions
        // in [2080, 2110) still carry the 0.80 prints in their trailing 40s
        // and must be blocked; t=2110 is the first clean window.
        let spot = spot_with_jump(2400, 2050); // belief goes ~certain YES
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let mut ticks = Vec::new();
        for s in open_s..close_s {
            let ask = if (2060..2070).contains(&s) { 0.80 } else { 0.90 };
            ticks.push(book_tick(s, ask - 0.02, ask));
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
        let model = AlphaModel::default();
        let base = HarnessConfig {
            edge_threshold: 0.05,
            latency_ms: 0,
            enter_within_close_s: 220, // entries open at t=2080, post-recovery
            ..HarnessConfig::default()
        };
        let off = run_market(&series, &spot, &model, &base);
        assert!(!off.trades.is_empty());
        let first_off = off.trades[0].decision_ts_ns / 1_000_000_000;
        assert!(first_off < 2110, "ungated entry near the window open, got {first_off}");

        let gated = run_market(
            &series,
            &spot,
            &model,
            &HarnessConfig { entry_stability_s: 40, ..base },
        );
        assert!(!gated.trades.is_empty());
        let t = &gated.trades[0];
        // The 0.80 dip prints must have aged out of the trailing 40s.
        assert!(
            t.decision_ts_ns >= 2_110 * 1_000_000_000,
            "stability gate must delay entry past the dip+window, got {}",
            t.decision_ts_ns / 1_000_000_000
        );
        assert!(t.stable_entry);
        assert!(t.trail_min_ask_40s.unwrap() >= t.side_ask_at_decision - 0.005);
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
    fn hybrid_passive_exit_fills_at_mid_or_converts_at_the_timeout_book() {
        // Entry YES at 0.52 (fill ~t=2050), exit horizon 30s (t=2080),
        // resting ask at the 0.51 mid with a 10s timeout (t=2090).
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
            passive_exit_timeout_s: 10,
            ..HarnessConfig::default()
        };
        let mk = |ticks: Vec<BookTick>| MarketSeries {
            meta,
            resolved_yes: false,
            ticks,
            date: "2026-05-01".into(),
        };

        // Case 1: the book lifts to 0.60/0.62 at t=2085, inside the timeout
        // window -> the resting ask fills at OUR level (the 0.51 mid).
        let lifted = mk((open_s..close_s)
            .map(|s| if s < 2085 { book_tick(s, 0.50, 0.52) } else { book_tick(s, 0.60, 0.62) })
            .collect());
        let out = run_market(&lifted, &spot, &AlphaModel::default(), &cfg);
        let t = &out.trades[0];
        assert_eq!(t.exit_filled_at_mid, Some(true));
        assert!((t.exit_price.unwrap() - 0.51).abs() < 1e-6, "maker fill at the mid");

        // Case 2: the book DROPS to 0.40/0.42 at t=2085 -> never crosses
        // 0.51; at the timeout we convert and cross against the book as of
        // t=2090 (bid 0.40), NOT the original exit tick's 0.50 bid.
        let dropped = mk((open_s..close_s)
            .map(|s| if s < 2085 { book_tick(s, 0.50, 0.52) } else { book_tick(s, 0.40, 0.42) })
            .collect());
        let out = run_market(&dropped, &spot, &AlphaModel::default(), &cfg);
        let t = &out.trades[0];
        assert_eq!(t.exit_filled_at_mid, Some(false));
        assert!(
            (t.exit_price.unwrap() - 0.40).abs() < 1e-6,
            "conversion crosses the timeout-time book: {:?}",
            t.exit_price
        );
        let expected = (50.0 / 0.52) * (0.40 - 0.52);
        assert!((t.pnl - expected).abs() < 0.1, "pnl ~{expected}: {}", t.pnl);

        // Case 3: flat book to the end -> conversion sells at the unchanged
        // 0.50 bid, matching the champion crossing exit exactly.
        let flat = mk((open_s..close_s).map(|s| book_tick(s, 0.50, 0.52)).collect());
        let hybrid = run_market(&flat, &spot, &AlphaModel::default(), &cfg);
        let champion = run_market(
            &flat,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig { passive_exit_timeout_s: 0, ..cfg },
        );
        assert_eq!(hybrid.trades[0].exit_filled_at_mid, Some(false));
        assert!((hybrid.trades[0].pnl - champion.trades[0].pnl).abs() < 1e-9);
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
                &HarnessConfig {
                    exit_at_mid: false,
                    passive_exit_timeout_s: 0,
                    pair_completion_margin: 0.0,
                    rearm_edge: 0.0,
                    ..base
                },
            )
            .trades,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    /// Persistent dislocation market (book stays stale 0.50/0.52 the whole
    /// window after a +1% spot jump at 2050s) for re-entry mechanics tests.
    fn persistent_dislocation() -> (MarketSeries, SpotHistory) {
        dislocation_market(2300) // never reprices before close
    }

    #[test]
    fn rearm_zero_keeps_cooldown_ladder_and_trades_identical() {
        // rearm_edge = 0 must leave multi-clip laddering exactly as before:
        // entries spaced by the cooldown into the same persisting
        // dislocation, and byte-identical trades vs the unset config.
        let (series, spot) = persistent_dislocation();
        let model = AlphaModel::default();
        let base = HarnessConfig {
            latency_ms: 0,
            edge_threshold: 0.16,
            max_clips: 3,
            clip_cooldown_ms: 20_000,
            ..HarnessConfig::default()
        };
        let out = run_market(&series, &spot, &model, &base);
        assert_eq!(out.trades.len(), 3, "cooldown ladder fills all clips");
        for w in out.trades.windows(2) {
            assert_eq!(
                w[1].decision_ts_ns - w[0].decision_ts_ns,
                20_000_000_000,
                "clips spaced by exactly the cooldown"
            );
        }
        let a = serde_json::to_string(&out.trades).unwrap();
        let b = serde_json::to_string(
            &run_market(&series, &spot, &model, &HarnessConfig { rearm_edge: 0.0, ..base })
                .trades,
        )
        .unwrap();
        assert_eq!(a, b, "rearm_edge=0 is byte-identical");
    }

    #[test]
    fn rearm_blocks_reentry_while_dislocation_persists() {
        // The quote that survives is adversely selected: with a re-arm level
        // set, a dislocation that never closes yields exactly one entry no
        // matter how many clips or how short the cooldown.
        let (series, spot) = persistent_dislocation();
        let out = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig {
                latency_ms: 0,
                edge_threshold: 0.16,
                max_clips: 3,
                clip_cooldown_ms: 1_000,
                rearm_edge: 0.04,
                ..HarnessConfig::default()
            },
        );
        assert_eq!(out.trades.len(), 1, "no re-entry into a persisting dislocation");
    }

    #[test]
    fn rearm_allows_reentry_after_dislocation_closes_and_reopens() {
        // Book: stale 0.50/0.52 until 2100s (dislocation open), repriced to
        // 0.97/0.99 until 2150s (closed: both edges collapse below the
        // re-arm level), then stale again (a fresh staleness event).
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
                    if (2100..2150).contains(&s) {
                        book_tick(s, 0.97, 0.99)
                    } else {
                        book_tick(s, 0.50, 0.52)
                    }
                })
                .collect(),
            date: "2026-05-01".into(),
        };
        let cfg = HarnessConfig {
            latency_ms: 0,
            edge_threshold: 0.16,
            max_clips: 2,
            clip_cooldown_ms: 1_000,
            rearm_edge: 0.04,
            ..HarnessConfig::default()
        };
        let out = run_market(&series, &spot, &AlphaModel::default(), &cfg);
        assert_eq!(out.trades.len(), 2, "re-opened dislocation is enterable");
        assert!(
            out.trades[0].decision_ts_ns < 2_100 * 1_000_000_000,
            "first entry hits the original dislocation"
        );
        assert!(
            out.trades[1].decision_ts_ns >= 2_150 * 1_000_000_000,
            "re-entry only after close-and-reopen, got {}",
            out.trades[1].decision_ts_ns
        );
        // Without the gate the second clip piles into the same dislocation
        // one cooldown later.
        let naive = run_market(
            &series,
            &spot,
            &AlphaModel::default(),
            &HarnessConfig { rearm_edge: 0.0, ..cfg },
        );
        assert_eq!(naive.trades.len(), 2);
        assert!(
            naive.trades[1].decision_ts_ns < 2_100 * 1_000_000_000,
            "naive cooldown re-entry stays in the same event"
        );
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

    #[test]
    fn fee_curve_math_at_half_is_175_cents_per_hundred_shares() {
        // Polymarket crypto takers: 0.07 * p * (1-p) per share. At p = 0.5
        // that is 1.75c/share; at the extremes it vanishes; at rate 0 it is
        // exactly zero (the parity guarantee).
        assert!((super::replay::curve_fee(0.07, 0.5, 1.0) - 0.0175).abs() < 1e-15);
        assert!((super::replay::curve_fee(0.07, 0.5, 100.0) - 1.75).abs() < 1e-12);
        assert_eq!(super::replay::curve_fee(0.0, 0.52, 96.0), 0.0);
        assert!(super::replay::curve_fee(0.07, 0.99, 1.0) < 0.001);
    }

    #[test]
    fn fee_features_default_off_and_byte_identical() {
        // Defaults must be off, and a run with both features explicitly
        // disabled must produce byte-identical trades to the default config.
        let base = HarnessConfig::default();
        assert_eq!(base.fee_curve_rate, 0.0);
        assert!(!base.fee_aware_exit);
        assert_eq!(base.fee_exit_margin, 0.0);
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let cfg = HarnessConfig { exit_after_s: 30, latency_ms: 0, ..base };
        let a = serde_json::to_string(&run_market(&series, &spot, &model, &cfg).trades).unwrap();
        let b = serde_json::to_string(
            &run_market(
                &series,
                &spot,
                &model,
                &HarnessConfig {
                    fee_curve_rate: 0.0,
                    fee_aware_exit: false,
                    fee_exit_margin: 0.0,
                    ..cfg
                },
            )
            .trades,
        )
        .unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn fee_curve_charges_entry_and_exit_legs_at_their_own_prices() {
        // Entry fills at the stale 0.52 ask, exit crosses at the 0.93 bid
        // 30s later: the curve fee must be rate*p*(1-p)*shares at EACH leg's
        // own price, and pnl must drop by exactly the total fee vs rate 0.
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let cfg = HarnessConfig {
            exit_after_s: 30,
            latency_ms: 0,
            ..HarnessConfig::default()
        };
        let gross = run_market(&series, &spot, &model, &cfg);
        let net = run_market(
            &series,
            &spot,
            &model,
            &HarnessConfig { fee_curve_rate: 0.07, ..cfg },
        );
        assert_eq!(gross.trades.len(), 1);
        assert_eq!(net.trades.len(), 1);
        let (g, n) = (&gross.trades[0], &net.trades[0]);
        assert_eq!(g.avg_price, n.avg_price);
        assert_eq!(g.shares, n.shares);
        assert_eq!(g.exit_price, n.exit_price);
        let px_exit = n.exit_price.unwrap();
        let expect_fee = super::replay::curve_fee(0.07, n.avg_price, n.shares)
            + super::replay::curve_fee(0.07, px_exit, n.shares);
        assert!((n.fee - expect_fee).abs() < 1e-9, "fee {} != {expect_fee}", n.fee);
        assert!((g.pnl - n.pnl - expect_fee).abs() < 1e-9);
        assert_eq!(g.fee, 0.0);
    }

    /// Spot jumps +1% at 2050s and reverts at `revert_at_s`; wavy otherwise.
    fn spot_jump_revert(secs: i64, jump_at_s: i64, revert_at_s: i64) -> SpotHistory {
        let mut ticks = Vec::new();
        let base = 100_000.0;
        for s in 0..secs {
            let wave = if s % 2 == 0 { 1.00005 } else { 0.99995 };
            let level = if s >= jump_at_s && s < revert_at_s { base * 1.01 } else { base };
            ticks.push(tick_spot(s, level * wave));
        }
        SpotHistory::new(ticks)
    }

    #[test]
    fn fee_aware_exit_holds_when_belief_beats_net_proceeds() {
        // Sustained dislocation, resolves YES: at the exit instant the belief
        // is ~1.0 while the bid nets 0.93 minus the exit fee, so the rule
        // must skip the sell and settle at resolution paying entry fee only.
        let (series, spot) = dislocation_market(2053);
        let model = AlphaModel::default();
        let cfg = HarnessConfig {
            exit_after_s: 30,
            latency_ms: 0,
            fee_curve_rate: 0.07,
            fee_aware_exit: true,
            ..HarnessConfig::default()
        };
        let out = run_market(&series, &spot, &model, &cfg);
        assert_eq!(out.trades.len(), 1);
        let t = &out.trades[0];
        assert!(t.fee_hold, "rule must convert the exit into a hold");
        assert!(t.exit_price.is_none());
        assert!(t.won, "held to resolution and resolved with the side");
        let entry_fee = super::replay::curve_fee(0.07, t.avg_price, t.shares);
        assert!((t.fee - entry_fee).abs() < 1e-9, "holds pay the entry fee only");
        assert!((t.pnl - (t.shares * (1.0 - t.avg_price) - entry_fee)).abs() < 1e-9);
        let alt = t.hold_alt_sell_pnl.unwrap();
        assert!(t.pnl > alt, "here holding realizes more than the sell would have");
        assert!(t.hold_alt_exit_fee.unwrap() > 0.0);
    }

    #[test]
    fn fee_aware_exit_sells_when_belief_dropped_below_net_proceeds() {
        // Spot reverts before the exit horizon: the belief collapses to ~0.5
        // while the book still bids 0.93, so net proceeds beat the hold EV
        // and the rule must take the champion crossing exit.
        let spot = spot_jump_revert(2400, 2050, 2065);
        let open_s = 2000_i64;
        let close_s = 2300_i64;
        let ticks = (open_s..close_s)
            .map(|s| {
                if s < 2053 { book_tick(s, 0.50, 0.52) } else { book_tick(s, 0.93, 0.95) }
            })
            .collect();
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
        let model = AlphaModel::default();
        let base = HarnessConfig {
            exit_after_s: 30,
            latency_ms: 0,
            fee_curve_rate: 0.07,
            fee_aware_exit: true,
            ..HarnessConfig::default()
        };
        let out = run_market(&series, &spot, &model, &base);
        assert_eq!(out.trades.len(), 1);
        let t = &out.trades[0];
        assert!(!t.fee_hold);
        assert!((t.exit_price.unwrap() - 0.93).abs() < 1e-6, "sold at the bid");
        // The variance premium makes holds rarer in reverse: a margin larger
        // than the remaining edge must flip the same exit into a hold.
        let strict = run_market(
            &series,
            &spot,
            &model,
            &HarnessConfig { fee_exit_margin: 0.5, ..base },
        );
        assert!(strict.trades[0].fee_hold, "margin flips the sell into a hold");
        assert!(strict.trades[0].exit_price.is_none());
    }
}
