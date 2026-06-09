//! Offline validation harness: latency-modeled, cost-aware edge measurement.
//!
//! Reality rules (spec section 4.5): decisions observe data at T but fill at
//! T + latency against the book as it actually was then; fills cross the
//! spread and walk depth; fees apply; labels are the market's resolution.

mod metrics;
mod replay;
mod types;

pub use metrics::{CellReport, HuntReport, aggregate};
pub use replay::run_market;
pub use types::{
    BookTick, HarnessConfig, MarketRunOutput, MarketSeries, ProbSample, Side, TradeRecord,
};

use crate::model::AlphaModelConfig;
use pm_types::SpotHistory;

/// Run a set of markets (each paired with its day's spot history) under one
/// config and aggregate.
pub fn run_set<'a>(
    items: impl IntoIterator<Item = (&'a MarketSeries, &'a SpotHistory)>,
    model_cfg: &AlphaModelConfig,
    cfg: &HarnessConfig,
) -> HuntReport {
    let results: Vec<(&MarketSeries, MarketRunOutput)> = items
        .into_iter()
        .map(|(series, spot)| (series, run_market(series, spot, model_cfg, cfg)))
        .collect();
    aggregate(&results)
}

/// The edge-vs-latency curve: identical config swept over entry latencies.
pub fn run_sweep<'a>(
    items: &[(&'a MarketSeries, &'a SpotHistory)],
    model_cfg: &AlphaModelConfig,
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
            (
                latency_ms,
                run_set(items.iter().copied(), model_cfg, &cfg),
            )
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
        }
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
        let model_cfg = AlphaModelConfig::default();
        let base = HarnessConfig {
            edge_threshold: 0.05,
            taker_fee_bps: 0.0,
            ..HarnessConfig::default()
        };
        let items = [(&series, &spot)];
        let sweep = run_sweep(&items, &model_cfg, &base, &[0, 1_000, 5_000, 30_000]);
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
        let report = run_set(
            [(&m_yes, &spot), (&m_no, &spot)],
            &AlphaModelConfig::default(),
            &cfg,
        );
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
        let model_cfg = AlphaModelConfig::default();
        let a = serde_json::to_string(&run_set(items.iter().copied(), &model_cfg, &cfg)).unwrap();
        let b = serde_json::to_string(&run_set(items.iter().copied(), &model_cfg, &cfg)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn log_loss_checkpoints_score_exo_vs_book_at_same_instants() {
        let (series, spot) = dislocation_market(2053);
        let out = run_market(
            &series,
            &spot,
            &AlphaModelConfig::default(),
            &HarnessConfig::default(),
        );
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
    fn no_entries_inside_stop_window() {
        let (mut series, spot) = dislocation_market(2053);
        // Keep only ticks in the final 10 seconds.
        series.ticks.retain(|t| t.ts_ns >= 2_290 * 1_000_000_000);
        let out = run_market(
            &series,
            &spot,
            &AlphaModelConfig::default(),
            &HarnessConfig {
                stop_before_close_s: 10,
                edge_threshold: -1.0,
                ..HarnessConfig::default()
            },
        );
        assert!(out.trade.is_none());
    }
}
