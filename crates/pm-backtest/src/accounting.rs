//! Per-run accounting: order/fill counters, the backtest report, mark-to-market,
//! and the report's `println!` presentation.

use chrono::{DateTime, Utc};
use pm_model::MetaTrainingSample;
use pm_risk::PortfolioSnapshot;
use serde::Serialize;

use crate::fills::Fill;

#[derive(Debug, Clone, Serialize, Default)]
pub struct StrategyCounters {
    pub orders_submitted: usize,
    pub orders_filled_taker: usize,
    pub orders_filled_maker: usize,
    pub orders_rejected_no_cash: usize,
    pub orders_rejected_no_liquidity: usize,
    pub orders_rejected_bad_price: usize,
    pub orders_rejected_no_inventory: usize,
    pub orders_rejected_risk_gate: usize,
    pub orders_rejected_model_gate: usize,
    pub orders_rejected_model_gate_confidence: usize,
    pub orders_rejected_model_gate_risk: usize,
    pub orders_rejected_model_gate_edge: usize,
    pub resting_orders_active: usize,
    pub resting_orders_cancelled_eom: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BacktestReport {
    pub events_processed: usize,
    pub counters: StrategyCounters,
    pub start_equity_usdc: f64,
    pub end_equity_usdc: f64,
    pub pnl_usdc: f64,
    pub maker_rebates_usdc: f64,
    pub peak_equity_usdc: f64,
    pub max_drawdown_pct: f64,
    pub final_yes_shares: f64,
    pub final_no_shares: f64,
    pub final_cash_usdc: f64,
    pub requested_shares: f64,
    pub filled_shares: f64,
    pub requested_notional_usdc: f64,
    pub filled_notional_usdc: f64,
    pub yes_resolved: bool,
    pub last_yes_mid: f32,
    pub fills: Vec<Fill>,
    pub final_portfolio: PortfolioSnapshot,
    pub model_training_samples: Vec<MetaTrainingSample>,
}

pub(crate) fn mark_to_market(cash: f64, yes_shares: f64, no_shares: f64, yes_mid: f32) -> f64 {
    let p = yes_mid.clamp(0.0, 1.0) as f64;
    cash + yes_shares * p + no_shares * (1.0 - p)
}

pub fn pretty_print(rep: &BacktestReport) {
    println!("== backtest report ==");
    println!("events_processed  : {}", rep.events_processed);
    println!(
        "orders            : submitted={}  filled[taker={} maker={}]  rejected[cash={} liq={} px={} inv={} risk={} model={} model_reason[conf={} risk={} edge={}]]  resting_active={}  resting_cancelled_eom={}",
        rep.counters.orders_submitted,
        rep.counters.orders_filled_taker,
        rep.counters.orders_filled_maker,
        rep.counters.orders_rejected_no_cash,
        rep.counters.orders_rejected_no_liquidity,
        rep.counters.orders_rejected_bad_price,
        rep.counters.orders_rejected_no_inventory,
        rep.counters.orders_rejected_risk_gate,
        rep.counters.orders_rejected_model_gate,
        rep.counters.orders_rejected_model_gate_confidence,
        rep.counters.orders_rejected_model_gate_risk,
        rep.counters.orders_rejected_model_gate_edge,
        rep.counters.resting_orders_active,
        rep.counters.resting_orders_cancelled_eom,
    );
    println!(
        "equity            : {:>10.4} -> {:>10.4} USDC  (pnl {:>+.4}; rebates {:>+.4})",
        rep.start_equity_usdc, rep.end_equity_usdc, rep.pnl_usdc, rep.maker_rebates_usdc
    );
    println!(
        "peak / max_dd     : {:>10.4} USDC   {:.2}%",
        rep.peak_equity_usdc,
        rep.max_drawdown_pct * 100.0
    );
    let fill_notional_ratio = if rep.requested_notional_usdc > 0.0 {
        rep.filled_notional_usdc / rep.requested_notional_usdc
    } else {
        0.0
    };
    let fill_shares_ratio = if rep.requested_shares > 0.0 {
        rep.filled_shares / rep.requested_shares
    } else {
        0.0
    };
    println!(
        "fill quality      : shares={:.1}% notional={:.1}% requested={:.4} filled={:.4}",
        fill_shares_ratio * 100.0,
        fill_notional_ratio * 100.0,
        rep.requested_notional_usdc,
        rep.filled_notional_usdc
    );
    println!(
        "final position    : yes={:.4}  no={:.4}  cash={:.4}",
        rep.final_yes_shares, rep.final_no_shares, rep.final_cash_usdc
    );
    println!(
        "resolution        : yes_resolved={}  last_mid={:.4}",
        rep.yes_resolved, rep.last_yes_mid
    );
    if let Some(reason) = rep.final_portfolio.halt_reason.as_deref() {
        println!("HALTED            : {reason}");
    }
    if rep.fills.is_empty() {
        return;
    }
    println!("\nfills (showing first 20):");
    for f in rep.fills.iter().take(20) {
        let dt = DateTime::<Utc>::from_timestamp_nanos(f.ts_ns);
        println!(
            "  {} {:>7} {:>22}  shares={:>8.4} price={:.4} notional={:.4}  {}",
            dt.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            f.side,
            f.tag,
            f.shares,
            f.price,
            f.notional,
            if f.maker { "MAKER" } else { "TAKER" }
        );
    }
    if rep.fills.len() > 20 {
        println!("  ... and {} more", rep.fills.len() - 20);
    }
}
