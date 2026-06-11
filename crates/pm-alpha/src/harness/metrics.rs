//! Aggregation: per token x window cell and aggregate, plus the latency sweep.

use super::types::MarketRunOutput;
use crate::state::Token;
use std::collections::BTreeMap;

fn clamp_p(p: f64) -> f64 {
    p.clamp(0.01, 0.99)
}

fn log_loss_term(p_yes: f64, resolved_yes: bool) -> f64 {
    let p = clamp_p(p_yes);
    if resolved_yes { -p.ln() } else { -(1.0 - p).ln() }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct CellReport {
    pub n_markets: usize,
    pub n_with_belief: usize,
    /// Markets with at least one clip filled.
    pub n_markets_traded: usize,
    pub n_trades: usize,
    pub n_wins: usize,
    pub total_pnl: f64,
    pub mean_pnl_per_trade: f64,
    pub hit_rate: f64,
    pub total_fees: f64,
    /// Mean log-loss of the exogenous belief at the fixed checkpoints.
    pub log_loss_exo: f64,
    /// Mean log-loss of the book mid at the same instants.
    pub log_loss_book: f64,
    pub n_samples: usize,
}

impl CellReport {
    fn finalize(&mut self, sum_ll_exo: f64, sum_ll_book: f64) {
        if self.n_trades > 0 {
            self.mean_pnl_per_trade = self.total_pnl / self.n_trades as f64;
            self.hit_rate = self.n_wins as f64 / self.n_trades as f64;
        }
        if self.n_samples > 0 {
            self.log_loss_exo = sum_ll_exo / self.n_samples as f64;
            self.log_loss_book = sum_ll_book / self.n_samples as f64;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct CellKey {
    pub token: Token,
    pub window_secs: u32,
}

impl Ord for Token {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}
impl PartialOrd for Token {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct HuntReport {
    pub cells: BTreeMap<String, CellReport>,
    /// Cells additionally split by exogenous regime at window open.
    pub regime_cells: BTreeMap<String, CellReport>,
    pub aggregate: CellReport,
}

#[derive(Debug, Default)]
struct Accum {
    report: CellReport,
    sum_ll_exo: f64,
    sum_ll_book: f64,
}

impl Accum {
    fn add(&mut self, out: &MarketRunOutput) {
        self.report.n_markets += 1;
        if out.had_belief {
            self.report.n_with_belief += 1;
        }
        if !out.trades.is_empty() {
            self.report.n_markets_traded += 1;
        }
        for t in &out.trades {
            self.report.n_trades += 1;
            if t.won {
                self.report.n_wins += 1;
            }
            self.report.total_pnl += t.pnl;
            self.report.total_fees += t.fee;
        }
        for s in &out.samples {
            self.report.n_samples += 1;
            self.sum_ll_exo += log_loss_term(s.p_exo, s.resolved_yes);
            self.sum_ll_book += log_loss_term(s.p_book, s.resolved_yes);
        }
    }

    fn finalize(mut self) -> CellReport {
        self.report.finalize(self.sum_ll_exo, self.sum_ll_book);
        self.report
    }
}

/// Aggregate per-market outputs into per-cell and aggregate reports. Takes
/// `MarketMeta` rather than the full series so callers can stream markets
/// and drop tapes as they go.
pub fn aggregate<'a>(
    results: impl IntoIterator<Item = (&'a crate::state::MarketMeta, &'a MarketRunOutput)>,
) -> HuntReport {
    let mut cells: BTreeMap<CellKey, Accum> = BTreeMap::new();
    let mut regime_cells: BTreeMap<String, Accum> = BTreeMap::new();
    let mut agg = Accum::default();
    for (meta, out) in results {
        let key = CellKey {
            token: meta.token,
            window_secs: meta.window_secs,
        };
        cells.entry(key).or_default().add(out);
        let regime = out.regime.map(|r| r.as_str()).unwrap_or("unknown");
        regime_cells
            .entry(format!("{}-{}s|{}", meta.token.as_str(), meta.window_secs, regime))
            .or_default()
            .add(out);
        agg.add(out);
    }
    HuntReport {
        cells: cells
            .into_iter()
            .map(|(k, v)| {
                (
                    format!("{}-{}s", k.token.as_str(), k.window_secs),
                    v.finalize(),
                )
            })
            .collect(),
        regime_cells: regime_cells
            .into_iter()
            .map(|(k, v)| (k, v.finalize()))
            .collect(),
        aggregate: agg.finalize(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::types::{ProbSample, Side, TradeRecord};
    use crate::state::{MarketMeta, Token};

    fn series(token: Token) -> MarketMeta {
        MarketMeta {
            token,
            window_secs: 300,
            open_ts_ns: 0,
            close_ts_ns: 300_000_000_000,
            strike: 100.0,
        }
    }

    fn output(pnl: f64, won: bool, p_exo: f64, p_book: f64) -> MarketRunOutput {
        MarketRunOutput {
            trades: vec![TradeRecord {
                side: Side::Yes,
                decision_ts_ns: 0,
                fill_ts_ns: 0,
                avg_price: 0.5,
                shares: 10.0,
                fee: 0.0,
                p_exo,
                mid_at_decision: p_book,
                pnl,
                won,
                exit_price: None,
                mark_60s: None,
                pnl_exit_mid_optimistic: None,
                is_completion: false,
            }],
            samples: vec![ProbSample {
                ts_ns: 60_000_000_000,
                p_exo,
                p_book,
                resolved_yes: true,
            }],
            had_belief: true,
            train_samples: Vec::new(),
            dir_samples: Vec::new(),
            regime: None,
            min_pair_cost: None,
            real_no_coverage: 0.0,
        }
    }

    #[test]
    fn sharp_exogenous_p_beats_book_mid_on_log_loss() {
        let s = series(Token::Btc);
        let results = vec![(s, output(5.0, true, 0.95, 0.5))];
        let r = aggregate(results.iter().map(|(m, o)| (m, o)));
        assert!(r.aggregate.log_loss_exo < r.aggregate.log_loss_book);
    }

    #[test]
    fn cells_partition_by_token_and_window() {
        let a = series(Token::Btc);
        let b = series(Token::Eth);
        let results = vec![
            (a, output(1.0, true, 0.9, 0.5)),
            (b, output(-1.0, false, 0.9, 0.5)),
        ];
        let r = aggregate(results.iter().map(|(m, o)| (m, o)));
        assert_eq!(r.cells.len(), 2);
        assert_eq!(r.aggregate.n_trades, 2);
        assert_eq!(r.aggregate.n_wins, 1);
        assert!((r.aggregate.hit_rate - 0.5).abs() < 1e-12);
        assert!((r.aggregate.total_pnl - 0.0).abs() < 1e-12);
    }

    #[test]
    fn log_loss_clamps_extremes() {
        assert!(log_loss_term(1.0, false).is_finite());
        assert!(log_loss_term(0.0, true).is_finite());
    }
}
