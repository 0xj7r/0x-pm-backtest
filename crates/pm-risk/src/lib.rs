//! Risk + portfolio.
//!
//! Two layers:
//!   * **Sizing**: `fractional_kelly_stake` — turns a calibrated edge into a
//!     dollar stake, bounded by a hard `max_stake` and a Kelly fraction.
//!   * **Portfolio state**: `PortfolioState` — running equity, peak, drawdown,
//!     daily exposure, and circuit-breaker gates that the runner can poll
//!     before letting orders through.

#![forbid(unsafe_code)]

use serde::Serialize;

/// Fractional Kelly position size on a single binary bet.
///
/// `payoff = (1 - price) / price`; `edge = calibrated_p - implied_p`. Returns
/// dollars to stake; never larger than `max_stake`, never negative.
pub fn fractional_kelly_stake(
    calibrated_p: f64,
    implied_p: f64,
    bankroll: f64,
    fraction: f64,
    max_stake: f64,
) -> f64 {
    if calibrated_p <= implied_p || bankroll <= 0.0 || fraction <= 0.0 {
        return 0.0;
    }
    let b = (1.0 - implied_p) / implied_p;
    let f_star = (b * calibrated_p - (1.0 - calibrated_p)) / b;
    let stake = (f_star.max(0.0) * fraction * bankroll).min(max_stake);
    stake.max(0.0)
}

#[derive(Debug, Clone)]
pub struct PortfolioLimits {
    /// Halt new buys when equity drops this fraction below peak (e.g. 0.20).
    pub max_drawdown_pct: f64,
    /// Cap aggregate net dollars at risk in any rolling 24h window.
    /// Repair buys that reduce inventory imbalance are exempt (see can_open..._ex).
    pub max_daily_exposure_usdc: f64,
    /// Maximum gross outlay per individual order.
    pub max_clip_usdc: f64,
    /// Maximum cumulative outlay (long-side gross dollars) on a single
    /// market over its lifetime. Critical to prevent a single wrong-side
    /// signal from reloading repeatedly into a losing position.
    ///
    /// Note: repair/pair buys (those that reduce |yes_shares - no_shares|)
    /// are exempt from this and the daily cap (see can_open_position_ex).
    pub max_per_market_exposure_usdc: f64,
}

impl Default for PortfolioLimits {
    fn default() -> Self {
        Self {
            max_drawdown_pct: 0.30,
            max_daily_exposure_usdc: 250.0,
            max_clip_usdc: 5.0,
            max_per_market_exposure_usdc: 50.0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PortfolioSnapshot {
    pub ts_ns: i64,
    pub equity_usdc: f64,
    pub peak_equity_usdc: f64,
    pub drawdown_pct: f64,
    pub daily_exposure_usdc: f64,
    pub halted: bool,
    pub halt_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PortfolioState {
    limits: PortfolioLimits,
    starting_equity: f64,
    peak_equity: f64,
    rolling_window: Vec<(i64, f64)>,
    per_market_outlay: std::collections::HashMap<u32, f64>,
    halt_reason: Option<&'static str>,
}

impl PortfolioState {
    pub fn new(starting_equity: f64, limits: PortfolioLimits) -> Self {
        Self {
            limits,
            per_market_outlay: std::collections::HashMap::new(),
            starting_equity,
            peak_equity: starting_equity,
            rolling_window: Vec::new(),
            halt_reason: None,
        }
    }

    /// Update peak/drawdown bookkeeping. Should be called every tick the
    /// equity value changes (and at minimum once per fill).
    pub fn mark(&mut self, equity_usdc: f64) {
        if equity_usdc > self.peak_equity {
            self.peak_equity = equity_usdc;
        }
        let dd = if self.peak_equity > 0.0 {
            1.0 - equity_usdc / self.peak_equity
        } else {
            0.0
        };
        if dd >= self.limits.max_drawdown_pct {
            self.halt_reason.get_or_insert("max_drawdown");
        }
    }

    /// Record a new outlay (gross dollars deployed by a buy) on a specific
    /// market. Called after a fill clears.
    pub fn record_outlay(&mut self, market_id: u32, ts_ns: i64, outlay_usdc: f64) {
        self.rolling_window.push((ts_ns, outlay_usdc));
        let cutoff = ts_ns - 86_400 * 1_000_000_000;
        self.rolling_window.retain(|(t, _)| *t >= cutoff);
        *self.per_market_outlay.entry(market_id).or_insert(0.0) += outlay_usdc;
        if self.daily_exposure() >= self.limits.max_daily_exposure_usdc {
            self.halt_reason.get_or_insert("max_daily_exposure");
        }
    }

    pub fn daily_exposure(&self) -> f64 {
        self.rolling_window.iter().map(|(_, v)| *v).sum()
    }

    pub fn market_exposure(&self, market_id: u32) -> f64 {
        self.per_market_outlay
            .get(&market_id)
            .copied()
            .unwrap_or(0.0)
    }

    /// True when a fresh order should be allowed through. The runner is the
    /// authority on this; strategies don't see the gate directly.
    pub fn can_open_position(&self, market_id: u32, prospective_outlay_usdc: f64) -> bool {
        self.can_open_position_ex(market_id, prospective_outlay_usdc, false)
    }

    /// Like can_open_position, but if `is_repair` is true (the buy reduces
    /// current |yes - no| imbalance), we exempt it from the gross daily and
    /// per-market exposure caps. This allows repair/pair legs to complete
    /// even after a tight exposure gate has been hit on initial deployment.
    /// Still respects max_clip, drawdown halt, and cash.
    ///
    /// This fixes the case where "gating exposure" (tight max_daily or
    /// max_per_market) unintentionally blocked the repair side, leaving the
    /// book stranded.
    pub fn can_open_position_ex(&self, market_id: u32, prospective_outlay_usdc: f64, is_repair: bool) -> bool {
        if self.halt_reason.is_some() {
            // Drawdown halt is hard. Exposure halts (daily cap) can be bypassed
            // for repair trades so we can still pair up and avoid stranding when
            // the user has gated gross exposure for capital/risk reasons.
            if let Some(reason) = self.halt_reason {
                if reason != "max_daily_exposure" {
                    return false;
                }
                if !is_repair {
                    return false;
                }
            }
        }
        if prospective_outlay_usdc > self.limits.max_clip_usdc {
            return false;
        }
        if !is_repair {
            let new_daily = self.daily_exposure() + prospective_outlay_usdc;
            if new_daily > self.limits.max_daily_exposure_usdc {
                return false;
            }
            let new_market = self.market_exposure(market_id) + prospective_outlay_usdc;
            if new_market > self.limits.max_per_market_exposure_usdc {
                return false;
            }
        }
        true
    }

    pub fn snapshot(&self, ts_ns: i64, equity_usdc: f64) -> PortfolioSnapshot {
        let drawdown_pct = if self.peak_equity > 0.0 {
            1.0 - equity_usdc / self.peak_equity
        } else {
            0.0
        };
        PortfolioSnapshot {
            ts_ns,
            equity_usdc,
            peak_equity_usdc: self.peak_equity,
            drawdown_pct,
            daily_exposure_usdc: self.daily_exposure(),
            halted: self.halt_reason.is_some(),
            halt_reason: self.halt_reason.map(|s| s.to_string()),
        }
    }

    pub fn starting_equity(&self) -> f64 {
        self.starting_equity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kelly_zero_for_no_edge() {
        assert_eq!(fractional_kelly_stake(0.50, 0.50, 100.0, 0.25, 5.0), 0.0);
        assert_eq!(fractional_kelly_stake(0.30, 0.50, 100.0, 0.25, 5.0), 0.0);
    }

    #[test]
    fn kelly_caps_at_max_stake() {
        let s = fractional_kelly_stake(0.90, 0.50, 1000.0, 1.0, 5.0);
        assert!(s <= 5.0 + 1e-9, "got {s}");
        assert!(s > 0.0);
    }

    #[test]
    fn drawdown_halt_triggers() {
        let mut p = PortfolioState::new(
            100.0,
            PortfolioLimits {
                max_drawdown_pct: 0.20,
                max_per_market_exposure_usdc: 100.0,
                ..Default::default()
            },
        );
        p.mark(100.0);
        assert!(p.can_open_position(1, 1.0));
        p.mark(79.0); // 21% drawdown
        assert!(!p.can_open_position(1, 1.0));
        let snap = p.snapshot(0, 79.0);
        assert!(snap.halted);
        assert_eq!(snap.halt_reason.as_deref(), Some("max_drawdown"));
    }

    #[test]
    fn daily_exposure_cap_blocks_after_window() {
        let mut p = PortfolioState::new(
            100.0,
            PortfolioLimits {
                max_daily_exposure_usdc: 10.0,
                max_per_market_exposure_usdc: 100.0,
                ..Default::default()
            },
        );
        p.mark(100.0);
        p.record_outlay(1, 0, 6.0);
        assert!(p.can_open_position(1, 3.0));
        p.record_outlay(1, 1_000_000_000, 4.0); // now at 10 total
        assert!(!p.can_open_position(1, 1.0));
    }

    #[test]
    fn rolling_window_drops_stale_entries() {
        let mut p = PortfolioState::new(
            100.0,
            PortfolioLimits {
                max_daily_exposure_usdc: 10.0,
                max_per_market_exposure_usdc: 100.0,
                ..Default::default()
            },
        );
        p.mark(100.0);
        let day = 86_400i64 * 1_000_000_000;
        p.record_outlay(1, 0, 8.0);
        p.record_outlay(2, day + 3_600 * 1_000_000_000, 1.0);
        assert!(p.daily_exposure() < 2.0);
    }

    #[test]
    fn per_market_cap_blocks_reload() {
        let mut p = PortfolioState::new(
            100.0,
            PortfolioLimits {
                max_per_market_exposure_usdc: 5.0,
                max_daily_exposure_usdc: 1000.0,
                max_clip_usdc: 3.0,
                ..Default::default()
            },
        );
        p.mark(100.0);
        assert!(p.can_open_position(7, 2.5));
        p.record_outlay(7, 0, 2.5);
        assert!(p.can_open_position(7, 2.4));
        p.record_outlay(7, 1, 2.4);
        // Total on market 7 = 4.9; one more 2.5 would exceed 5.0 cap.
        assert!(!p.can_open_position(7, 2.5));
        // But a different market is still OK.
        assert!(p.can_open_position(99, 2.5));
    }

    #[test]
    fn repair_buys_bypass_exposure_caps() {
        // Simulate being stranded long YES (yes=10, no=0), daily already at cap.
        let mut p = PortfolioState::new(
            100.0,
            PortfolioLimits {
                max_daily_exposure_usdc: 10.0,
                max_per_market_exposure_usdc: 20.0,
                max_clip_usdc: 100.0,
                ..Default::default()
            },
        );
        p.mark(100.0);
        p.record_outlay(1, 0, 10.0); // at daily cap
        // Non-repair buy (would increase imbalance) should be blocked.
        assert!(!p.can_open_position(1, 1.0));
        // But a repair buy (BuyNo, reduces |10-0|) should be allowed via ex.
        assert!(p.can_open_position_ex(1, 1.0, true /* is_repair */));
        // Even if it would push per-market over (if we had high current).
        p.record_outlay(1, 1, 9.0); // market at 19
        assert!(!p.can_open_position(1, 2.0));
        assert!(p.can_open_position_ex(1, 2.0, true));
    }
}
