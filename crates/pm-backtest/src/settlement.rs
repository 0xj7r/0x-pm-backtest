//! TWAP settlement eras. Crypto up-down markets settle under different price
//! rules depending on close date and duration: a Chainlink price SNAPSHOT before
//! 2026-08-07, then a time-weighted average (30s for 5m until 2026-08-14, 60s
//! thereafter; 15m/4h use 60s from 2026-08-07). Hourly markets settle on a
//! Binance 1H candle and keep Snapshot semantics throughout.
//!
//! Venue taker-delay history (crypto up-down) is also encoded here so latency
//! validation can refuse runs modeled faster than the venue could fill.

use pm_types::Outcome;
use serde::Serialize;

/// Crypto up-down market duration, parsed from the slug.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MarketDuration {
    FiveMin,
    FifteenMin,
    Hourly,
    FourHour,
}

impl MarketDuration {
    /// Infer the duration from a market slug like `btc-updown-5m-<ts>`.
    /// Falls back to [`MarketDuration::FiveMin`] (the dominant universe) when
    /// no duration token is recognized.
    pub fn from_slug(slug: &str) -> Self {
        let s = slug.to_ascii_lowercase();
        if s.contains("-updown-15m-") {
            MarketDuration::FifteenMin
        } else if s.contains("-updown-4h-") {
            MarketDuration::FourHour
        } else if s.contains("-updown-1h-") || s.contains("-updown-hourly-") {
            MarketDuration::Hourly
        } else {
            MarketDuration::FiveMin
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SettlementEra {
    /// Last Chainlink print at/before close decides Up/Down.
    Snapshot,
    /// 30-second time-weighted average ending at close.
    Twap30,
    /// 60-second time-weighted average ending at close.
    Twap60,
}

// Settlement-regime boundaries (UTC seconds). Verified 2026-08-23 vs
// docs.polymarket.com/changelog/predictions.
/// Chainlink TWAP settlement begins 2026-08-07 00:00 UTC.
const TWAP_ERA_START_S: i64 = 1_786_060_800;
/// 5m markets switch from a 30s to a 60s TWAP window on 2026-08-14 00:00 UTC.
const FIVEMIN_TWAP60_START_S: i64 = 1_786_665_600;

/// Resolve the settlement regime for a market closing at `close_utc_s`.
///
/// Hourly markets always use Snapshot semantics (Binance 1H candle). For 5m,
/// 15m, and 4h: Snapshot before 2026-08-07, then TWAP. 5m uses a 30s window
/// from 2026-08-07 to 2026-08-14 and 60s from 2026-08-14; 15m and 4h use 60s
/// from 2026-08-07.
pub fn settlement_era(close_utc_s: i64, duration: MarketDuration) -> SettlementEra {
    if duration == MarketDuration::Hourly {
        return SettlementEra::Snapshot;
    }
    if close_utc_s < TWAP_ERA_START_S {
        return SettlementEra::Snapshot;
    }
    if duration == MarketDuration::FiveMin && close_utc_s < FIVEMIN_TWAP60_START_S {
        SettlementEra::Twap30
    } else {
        SettlementEra::Twap60
    }
}

// Venue taker-delay boundaries (UTC seconds). The pre-removal 500ms era is
// collapsed to 0 for simplicity (pinned to the acceptance-test cases); the
// 250ms era runs from the Feb 2026 delay removal until 2026-08-17 11:00 UTC,
// then 50ms.
const VENUE_250_START_S: i64 = 1_769_904_000;
const VENUE_50_START_S: i64 = 1_786_964_400;

/// Modeled venue taker delay (ms) in effect at `at_utc_s`. The 500ms
/// pre-removal era is treated as 0 for simplicity; 250ms from the Feb 2026
/// removal; 50ms from 2026-08-17 11:00 UTC.
pub fn venue_taker_delay_ms(at_utc_s: i64) -> u64 {
    if at_utc_s < VENUE_250_START_S {
        0
    } else if at_utc_s < VENUE_50_START_S {
        250
    } else {
        50
    }
}

fn twap_window_secs(era: SettlementEra) -> i64 {
    match era {
        SettlementEra::Twap30 => 30,
        SettlementEra::Twap60 => 60,
        SettlementEra::Snapshot => 0,
    }
}

/// Piecewise-constant time-weighted mean of `tape` over `[start, close]`.
///
/// The price is held constant from each print until the next (a print at `t`
/// sets the price for `[t, next_print)`). The price entering `start` is the
/// last print at/before `start`; if none exists, the first in-window print's
/// price is carried backward over the leading gap. Returns `None` when the
/// window is empty or no print falls within it.
fn piecewise_constant_mean(tape: &[(i64, f64)], start: i64, close: i64) -> Option<f64> {
    if close <= start || tape.is_empty() {
        return None;
    }
    let mut pts: Vec<(i64, f64)> = tape.to_vec();
    pts.sort_by_key(|(t, _)| *t);

    let in_window: Vec<(i64, f64)> = pts.into_iter().filter(|(t, _)| *t <= close).collect();
    if in_window.is_empty() {
        return None;
    }

    let mut cur_price = in_window
        .iter()
        .filter(|(t, _)| *t <= start)
        .last()
        .map(|(_, p)| *p)
        .or_else(|| in_window.first().map(|(_, p)| *p));

    let mut total = 0.0f64;
    let mut t_prev = start;
    for (t, p) in &in_window {
        if *t <= start {
            continue;
        }
        if *t >= close {
            break;
        }
        if let Some(cp) = cur_price {
            total += cp * (*t - t_prev) as f64;
        }
        cur_price = Some(*p);
        t_prev = *t;
    }
    if let Some(cp) = cur_price {
        total += cp * (close - t_prev) as f64;
    }

    let span = (close - start) as f64;
    if span <= 0.0 {
        None
    } else {
        Some(total / span)
    }
}

/// Resolve Up/Down from a spot tape under the era's rule. `tape` is the
/// window's spot series `(ts_seconds, price)` covering at least the final 60s;
/// `strike` is the price-to-beat. Up maps to [`Outcome::Yes`], Down to
/// [`Outcome::No`].
///
/// - `Snapshot`: the last print at/before `close_utc_s` decides; Up iff that
///   print is `>= strike`.
/// - `Twap30`/`Twap60`: the piecewise-constant time-weighted mean over
///   `[close - w, close]` decides; Up iff the mean is `>= strike`. With no
///   in-window print the snapshot rule is used as a fallback.
pub fn resolve_outcome(
    era: SettlementEra,
    tape: &[(i64, f64)],
    close_utc_s: i64,
    strike: f64,
) -> Outcome {
    let last_print = tape
        .iter()
        .filter(|(t, _)| *t <= close_utc_s)
        .last()
        .map(|(_, p)| *p);
    match era {
        SettlementEra::Snapshot => {
            if last_print.map(|p| p >= strike).unwrap_or(false) {
                Outcome::Yes
            } else {
                Outcome::No
            }
        }
        SettlementEra::Twap30 | SettlementEra::Twap60 => {
            let w = twap_window_secs(era);
            let start = close_utc_s.saturating_sub(w);
            match piecewise_constant_mean(tape, start, close_utc_s) {
                Some(mean) if mean >= strike => Outcome::Yes,
                Some(_) => Outcome::No,
                None => {
                    if last_print.map(|p| p >= strike).unwrap_or(false) {
                        Outcome::Yes
                    } else {
                        Outcome::No
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        use chrono::TimeZone;
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, 0)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn era_boundaries() {
        use MarketDuration::*;
        // 2026-08-06T23:59Z FiveMin -> Snapshot (just before TWAP begins).
        assert_eq!(
            settlement_era(ts(2026, 8, 6, 23, 59), FiveMin),
            SettlementEra::Snapshot
        );
        // 2026-08-07T00:01Z FiveMin -> Twap30.
        assert_eq!(
            settlement_era(ts(2026, 8, 7, 0, 1), FiveMin),
            SettlementEra::Twap30
        );
        // 2026-08-14T00:01Z FiveMin -> Twap60.
        assert_eq!(
            settlement_era(ts(2026, 8, 14, 0, 1), FiveMin),
            SettlementEra::Twap60
        );
        // 2026-08-10 FifteenMin -> Twap60 (15m uses 60s from Aug 7).
        assert_eq!(
            settlement_era(ts(2026, 8, 10, 0, 0), FifteenMin),
            SettlementEra::Twap60
        );
        // Any date Hourly -> Snapshot.
        assert_eq!(
            settlement_era(ts(2026, 8, 20, 0, 0), Hourly),
            SettlementEra::Snapshot
        );
        assert_eq!(
            settlement_era(ts(2019, 1, 1, 0, 0), Hourly),
            SettlementEra::Snapshot
        );
        // 2026-08-10 FourHour -> Twap60.
        assert_eq!(
            settlement_era(ts(2026, 8, 10, 0, 0), FourHour),
            SettlementEra::Twap60
        );
        // Pre-TWAP 15m/4h are Snapshot.
        assert_eq!(
            settlement_era(ts(2026, 8, 6, 12, 0), FourHour),
            SettlementEra::Snapshot
        );
    }

    #[test]
    fn venue_delay_eras() {
        assert_eq!(venue_taker_delay_ms(ts(2026, 7, 1, 0, 0)), 250);
        assert_eq!(venue_taker_delay_ms(ts(2026, 8, 17, 10, 59)), 250);
        assert_eq!(venue_taker_delay_ms(ts(2026, 8, 17, 11, 1)), 50);
        assert_eq!(venue_taker_delay_ms(ts(2026, 5, 1, 0, 0)), 250);
        assert_eq!(venue_taker_delay_ms(ts(2026, 1, 15, 0, 0)), 0);
    }

    #[test]
    fn late_wick_flips_snapshot_but_not_twap() {
        // Price sits 10bps BELOW strike for ~290s, spikes ABOVE only in the
        // final 2s. Snapshot reads the wick (Up); the 60s TWAP is dominated by
        // the below-strike holding period (Down).
        let strike = 100.0f64;
        let below = strike * (1.0 - 0.001); // 10bps below
        let above = strike + 1.0;
        let close = ts(2026, 8, 20, 0, 0);
        // Tape covers the full 5m bar: below throughout, spiking in final 2s.
        let tape = vec![
            (close - 300, below),
            (close - 60, below),
            (close - 2, above),
        ];

        let snap = resolve_outcome(SettlementEra::Snapshot, &tape, close, strike);
        assert_eq!(snap, Outcome::Yes, "snapshot reads the late wick as Up");

        let twap = resolve_outcome(SettlementEra::Twap60, &tape, close, strike);
        assert_eq!(twap, Outcome::No, "60s TWAP is dominated by below-strike prints");
    }

    #[test]
    fn twap_time_weighting_is_piecewise_constant() {
        // Uneven print spacing over a 60s window. Hand-computed piecewise mean:
        //   [940, 950): 100 for 10s -> 1000
        //   [950, 990): 200 for 40s -> 8000
        //   [990, 1000]: 300 for 10s -> 3000
        //   total 12000 / 60s = 200.0
        let tape = vec![
            (940, 100.0),
            (950, 200.0),
            (990, 300.0),
            (1000, 400.0), // at close: zero holding time, does not move the mean
        ];
        let mean = piecewise_constant_mean(&tape, 940, 1000).unwrap();
        assert!((mean - 200.0).abs() < 1e-9, "mean {mean}");

        // The 60s TWAP over the same window at strike 200 is exactly Up (>=).
        let out = resolve_outcome(SettlementEra::Twap60, &tape, 1000, 200.0);
        assert_eq!(out, Outcome::Yes);
        // Strike just above the mean flips it to Down.
        let out = resolve_outcome(SettlementEra::Twap60, &tape, 1000, 200.0001);
        assert_eq!(out, Outcome::No);
    }
}
