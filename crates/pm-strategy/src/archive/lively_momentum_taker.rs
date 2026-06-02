//! Lively-style repeated taker momentum.
//!
//! This targets the wallet-derived shape: many small taker clips per market,
//! following the current window winner when the underlying spot tape confirms.
//! It is intentionally separate from Bonereaper so we can test execution shape
//! without inheriting old model gates.

use crate::spot_momentum::weighted_multi_tf_return;
use crate::{Ctx, OrderRequest, Side, Strategy, StrategyOutput};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};

const DEFAULT_WINDOW_NS: i64 = 300_000_000_000;

#[derive(Debug, Clone)]
pub struct LivelyMomentumTakerConfig {
    /// Wait this long after market open before taking. This avoids the opening
    /// print/noise but does not force the strategy into a late-only sleeve.
    pub min_seconds_after_open: f32,
    /// Stop opening fresh clips after this many seconds from market open.
    /// BTC 5m testing showed early tail-spray and late drift-chasing can swamp
    /// the edge; longer horizons should use a horizon-specific value.
    pub max_seconds_after_open: f32,
    /// Stop taking inside this final window to avoid unresolved latency risk.
    pub stop_seconds_before_close: f32,
    /// Minimum time between child clips.
    pub refresh_secs: f32,
    /// Minimum absolute move from market open to current spot, in bps.
    pub min_window_delta_bps: f64,
    /// Minimum fast spot return in the same direction, in bps.
    pub min_fast_return_bps: f64,
    /// If true, weak-signal markets still emit clips using a deterministic
    /// fallback side. This matches the target wallet's broad participation:
    /// signal controls side bias, not whether the strategy shows up at all.
    pub always_on: bool,
    /// Book skew needed for the weak-signal fallback to trust market pricing.
    /// Below this, fallback uses tiny spot drift if present, then the book mid.
    pub fallback_book_skew: f32,
    /// Do not chase above this side ask.
    pub max_entry_price: f32,
    /// Avoid cheap-tail spray unless a separate tail sleeve owns the sizing.
    pub min_entry_price: f32,
    /// Dollar notional per child clip.
    pub clip_usdc: f64,
    /// Clip multiplier for the first tradable phase. This throttles the
    /// 30-45s bucket without deleting it, because hard-skipping that bucket
    /// removed too much upside in the May slice.
    pub early_phase_clip_multiplier: f64,
    /// Multiplier when the shared model points the other way and this fill is
    /// not cheap enough to justify leaning through it.
    pub weak_model_clip_multiplier: f64,
    /// Multiplier when the model side probability is below the current fill
    /// price for the side we want to buy.
    pub negative_edge_clip_multiplier: f64,
    /// Start tapering same-side residual once it reaches this fraction of cap.
    pub residual_taper_start_frac: f64,
    /// Minimum residual taper multiplier at the residual cap.
    pub residual_min_clip_multiplier: f64,
    /// Hold instead of emitting dust clips below this total multiplier.
    pub min_clip_multiplier_to_emit: f64,
    /// Maximum child clips per market.
    pub max_clips: usize,
    /// Do not intentionally add to a same-side residual beyond this many
    /// shares. Above the cap, same-side signals pause; opposite-side signals
    /// can still buy and naturally rebalance the book.
    pub max_residual_shares: f64,
    /// Extra price discipline while there is still enough time for a full
    /// reversal. This is not a pair-cost rule; it only avoids paying near-par
    /// early when the wallet-style edge should come from repeated loading.
    pub max_early_entry_price: f32,
    /// Price cap for the middle of the window.
    pub max_mid_entry_price: f32,
    /// Book depth the taker clip may sweep.
    pub sweep_depth: usize,
    /// Market window length in ns. The runner sets this from the actual market
    /// open/close timestamps so the same execution shape can be tested on
    /// BTC/ETH 5m and 15m markets without leaking a BTC5m assumption.
    pub market_window_ns: i64,
}

impl Default for LivelyMomentumTakerConfig {
    fn default() -> Self {
        Self {
            min_seconds_after_open: 30.0,
            max_seconds_after_open: 120.0,
            stop_seconds_before_close: 4.0,
            refresh_secs: 2.0,
            min_window_delta_bps: 0.8,
            min_fast_return_bps: 0.15,
            always_on: false,
            fallback_book_skew: 0.02,
            max_entry_price: 0.97,
            min_entry_price: 0.25,
            clip_usdc: 12.0,
            early_phase_clip_multiplier: 1.0,
            weak_model_clip_multiplier: 1.0,
            negative_edge_clip_multiplier: 1.0,
            residual_taper_start_frac: 1.0,
            residual_min_clip_multiplier: 1.0,
            min_clip_multiplier_to_emit: 0.0,
            max_clips: 30,
            max_residual_shares: 150.0,
            max_early_entry_price: 0.88,
            max_mid_entry_price: 0.94,
            sweep_depth: 5,
            market_window_ns: DEFAULT_WINDOW_NS,
        }
    }
}

pub struct LivelyMomentumTaker {
    cfg: LivelyMomentumTakerConfig,
    clips: usize,
    last_emit_ns: i64,
}

impl LivelyMomentumTaker {
    pub fn new(cfg: LivelyMomentumTakerConfig) -> Self {
        Self {
            cfg,
            clips: 0,
            last_emit_ns: i64::MIN / 2,
        }
    }
}

fn shares_capped(usdc: f64, fill_px: f32) -> f64 {
    if usdc <= 0.0 || fill_px <= 0.0 {
        return 0.0;
    }
    let raw = (usdc * 0.98) / fill_px as f64;
    ((raw * 1000.0).floor() / 1000.0).max(0.0)
}

fn no_ask(event: &ReplayEvent) -> f32 {
    (1.0 - event.yes_bid).clamp(0.0, 1.0)
}

fn phase_price_cap(cfg: &LivelyMomentumTakerConfig, seconds_to_close: f32) -> f32 {
    let phase_cap = if seconds_to_close > 120.0 {
        cfg.max_early_entry_price
    } else if seconds_to_close > 30.0 {
        cfg.max_mid_entry_price
    } else {
        cfg.max_entry_price
    };
    phase_cap.min(cfg.max_entry_price).clamp(0.0, 0.999)
}

fn fallback_side(
    event: &ReplayEvent,
    window_delta_bps: f64,
    fast_bps: f64,
    book_skew: f32,
) -> Side {
    if event.yes_mid >= 0.5 + book_skew {
        Side::BuyYes
    } else if event.yes_mid <= 0.5 - book_skew {
        Side::BuyNo
    } else if window_delta_bps > 0.0 || fast_bps > 0.0 {
        Side::BuyYes
    } else {
        Side::BuyNo
    }
}

fn side_is_yes(side: Side) -> bool {
    matches!(side, Side::BuyYes | Side::SellNo)
}

fn attribution_side_probability(ctx: &Ctx, side: Side) -> Option<f32> {
    let attr = ctx.model_attribution?;
    let p_direction = attr.side_probability_post_meta;
    if !p_direction.is_finite() {
        return None;
    }
    let p_direction = p_direction.clamp(0.0, 1.0);
    let p_yes = if attr.direction_side_is_yes {
        p_direction
    } else {
        1.0 - p_direction
    };
    Some(if side_is_yes(side) {
        p_yes
    } else {
        1.0 - p_yes
    })
}

fn same_side_residual(ctx: &Ctx, side: Side) -> f64 {
    let residual = ctx.yes_shares - ctx.no_shares;
    match side {
        Side::BuyYes => residual.max(0.0),
        Side::BuyNo => (-residual).max(0.0),
        Side::SellYes | Side::SellNo => 0.0,
    }
}

fn clip_multiplier(
    cfg: &LivelyMomentumTakerConfig,
    ctx: &Ctx,
    side: Side,
    fill_px: f32,
    seconds_after_open: f32,
) -> f64 {
    let mut mult = 1.0_f64;

    if seconds_after_open < 45.0 {
        mult *= cfg.early_phase_clip_multiplier.clamp(0.0, 1.0);
    }

    if let Some(p_side) = attribution_side_probability(ctx, side) {
        let p_side = p_side as f64;
        let fill_px = fill_px as f64;
        let attr = ctx.model_attribution.expect("checked above");
        if attr.direction_side_is_yes != side_is_yes(side) && fill_px > 0.55 {
            mult *= cfg.weak_model_clip_multiplier.clamp(0.0, 1.0);
        }
        if p_side < fill_px {
            mult *= cfg.negative_edge_clip_multiplier.clamp(0.0, 1.0);
        }
    }

    let residual_cap = cfg.max_residual_shares.max(0.0);
    if residual_cap > 0.0 {
        let taper_start = residual_cap * cfg.residual_taper_start_frac.clamp(0.0, 1.0);
        let same_residual = same_side_residual(ctx, side);
        if same_residual > taper_start && residual_cap > taper_start {
            let progress =
                ((same_residual - taper_start) / (residual_cap - taper_start)).clamp(0.0, 1.0);
            let min_mult = cfg.residual_min_clip_multiplier.clamp(0.0, 1.0);
            mult *= 1.0 - progress * (1.0 - min_mult);
        }
    }

    mult.clamp(0.0, 1.0)
}

impl Strategy for LivelyMomentumTaker {
    fn on_event(
        &mut self,
        event: &ReplayEvent,
        ctx: &Ctx,
        spot: &SpotHistory,
        _trades: &TradeHistory,
    ) -> StrategyOutput {
        if self.clips >= self.cfg.max_clips || spot.is_empty() {
            return StrategyOutput::hold();
        }
        let market_window_ns = self.cfg.market_window_ns.max(1);
        if event.yes_bid <= 0.0 || event.yes_ask <= 0.0 || ctx.market_close_ns <= market_window_ns {
            return StrategyOutput::hold();
        }

        let open_ns = ctx.market_close_ns - market_window_ns;
        let seconds_after_open = (event.ts_ns - open_ns) as f32 / 1e9;
        let seconds_to_close = (ctx.market_close_ns - event.ts_ns) as f32 / 1e9;
        if seconds_after_open < self.cfg.min_seconds_after_open
            || seconds_after_open > self.cfg.max_seconds_after_open
            || seconds_to_close < self.cfg.stop_seconds_before_close
        {
            return StrategyOutput::hold();
        }
        let min_interval_ns = (self.cfg.refresh_secs.max(0.0) as f64 * 1e9) as i64;
        if event.ts_ns - self.last_emit_ns < min_interval_ns {
            return StrategyOutput::hold();
        }

        let Some(open_px) = spot.price_at_or_before(open_ns) else {
            return StrategyOutput::hold();
        };
        let Some(now_px) = spot.price_at_or_before(event.ts_ns) else {
            return StrategyOutput::hold();
        };
        if open_px <= 0.0 || !open_px.is_finite() || !now_px.is_finite() {
            return StrategyOutput::hold();
        }

        let window_delta_bps = (now_px / open_px - 1.0) * 10_000.0;
        let Some(fast_return) = weighted_multi_tf_return(event.ts_ns, spot) else {
            return StrategyOutput::hold();
        };
        let fast_bps = fast_return * 10_000.0;

        let signal_side = if window_delta_bps >= self.cfg.min_window_delta_bps
            && fast_bps >= -self.cfg.min_fast_return_bps
        {
            Side::BuyYes
        } else if window_delta_bps <= -self.cfg.min_window_delta_bps
            && fast_bps <= self.cfg.min_fast_return_bps
        {
            Side::BuyNo
        } else if fast_bps >= self.cfg.min_fast_return_bps {
            Side::BuyYes
        } else if fast_bps <= -self.cfg.min_fast_return_bps {
            Side::BuyNo
        } else if self.cfg.always_on {
            fallback_side(
                event,
                window_delta_bps,
                fast_bps,
                self.cfg.fallback_book_skew.max(0.0),
            )
        } else {
            return StrategyOutput::hold();
        };
        let signal_fill_px = match signal_side {
            Side::BuyYes => event.yes_ask,
            Side::BuyNo => no_ask(event),
            Side::SellYes | Side::SellNo => return StrategyOutput::hold(),
        };

        let residual = ctx.yes_shares - ctx.no_shares;
        let residual_cap = self.cfg.max_residual_shares.max(0.0);
        if matches!(signal_side, Side::BuyYes) && residual >= residual_cap {
            return StrategyOutput::hold();
        }
        if matches!(signal_side, Side::BuyNo) && -residual >= residual_cap {
            return StrategyOutput::hold();
        }

        let price_cap = phase_price_cap(&self.cfg, seconds_to_close);
        if signal_fill_px < self.cfg.min_entry_price || signal_fill_px > price_cap {
            return StrategyOutput::hold();
        }

        let mult = clip_multiplier(
            &self.cfg,
            ctx,
            signal_side,
            signal_fill_px,
            seconds_after_open,
        );
        if mult < self.cfg.min_clip_multiplier_to_emit.clamp(0.0, 1.0) {
            return StrategyOutput::hold();
        }

        let shares = shares_capped(self.cfg.clip_usdc * mult, signal_fill_px);
        if shares <= 0.0 {
            return StrategyOutput::hold();
        }
        self.clips += 1;
        self.last_emit_ns = event.ts_ns;
        StrategyOutput::one(OrderRequest {
            side: signal_side,
            shares,
            max_depth: self.cfg.sweep_depth.max(1),
            limit_price: Some(price_cap),
            tag: "lively_momentum_taker",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_model::ModelAttribution;
    use pm_types::{BookLevel, MarketId, ReplayFlags, SpotTick, tape::TAPE_DEPTH};

    fn event(ts_ns: i64, yes_bid: f32, yes_ask: f32) -> ReplayEvent {
        let mut bids = [BookLevel::default(); TAPE_DEPTH];
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        bids[0] = BookLevel {
            price: yes_bid,
            size: 500.0,
        };
        asks[0] = BookLevel {
            price: yes_ask,
            size: 500.0,
        };
        ReplayEvent {
            ts_ns,
            market_id: MarketId(1),
            yes_mid: 0.5 * (yes_bid + yes_ask),
            yes_bid,
            yes_ask,
            volume: 0.0,
            bids,
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    fn ctx(close_ns: i64) -> Ctx {
        Ctx {
            events_seen: 1,
            yes_shares: 0.0,
            no_shares: 0.0,
            cash_usdc: 100.0,
            market_yes_range_so_far: 0.0,
            prior_market_range_1d: 0.0,
            prior_market_range_3d: 0.0,
            prior_market_range_7d: 0.0,
            model_output: None,
            model_attribution: None,
            market_close_ns: close_ns,
            btc_net_exposure_shares: 0.0,
            eth_net_exposure_shares: 0.0,
            daily_start_cash_usdc: 0.0,
            daily_loss_cap_pct: 1.0,
            current_daily_loss_pct: 0.0,
        }
    }

    fn rising_spot(close_ns: i64, now_ns: i64) -> SpotHistory {
        let open_ns = close_ns - DEFAULT_WINDOW_NS;
        SpotHistory::new(vec![
            SpotTick {
                ts_ns: open_ns,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns - 30_000_000_000,
                price: 100.02,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 100.05,
                quantity: 1.0,
                is_buyer_maker: false,
            },
        ])
    }

    fn falling_spot(close_ns: i64, now_ns: i64) -> SpotHistory {
        let open_ns = close_ns - DEFAULT_WINDOW_NS;
        SpotHistory::new(vec![
            SpotTick {
                ts_ns: open_ns,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns - 30_000_000_000,
                price: 99.98,
                quantity: 1.0,
                is_buyer_maker: true,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 99.95,
                quantity: 1.0,
                is_buyer_maker: true,
            },
        ])
    }

    #[test]
    fn emits_repeated_yes_clips_when_window_winner_is_confirmed() {
        let close_ns = 1_000_000_000_000;
        let now_ns = close_ns - 240_000_000_000;
        let mut strategy = LivelyMomentumTaker::new(LivelyMomentumTakerConfig {
            clip_usdc: 15.0,
            refresh_secs: 1.0,
            ..LivelyMomentumTakerConfig::default()
        });
        let spot = rising_spot(close_ns, now_ns);

        let first = strategy.on_event(
            &event(now_ns, 0.58, 0.60),
            &ctx(close_ns),
            &spot,
            &TradeHistory::default(),
        );
        let second = strategy.on_event(
            &event(now_ns + 2_000_000_000, 0.59, 0.61),
            &ctx(close_ns),
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(first.orders.len(), 1);
        assert_eq!(first.orders[0].side, Side::BuyYes);
        assert_eq!(first.orders[0].tag, "lively_momentum_taker");
        assert_eq!(second.orders.len(), 1);
    }

    #[test]
    fn holds_when_same_side_signal_would_exceed_residual_cap() {
        let close_ns = 1_000_000_000_000;
        let now_ns = close_ns - 240_000_000_000;
        let mut strategy = LivelyMomentumTaker::new(LivelyMomentumTakerConfig {
            max_residual_shares: 50.0,
            ..LivelyMomentumTakerConfig::default()
        });
        let mut context = ctx(close_ns);
        context.yes_shares = 55.0;
        context.no_shares = 0.0;
        let spot = rising_spot(close_ns, now_ns);

        let out = strategy.on_event(
            &event(now_ns, 0.58, 0.60),
            &context,
            &spot,
            &TradeHistory::default(),
        );

        assert!(out.orders.is_empty());
    }

    #[test]
    fn buys_opposite_side_when_signal_flips_even_with_existing_residual() {
        let close_ns = 1_000_000_000_000;
        let now_ns = close_ns - 240_000_000_000;
        let mut strategy = LivelyMomentumTaker::new(LivelyMomentumTakerConfig {
            max_residual_shares: 50.0,
            ..LivelyMomentumTakerConfig::default()
        });
        let mut context = ctx(close_ns);
        context.yes_shares = 55.0;
        context.no_shares = 0.0;
        let spot = falling_spot(close_ns, now_ns);

        let out = strategy.on_event(
            &event(now_ns, 0.40, 0.42),
            &context,
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyNo);
        assert_eq!(out.orders[0].tag, "lively_momentum_taker");
    }

    #[test]
    fn throttles_early_phase_without_skipping_it() {
        let close_ns = 1_000_000_000_000;
        let now_ns = close_ns - 265_000_000_000;
        let mut strategy = LivelyMomentumTaker::new(LivelyMomentumTakerConfig {
            clip_usdc: 15.0,
            refresh_secs: 1.0,
            early_phase_clip_multiplier: 0.50,
            ..LivelyMomentumTakerConfig::default()
        });
        let spot = rising_spot(close_ns, now_ns);

        let out = strategy.on_event(
            &event(now_ns, 0.58, 0.60),
            &ctx(close_ns),
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyYes);
        assert!(out.orders[0].shares < shares_capped(15.0, 0.60));
        assert_eq!(out.orders[0].shares, shares_capped(7.5, 0.60));
    }

    #[test]
    fn throttles_model_disagreed_high_price_clip() {
        let close_ns = 1_000_000_000_000;
        let now_ns = close_ns - 240_000_000_000;
        let mut strategy = LivelyMomentumTaker::new(LivelyMomentumTakerConfig {
            clip_usdc: 20.0,
            refresh_secs: 1.0,
            weak_model_clip_multiplier: 0.50,
            negative_edge_clip_multiplier: 0.50,
            min_clip_multiplier_to_emit: 0.20,
            ..LivelyMomentumTakerConfig::default()
        });
        let mut context = ctx(close_ns);
        context.model_attribution = Some(ModelAttribution {
            direction_side_is_yes: false,
            side_probability_post_meta: 0.60,
            ..ModelAttribution::default()
        });
        let spot = rising_spot(close_ns, now_ns);

        let out = strategy.on_event(
            &event(now_ns, 0.58, 0.60),
            &context,
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyYes);
        assert_eq!(out.orders[0].shares, shares_capped(5.0, 0.60));
    }

    #[test]
    fn tapers_same_side_residual_before_hard_cap() {
        let close_ns = 1_000_000_000_000;
        let now_ns = close_ns - 240_000_000_000;
        let mut strategy = LivelyMomentumTaker::new(LivelyMomentumTakerConfig {
            clip_usdc: 20.0,
            refresh_secs: 1.0,
            max_residual_shares: 100.0,
            residual_taper_start_frac: 0.50,
            residual_min_clip_multiplier: 0.50,
            ..LivelyMomentumTakerConfig::default()
        });
        let mut context = ctx(close_ns);
        context.yes_shares = 75.0;
        let spot = rising_spot(close_ns, now_ns);

        let out = strategy.on_event(
            &event(now_ns, 0.58, 0.60),
            &context,
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].shares, shares_capped(15.0, 0.60));
    }

    #[test]
    fn uses_configured_market_window_for_open_price() {
        let close_ns = 1_900_000_000_000;
        let window_ns = 900_000_000_000;
        let now_ns = close_ns - 780_000_000_000;
        let mut strategy = LivelyMomentumTaker::new(LivelyMomentumTakerConfig {
            clip_usdc: 15.0,
            refresh_secs: 1.0,
            market_window_ns: window_ns,
            max_seconds_after_open: 700.0,
            ..LivelyMomentumTakerConfig::default()
        });
        let spot = SpotHistory::new(vec![
            SpotTick {
                ts_ns: close_ns - window_ns,
                price: 100.0,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns - 30_000_000_000,
                price: 100.02,
                quantity: 1.0,
                is_buyer_maker: false,
            },
            SpotTick {
                ts_ns: now_ns,
                price: 100.05,
                quantity: 1.0,
                is_buyer_maker: false,
            },
        ]);

        let out = strategy.on_event(
            &event(now_ns, 0.58, 0.60),
            &ctx(close_ns),
            &spot,
            &TradeHistory::default(),
        );

        assert_eq!(out.orders.len(), 1);
        assert_eq!(out.orders[0].side, Side::BuyYes);
    }
}
