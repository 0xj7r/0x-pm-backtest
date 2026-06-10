//! The exogenous input contract.
//!
//! `ExoState` is everything an exogenous signal is allowed to see. It contains
//! no Polymarket book data by construction — that is the leakage guarantee.
//! Anything price-anchored (book mid, spread, depth) lives only in the harness
//! and downstream strategy layers.

use pm_types::SpotHistory;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Token {
    Btc,
    Eth,
    Sol,
    Xrp,
}

impl Token {
    pub fn from_slug(slug: &str) -> Option<Self> {
        let slug = slug.to_ascii_lowercase();
        if slug.starts_with("btc-") {
            Some(Self::Btc)
        } else if slug.starts_with("eth-") {
            Some(Self::Eth)
        } else if slug.starts_with("sol-") {
            Some(Self::Sol)
        } else if slug.starts_with("xrp-") {
            Some(Self::Xrp)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Btc => "BTC",
            Self::Eth => "ETH",
            Self::Sol => "SOL",
            Self::Xrp => "XRP",
        }
    }
}

/// Static facts about one up/down market window.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct MarketMeta {
    pub token: Token,
    /// Window length in seconds (300 for 5m, 900 for 15m).
    pub window_secs: u32,
    pub open_ts_ns: i64,
    pub close_ts_ns: i64,
    /// Underlying level to beat: the oracle value at window open. We proxy it
    /// with CEX spot at the open instant (no Chainlink history in our data).
    pub strike: f64,
}

/// Perp-complex history for the underlying (Binance USD-M futures):
/// taker prints, 5-minute open interest, 8h funding events. All series are
/// sorted by timestamp; queries must use at-or-before semantics.
#[derive(Debug, Default, Clone)]
pub struct PerpState {
    pub trades: SpotHistory,
    /// (ts_ns, open interest in contracts), 5-minute cadence.
    pub oi: Vec<(i64, f64)>,
    /// (ts_ns, funding rate), one row per funding event.
    pub funding: Vec<(i64, f64)>,
}

impl PerpState {
    fn at_or_before(series: &[(i64, f64)], ts_ns: i64) -> Option<f64> {
        let idx = series.partition_point(|(t, _)| *t <= ts_ns);
        if idx == 0 { None } else { Some(series[idx - 1].1) }
    }

    pub fn oi_at(&self, ts_ns: i64) -> Option<f64> {
        Self::at_or_before(&self.oi, ts_ns)
    }

    /// Relative OI change over the trailing window, e.g. -0.02 = 2% drop.
    pub fn oi_delta_frac(&self, ts_ns: i64, lookback_ns: i64) -> Option<f64> {
        let now = self.oi_at(ts_ns)?;
        let then = Self::at_or_before(&self.oi, ts_ns - lookback_ns)?;
        if then.abs() < 1e-9 { return None; }
        Some(now / then - 1.0)
    }

    pub fn funding_at(&self, ts_ns: i64) -> Option<f64> {
        Self::at_or_before(&self.funding, ts_ns)
    }

    /// Perp-minus-spot basis as a fraction of spot, at-or-before ts.
    pub fn basis_frac(&self, spot: &SpotHistory, ts_ns: i64) -> Option<f64> {
        let perp = self.trades.price_at_or_before(ts_ns)?;
        let s = spot.price_at_or_before(ts_ns)?;
        if s <= 0.0 { return None; }
        Some(perp / s - 1.0)
    }
}

/// Everything an exogenous signal may see at one decision instant.
pub struct ExoState<'a> {
    pub spot: &'a SpotHistory,
    /// Perp complex (None when not loaded; features must degrade gracefully).
    pub perp: Option<&'a PerpState>,
    pub market: MarketMeta,
    pub now_ns: i64,
}

impl ExoState<'_> {
    /// Seconds until resolution, floored at zero.
    pub fn time_remaining_s(&self) -> f64 {
        ((self.market.close_ts_ns - self.now_ns).max(0)) as f64 / 1e9
    }

    /// Fraction of the window still ahead, clamped to [0, 1].
    pub fn tau_fraction(&self) -> f64 {
        if self.market.window_secs == 0 {
            return 0.0;
        }
        (self.time_remaining_s() / self.market.window_secs as f64).clamp(0.0, 1.0)
    }

    /// Last spot price at or before now.
    pub fn spot_now(&self) -> Option<f64> {
        self.spot.price_at_or_before(self.now_ns)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::SpotTick;

    fn meta(open_ns: i64, close_ns: i64) -> MarketMeta {
        MarketMeta {
            token: Token::Btc,
            window_secs: 300,
            open_ts_ns: open_ns,
            close_ts_ns: close_ns,
            strike: 100_000.0,
        }
    }

    #[test]
    fn time_remaining_floors_at_zero() {
        let spot = SpotHistory::default();
        let s = ExoState {
            spot: &spot,
            perp: None,
            market: meta(0, 300_000_000_000),
            now_ns: 400_000_000_000,
        };
        assert_eq!(s.time_remaining_s(), 0.0);
        assert_eq!(s.tau_fraction(), 0.0);
    }

    #[test]
    fn tau_fraction_clamps_and_scales() {
        let spot = SpotHistory::default();
        let s = ExoState {
            spot: &spot,
            perp: None,
            market: meta(0, 300_000_000_000),
            now_ns: 150_000_000_000,
        };
        assert!((s.tau_fraction() - 0.5).abs() < 1e-12);

        // Before the open, remaining exceeds the window: clamp to 1.
        let early = ExoState {
            spot: &spot,
            perp: None,
            market: meta(0, 300_000_000_000),
            now_ns: -100_000_000_000,
        };
        assert_eq!(early.tau_fraction(), 1.0);
    }

    #[test]
    fn token_from_slug() {
        assert_eq!(Token::from_slug("btc-updown-5m-1777593600"), Some(Token::Btc));
        assert_eq!(Token::from_slug("eth-updown-15m-1"), Some(Token::Eth));
        assert_eq!(Token::from_slug("doge-updown-5m-1"), None);
    }

    #[test]
    fn spot_now_reads_last_price() {
        let spot = SpotHistory::new(vec![SpotTick {
            ts_ns: 10,
            price: 99.5,
            quantity: 1.0,
            is_buyer_maker: false,
        }]);
        let s = ExoState {
            spot: &spot,
            perp: None,
            market: meta(0, 300_000_000_000),
            now_ns: 20,
        };
        assert_eq!(s.spot_now(), Some(99.5));
    }
}
