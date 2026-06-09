use std::path::Path;

use anyhow::{Context, Result};
use pm_model::{ModelConfig, ModelMarketContext, ModelState, OnlineMetaCalibratorSnapshot};
use pm_strategy::Ctx;
use pm_types::{ReplayEvent, SpotHistory};
use pm_strategy::regime::WhipsawRiskSnapshot;

/// Prior-market YES-mid range statistics computed by the driver for the current
/// market, covering 1-day, 3-day, and 7-day trailing windows. The driver
/// derives these (porting `prior_market_range_mean` from walkforward.rs:1614)
/// and passes them in so the enricher stays pure.
#[derive(Debug, Clone, Copy, Default)]
pub struct PriorRanges {
    pub d1: f32,
    pub d3: f32,
    pub d7: f32,
}

/// Shared context enricher. Called by the engine (when present) after
/// `build_ctx` to fill the regime, model, and prior-range `Ctx` fields before
/// passing the context to `on_event_scored`.
///
/// `new_without_model()` is used until `with_model_snapshot` is called (Task 6).
/// Phase-1 integration tests that never attach an enricher are unaffected.
pub struct CtxEnricher {
    model: Option<(ModelState, ModelConfig, ModelMarketContext)>,
}

impl CtxEnricher {
    /// Create an enricher that fills regime + prior-range only (no model eval).
    pub fn new_without_model() -> Self {
        Self { model: None }
    }

    /// Create an enricher that also evaluates the model from a frozen snapshot.
    ///
    /// Ports `read_meta_snapshot` from `walkforward.rs:3440-3449` and
    /// `ModelState::load_meta_calibrator_snapshot`. Both `model_cfg` and
    /// `market_context` are driver-supplied; callers must pass the values that
    /// match the champion being replayed. The BTC-5m champion uses
    /// `ModelMarketContext::default()` (market-context features disabled).
    pub fn with_model_snapshot(
        path: impl AsRef<Path>,
        model_cfg: ModelConfig,
        market_context: ModelMarketContext,
    ) -> Result<Self> {
        let path = path.as_ref();
        let file = std::fs::File::open(path)
            .with_context(|| format!("open meta-calibrator snapshot {}", path.display()))?;
        let snapshot: OnlineMetaCalibratorSnapshot =
            serde_json::from_reader(std::io::BufReader::new(file))
                .with_context(|| format!("parse meta-calibrator snapshot {}", path.display()))?;
        let mut model_state = ModelState::new();
        model_state.load_meta_calibrator_snapshot(snapshot);
        Ok(Self {
            model: Some((model_state, model_cfg, market_context)),
        })
    }

    /// Populate the five `regime_*` fields on `ctx`.
    ///
    /// Ports `WhipsawRiskSnapshot::from_history` (regime.rs:64) and the
    /// field assignments from runner.rs:772-776. When `spot` is empty the
    /// snapshot falls back to its `Default` (all zeros).
    pub fn fill_regime(&self, ctx: &mut Ctx, ts_ns: i64, spot: &SpotHistory) {
        let snap = if spot.is_empty() {
            WhipsawRiskSnapshot::default()
        } else {
            WhipsawRiskSnapshot::from_history(ts_ns, spot)
        };
        ctx.regime_whipsaw_score = snap.score;
        ctx.regime_path_efficiency = snap.path_efficiency;
        ctx.regime_reversal_pressure = snap.reversal_pressure;
        ctx.regime_sign_flip_rate = snap.sign_flip_rate;
        ctx.regime_realized_vol_180s_bps = snap.realized_vol_180s_bps;
    }

    /// Populate `prior_market_range_{1d,3d,7d}` from driver-supplied values.
    pub fn fill_prior_range(&self, ctx: &mut Ctx, ranges: PriorRanges) {
        ctx.prior_market_range_1d = ranges.d1;
        ctx.prior_market_range_3d = ranges.d3;
        ctx.prior_market_range_7d = ranges.d7;
    }

    /// Evaluate the model and populate `ctx.model_output` + `ctx.model_attribution`.
    ///
    /// Ports `ModelState::evaluate_detailed_with_market_context` call from
    /// runner.rs:740-746. No-op if no model snapshot was loaded.
    pub fn fill_model(
        &mut self,
        ctx: &mut Ctx,
        event: &ReplayEvent,
        _ts_ns: i64,
        secs_since_open: i64,
        spot: &SpotHistory,
    ) {
        let Some((model_state, model_cfg, market_context)) = &mut self.model else {
            return;
        };
        let eval = model_state.evaluate_detailed_with_market_context(
            event,
            spot,
            secs_since_open as f32,
            model_cfg,
            *market_context,
        );
        ctx.model_output = Some(eval.output);
        ctx.model_attribution = Some(eval.attribution);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_model::{ModelConfig, ModelMarketContext};
    use pm_types::{BookLevel, ReplayEvent, ReplayFlags, SpotHistory, SpotTick};

    fn spot_tick(ts_ns: i64, price: f64) -> SpotTick {
        SpotTick {
            ts_ns,
            price,
            quantity: 1.0,
            is_buyer_maker: false,
        }
    }

    fn make_spot(n: usize) -> SpotHistory {
        let ticks: Vec<SpotTick> = (0..n as i64)
            .map(|i| spot_tick(i * 3_000_000_000, 100.0 + (i as f64 * 0.3).sin() * 2.0))
            .collect();
        SpotHistory::new(ticks)
    }

    fn make_replay_event() -> ReplayEvent {
        use pm_types::MarketId;
        let mut asks = [BookLevel::default(); 5];
        asks[0] = BookLevel { price: 0.55, size: 500.0 };
        let mut bids = [BookLevel::default(); 5];
        bids[0] = BookLevel { price: 0.45, size: 500.0 };
        ReplayEvent {
            ts_ns: 60_000_000_000,
            market_id: MarketId(0),
            yes_mid: 0.50,
            yes_bid: 0.45,
            yes_ask: 0.55,
            volume: 1000.0,
            bids,
            asks,
            spot_price: 100.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    #[test]
    fn regime_scores_are_populated_and_live_safe() {
        // Build a SpotHistory with a non-trivial up-then-down path over 60 seconds
        // so WhipsawRiskSnapshot has enough samples (>=12 required in 180s window).
        let ticks: Vec<SpotTick> = (0..60i64)
            .map(|i| {
                let price = 100.0 + (i as f64 * 0.3).sin() * 2.0;
                spot_tick(i * 3_000_000_000, price)
            })
            .collect();
        let spot = SpotHistory::new(ticks);
        let ts_ns = 59 * 3_000_000_000i64;

        let enr = CtxEnricher::new_without_model();
        let mut ctx = pm_strategy::Ctx::default();
        enr.fill_regime(&mut ctx, ts_ns, &spot);

        // Scores are within valid ranges on a non-trivial path.
        assert!(
            ctx.regime_realized_vol_180s_bps >= 0.0,
            "realized_vol should be non-negative, got {}",
            ctx.regime_realized_vol_180s_bps
        );
        assert!(
            ctx.regime_path_efficiency >= 0.0 && ctx.regime_path_efficiency <= 1.0,
            "path_efficiency must be in [0,1], got {}",
            ctx.regime_path_efficiency
        );
        assert!(
            ctx.regime_whipsaw_score >= 0.0 && ctx.regime_whipsaw_score <= 1.0,
            "whipsaw_score must be in [0,1], got {}",
            ctx.regime_whipsaw_score
        );
    }

    #[test]
    fn regime_defaults_on_empty_spot() {
        let spot = SpotHistory::default();
        let enr = CtxEnricher::new_without_model();
        let mut ctx = pm_strategy::Ctx::default();
        enr.fill_regime(&mut ctx, 0, &spot);
        assert_eq!(ctx.regime_whipsaw_score, 0.0);
        assert_eq!(ctx.regime_path_efficiency, 0.0);
    }

    #[test]
    fn prior_range_fills_ctx_fields() {
        let enr = CtxEnricher::new_without_model();
        let mut ctx = pm_strategy::Ctx::default();
        enr.fill_prior_range(&mut ctx, PriorRanges { d1: 0.05, d3: 0.08, d7: 0.12 });
        assert!((ctx.prior_market_range_1d - 0.05).abs() < 1e-6);
        assert!((ctx.prior_market_range_3d - 0.08).abs() < 1e-6);
        assert!((ctx.prior_market_range_7d - 0.12).abs() < 1e-6);
    }

    #[test]
    fn model_eval_populates_model_output_from_snapshot() {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/snap_test.json"
        );
        let mut enr = CtxEnricher::with_model_snapshot(
            fixture,
            ModelConfig::default(),
            ModelMarketContext::default(),
        )
        .expect("load snapshot fixture");
        let spot = make_spot(60);
        let event = make_replay_event();
        let mut ctx = pm_strategy::Ctx::default();
        enr.fill_model(&mut ctx, &event, event.ts_ns, 30, &spot);
        assert!(
            ctx.model_output.is_some(),
            "model_output should be Some after fill_model"
        );
        assert!(
            ctx.model_attribution.is_some(),
            "model_attribution should be Some after fill_model"
        );
    }

    /// Contract test: market_context flows through to model attribution.
    ///
    /// Loads the same fixture into two enrichers — one with
    /// `ModelMarketContext::default()` (champion setting, Unknown/0s) and one
    /// with `ModelMarketContext::btc_5m()` (BTC/300s). The 9 market-context
    /// slots in `meta_features.values` must differ between the two, proving the
    /// param flows all the way through `fill_model`.
    ///
    /// `calibrated_p` is NOT asserted here: with an untrained fixture snapshot
    /// the meta-calibrator returns the same floor for any feature vector.
    /// Instead we assert on `META_FEATURE_NAMES[75] = "market_is_btc"`, which is
    /// deterministically 0.0 for Unknown and 1.0 for BTC regardless of training.
    #[test]
    fn market_context_param_flows_through_to_model_output() {
        use pm_model::META_FEATURE_NAMES;

        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/snap_test.json"
        );
        let spot = make_spot(60);
        let event = make_replay_event();

        let mut enr_default = CtxEnricher::with_model_snapshot(
            fixture,
            ModelConfig::default(),
            ModelMarketContext::default(),
        )
        .expect("load snapshot fixture (default context)");
        let mut ctx_default = pm_strategy::Ctx::default();
        enr_default.fill_model(&mut ctx_default, &event, event.ts_ns, 30, &spot);

        let mut enr_btc5m = CtxEnricher::with_model_snapshot(
            fixture,
            ModelConfig::default(),
            ModelMarketContext::btc_5m(),
        )
        .expect("load snapshot fixture (btc_5m context)");
        let mut ctx_btc5m = pm_strategy::Ctx::default();
        enr_btc5m.fill_model(&mut ctx_btc5m, &event, event.ts_ns, 30, &spot);

        let attr_default = ctx_default
            .model_attribution
            .expect("default context must produce model_attribution");
        let attr_btc5m = ctx_btc5m
            .model_attribution
            .expect("btc_5m context must produce model_attribution");

        // Locate the "market_is_btc" feature by name to make the index robust.
        let market_is_btc_idx = META_FEATURE_NAMES
            .iter()
            .position(|&n| n == "market_is_btc")
            .expect("market_is_btc must exist in META_FEATURE_NAMES");

        let val_default = attr_default.meta_features.values[market_is_btc_idx];
        let val_btc5m = attr_btc5m.meta_features.values[market_is_btc_idx];

        assert!(
            (val_default - val_btc5m).abs() > 1e-6,
            "meta_features[market_is_btc] must differ: default={val_default} btc_5m={val_btc5m}; \
             market_context param is not flowing through fill_model",
        );
        assert_eq!(
            val_default, 0.0,
            "default context (Unknown asset) must produce market_is_btc=0.0, got {val_default}"
        );
        assert_eq!(
            val_btc5m, 1.0,
            "btc_5m context must produce market_is_btc=1.0, got {val_btc5m}"
        );
    }

    #[test]
    fn fill_model_noop_without_snapshot() {
        let mut enr = CtxEnricher::new_without_model();
        let spot = make_spot(10);
        let event = make_replay_event();
        let mut ctx = pm_strategy::Ctx::default();
        enr.fill_model(&mut ctx, &event, event.ts_ns, 30, &spot);
        assert!(ctx.model_output.is_none(), "should be None when no model loaded");
    }
}
