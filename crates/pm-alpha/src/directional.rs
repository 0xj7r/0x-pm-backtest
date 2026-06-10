//! Directional (continuation) feature vector — exogenous only.
//!
//! These features feed the supervised P(continuation) model for trending
//! regimes (roadmap phase B). All are pure functions of `ExoState` (spot +
//! perp complex); every feature degrades to 0.0 when its input is absent so
//! partial data never poisons a vector.

use crate::state::ExoState;

pub const DIR_FEATURES: usize = 14;

pub const DIR_FEATURE_NAMES: [&str; DIR_FEATURES] = [
    "funding_rate_bps",     // last funding event, bps (carry/crowding)
    "oi_delta_5m",          // open-interest change, trailing 5m, frac
    "oi_delta_30m",         // open-interest change, trailing 30m, frac
    "basis_bps",            // perp minus spot, bps of spot
    "perp_flow_imbal_60s",  // signed perp taker flow, [-1, 1]
    "perp_burst_300s",      // large-print arrival rate vs baseline
    "liq_proxy",            // burst AND OI drop => forced-flow cascade
    "spot_flow_imbal_60s",  // signed spot taker flow, [-1, 1]
    "trend_60s_sigma",      // trailing return in bar-sigma units
    "trend_300s_sigma",
    "trend_1800s_sigma",
    "trend_alignment",      // sign agreement across the three horizons
    "vol_expansion",        // sigma(300s)/sigma(1800s) - 1
    "tau_fraction",
];

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DirFeatures {
    pub values: [f32; DIR_FEATURES],
}

/// One supervised continuation sample: the feature vector at a decision
/// instant plus the direction of the move then in progress and the window's
/// resolution. Label downstream: continuation = (move_up == resolved_yes).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct DirSample {
    pub ts_ns: i64,
    pub features: DirFeatures,
    pub move_up: bool,
    pub resolved_yes: bool,
}

/// Trailing return in remaining-bar sigma units (shared with the calibrator's
/// convention): r_w / (sigma_bar * sqrt(w/bar)).
fn trend_sigma(state: &ExoState, sigma_bar_bps: f64, w_s: i64) -> f64 {
    let bar_s = state.market.window_secs.max(1) as f64;
    let sigma_frac = (sigma_bar_bps / 10_000.0).max(1e-9);
    let r = state
        .spot
        .trailing_return(state.now_ns, w_s * 1_000_000_000)
        .unwrap_or(0.0);
    let scale = sigma_frac * (w_s as f64 / bar_s).sqrt().max(1e-6);
    (r / scale).clamp(-8.0, 8.0)
}

/// Build the directional vector at one decision instant. `sigma_bar_bps`
/// comes from the caller's belief computation (avoids recomputing vol).
pub fn dir_features(state: &ExoState, sigma_bar_bps: f64) -> DirFeatures {
    let now = state.now_ns;

    let (funding_bps, oi_5m, oi_30m, basis_bps, perp_imbal, perp_burst, liq_proxy) =
        match state.perp {
            Some(p) => {
                let funding = p.funding_at(now).unwrap_or(0.0) * 10_000.0;
                let oi5 = p.oi_delta_frac(now, 300_000_000_000).unwrap_or(0.0);
                let oi30 = p.oi_delta_frac(now, 1_800_000_000_000).unwrap_or(0.0);
                let basis = p
                    .basis_frac(state.spot, now)
                    .map(|b| b * 10_000.0)
                    .unwrap_or(0.0);
                let flow = p.trades.signed_flow_and_adverse(now, 60_000_000_000, true);
                // Burst: trade arrival in the last 60s vs the trailing-300s
                // baseline rate (1.0 = normal, >2 = burst).
                let short = p.trades.range(now - 60_000_000_000, now).len() as f64 / 60.0;
                let long = p.trades.range(now - 300_000_000_000, now).len() as f64 / 300.0;
                let burst = if long > 1e-9 { (short / long).min(8.0) } else { 0.0 };
                // Liquidation proxy: a print burst with falling OI is forced
                // flow being closed out, the most mechanical continuation
                // signal at this horizon.
                let liq = if burst > 2.0 && oi5 < -0.002 { burst * (-oi5) * 100.0 } else { 0.0 };
                (funding, oi5, oi30, basis, flow.imbalance, burst, liq.min(8.0))
            }
            None => (0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0),
        };

    let spot_flow = state
        .spot
        .signed_flow_and_adverse(now, 60_000_000_000, true)
        .imbalance;

    let t60 = trend_sigma(state, sigma_bar_bps, 60);
    let t300 = trend_sigma(state, sigma_bar_bps, 300);
    let t1800 = trend_sigma(state, sigma_bar_bps, 1800);
    let signs = [t60, t300, t1800].map(|t| if t > 0.1 { 1i8 } else if t < -0.1 { -1 } else { 0 });
    let alignment = if signs.iter().all(|s| *s == 1) || signs.iter().all(|s| *s == -1) {
        signs[0] as f64
    } else {
        0.0
    };

    let vol_expansion = {
        let short =
            crate::vol::realized_vol_bps_over_bar(state.spot, now, 300, 1, state.market.window_secs);
        match short {
            Some(s) if sigma_bar_bps > 1e-9 => ((s / sigma_bar_bps) - 1.0).clamp(-1.0, 3.0),
            _ => 0.0,
        }
    };

    DirFeatures {
        values: [
            funding_bps.clamp(-50.0, 50.0) as f32,
            (oi_5m * 100.0).clamp(-5.0, 5.0) as f32,
            (oi_30m * 100.0).clamp(-10.0, 10.0) as f32,
            basis_bps.clamp(-100.0, 100.0) as f32,
            perp_imbal.clamp(-1.0, 1.0) as f32,
            perp_burst as f32,
            liq_proxy as f32,
            spot_flow.clamp(-1.0, 1.0) as f32,
            t60 as f32,
            t300 as f32,
            t1800 as f32,
            alignment as f32,
            vol_expansion as f32,
            state.tau_fraction() as f32,
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{MarketMeta, PerpState, Token};
    use pm_types::{SpotHistory, SpotTick};

    fn tick(ts_s: i64, price: f64, qty: f32, sell: bool) -> SpotTick {
        SpotTick {
            ts_ns: ts_s * 1_000_000_000,
            price,
            quantity: qty,
            is_buyer_maker: sell,
        }
    }

    fn trending_spot(n: i64) -> SpotHistory {
        SpotHistory::new(
            (0..n)
                .map(|s| {
                    let noise = if s % 2 == 0 { 8.0 } else { -8.0 };
                    tick(s, 100_000.0 + 15.0 * s as f64 + noise, 1.0, false)
                })
                .collect(),
        )
    }

    fn state<'a>(spot: &'a SpotHistory, perp: Option<&'a PerpState>, now_s: i64) -> ExoState<'a> {
        ExoState {
            spot,
            perp,
            market: MarketMeta {
                token: Token::Btc,
                window_secs: 300,
                open_ts_ns: (now_s - 100) * 1_000_000_000,
                close_ts_ns: (now_s + 200) * 1_000_000_000,
                strike: 100_000.0,
            },
            now_ns: now_s * 1_000_000_000,
        }
    }

    #[test]
    fn degrades_to_zero_perp_features_without_perp() {
        let spot = trending_spot(2400);
        let f = dir_features(&state(&spot, None, 2300), 20.0);
        for i in 0..7 {
            assert_eq!(f.values[i], 0.0, "{}", DIR_FEATURE_NAMES[i]);
        }
        // Trend features still live.
        assert!(f.values[8] > 0.0, "uptrend should give positive t60");
        assert_eq!(f.values[11], 1.0, "all horizons aligned up");
    }

    #[test]
    fn liquidation_proxy_fires_on_burst_plus_oi_drop() {
        let spot = trending_spot(2400);
        let now_s = 2300i64;
        // Perp prints: baseline 1/s for 300s, burst 5/s in the last 60s.
        let mut prints = Vec::new();
        for s in (now_s - 300)..(now_s - 60) {
            prints.push(tick(s, 100_000.0, 1.0, true));
        }
        for s in (now_s - 60)..now_s {
            for k in 0..5 {
                prints.push(SpotTick {
                    ts_ns: s * 1_000_000_000 + k * 100_000_000,
                    price: 100_000.0,
                    quantity: 2.0,
                    is_buyer_maker: true,
                });
            }
        }
        let perp = PerpState {
            trades: SpotHistory::new(prints),
            oi: vec![
                ((now_s - 400) * 1_000_000_000, 100_000.0),
                ((now_s - 100) * 1_000_000_000, 100_000.0),
                ((now_s - 10) * 1_000_000_000, 98_500.0), // -1.5% in 5m
            ],
            funding: vec![((now_s - 1000) * 1_000_000_000, -0.0003)],
        };
        let f = dir_features(&state(&spot, Some(&perp), now_s), 20.0);
        assert!(f.values[5] > 2.0, "burst should register: {}", f.values[5]);
        assert!(f.values[1] < -1.0, "oi 5m drop in %: {}", f.values[1]);
        assert!(f.values[6] > 0.0, "liq proxy should fire: {}", f.values[6]);
        assert!((f.values[0] - -3.0).abs() < 0.01, "funding bps: {}", f.values[0]);
    }

    #[test]
    fn choppy_tape_has_no_alignment() {
        let spot = SpotHistory::new(
            (0..2400)
                .map(|s| {
                    let z = if (s / 30) % 2 == 0 { 1.001 } else { 0.999 };
                    tick(s, 100_000.0 * z, 1.0, false)
                })
                .collect(),
        );
        let f = dir_features(&state(&spot, None, 2300), 20.0);
        assert_eq!(f.values[11], 0.0, "no alignment in chop");
    }
}
