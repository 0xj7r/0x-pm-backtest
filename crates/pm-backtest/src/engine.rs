//! The event-loop backtest engine: `run_backtest` steps a strategy through
//! replay events, matching orders against the maker/taker fill logic in
//! [`crate::fills`] and producing a [`crate::accounting::BacktestReport`].

use anyhow::Result;
use pm_model::{MetaFeatures, MetaTrainingSample, ModelConfig, ModelState, edge_vs_mid};
use pm_risk::PortfolioState;
use pm_strategy::regime::{WhipsawRiskSnapshot, classify_market_regime_cluster};
use pm_strategy::{Ctx, Side, Strategy};
use pm_types::{ReplayEvent, SpotHistory, TradeHistory};
use std::io::Write;

use crate::accounting::{BacktestReport, StrategyCounters, mark_to_market};
use crate::config::RunnerConfig;
use crate::fills::{
    BinanceFlowFeatures, DecisionLogRow, Fill, FillModelContext, PendingTakerOrder, RestingOrder,
    annotate_post_fill_paths, check_resting_fills, check_trade_driven_resting_fills,
    model_gate_edge_for_order, order_adds_yes_exposure, order_request_notional_usdc,
    order_requires_model_gate, process_pending_takers, submit_maker_order, submit_taker_order,
    write_decision_rows_parquet,
};

pub fn run_backtest<S: Strategy>(
    events: &[ReplayEvent],
    spot: &SpotHistory,
    trades: &TradeHistory,
    strategy: &mut S,
    cfg: &RunnerConfig,
) -> Result<BacktestReport> {
    let mut cash = cfg.starting_cash_usdc;
    let mut yes_shares = 0.0f64;
    let mut no_shares = 0.0f64;
    let mut counters = StrategyCounters::default();
    let mut fills: Vec<Fill> = Vec::new();
    let mut last_mid = 0.0f32;
    let mut total_rebates = 0.0f64;
    let mut total_requested_shares = 0.0f64;
    let mut total_requested_notional = 0.0f64;

    // Direction-grouped resting orders — major CPU win for grids that accumulate
    // resting orders. BuyYes/SellNo in one list, SellYes/BuyNo in the other.
    // All hot-path scans now touch roughly half as many elements.
    let mut resting_buy_yes_sell_no: Vec<RestingOrder> = Vec::new();
    let mut resting_sell_yes_buy_no: Vec<RestingOrder> = Vec::new();
    let mut pending_takers: Vec<PendingTakerOrder> = Vec::new();

    let mut trade_cursor = 0usize;
    let mut events_processed = 0usize;
    let mut market_yes_min = f32::INFINITY;
    let mut market_yes_max = f32::NEG_INFINITY;

    // Capacity hints for hot path on large days.
    resting_buy_yes_sell_no.reserve(1024);
    resting_sell_yes_buy_no.reserve(1024);
    pending_takers.reserve(128);
    fills.reserve(8192);
    let mut model_state = ModelState::new();
    if let Some(snapshot) = cfg.meta_calibrator_snapshot.clone() {
        model_state.load_meta_calibrator_snapshot(snapshot);
    }
    let model_cfg = ModelConfig {
        enable_meta_calibration: cfg.enable_meta_calibration,
        btc_whipsaw_risk_weight: cfg.model_btc_whipsaw_risk_weight,
        btc_path_inefficiency_risk_weight: cfg.model_btc_path_inefficiency_risk_weight,
        btc_reversal_pressure_risk_weight: cfg.model_btc_reversal_pressure_risk_weight,
        ..ModelConfig::default()
    };
    let market_open_ts_ns = if cfg.market_open_ns > 0 {
        cfg.market_open_ns
    } else if cfg.market_close_ns > 300_000_000_000 {
        cfg.market_close_ns - 300_000_000_000
    } else {
        events.first().map_or(0, |e| e.ts_ns).max(0)
    };

    let mut portfolio = PortfolioState::new(cfg.starting_cash_usdc, cfg.portfolio_limits.clone());
    portfolio.mark(cfg.starting_cash_usdc);

    let mut curve_file = match cfg.equity_curve_jsonl.as_deref() {
        Some(p) => Some(std::fs::File::create(p)?),
        None => None,
    };
    let mut decision_file = match cfg.decision_log_jsonl.as_deref() {
        Some(p) => Some(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)?,
        ),
        None => None,
    };
    let mut decision_rows = if cfg.decision_log_parquet.is_some() {
        Some(Vec::new())
    } else {
        None
    };
    let snap_every = cfg.snapshot_every_n.max(1);
    let decision_every = cfg.decision_log_every_n.max(1);

    let mut last_window_idx: isize = -1;
    let mut last_canonical_prediction_is_yes: Option<bool> = None;
    let mut last_canonical_sample_point: Option<(MetaFeatures, f32, bool)> = None;
    let mut last_meta_sample_bucket: Option<i64> = None;
    let mut canonical_meta_sample_points: Vec<(MetaFeatures, f32, bool)> = Vec::new();
    for (idx, event) in events.iter().enumerate() {
        if market_open_ts_ns > 0 && event.ts_ns < market_open_ts_ns {
            continue;
        }
        if cfg.market_close_ns > 0 && event.ts_ns > cfg.market_close_ns {
            break;
        }
        last_mid = event.yes_mid;
        market_yes_min = market_yes_min.min(event.yes_mid);
        market_yes_max = market_yes_max.max(event.yes_mid);
        let market_yes_range_so_far = if market_yes_min.is_finite() && market_yes_max.is_finite() {
            market_yes_max - market_yes_min
        } else {
            0.0
        };
        last_window_idx = idx as isize;
        events_processed += 1;

        // Fast path: skip expensive linear scans over resting orders when there are none
        // (very common case). This is a free, high-value win in the hot per-event loop
        // for large grids.
        // Pass only the relevant half-list (direction-grouped split).
        if !resting_buy_yes_sell_no.is_empty() {
            check_trade_driven_resting_fills(
                event,
                trades,
                &mut trade_cursor,
                &mut resting_buy_yes_sell_no,
                &mut cash,
                &mut yes_shares,
                &mut no_shares,
                &mut portfolio,
                &mut counters,
                &mut fills,
                &mut total_rebates,
                cfg.maker_rebate_bps,
            );
        }
        if !resting_sell_yes_buy_no.is_empty() {
            check_trade_driven_resting_fills(
                event,
                trades,
                &mut trade_cursor,
                &mut resting_sell_yes_buy_no,
                &mut cash,
                &mut yes_shares,
                &mut no_shares,
                &mut portfolio,
                &mut counters,
                &mut fills,
                &mut total_rebates,
                cfg.maker_rebate_bps,
            );
        }

        if !resting_buy_yes_sell_no.is_empty() {
            check_resting_fills(
                event,
                &mut resting_buy_yes_sell_no,
                &mut cash,
                &mut yes_shares,
                &mut no_shares,
                &mut portfolio,
                &mut counters,
                &mut fills,
                &mut total_rebates,
                cfg.maker_rebate_bps,
            );
        }
        if !resting_sell_yes_buy_no.is_empty() {
            check_resting_fills(
                event,
                &mut resting_sell_yes_buy_no,
                &mut cash,
                &mut yes_shares,
                &mut no_shares,
                &mut portfolio,
                &mut counters,
                &mut fills,
                &mut total_rebates,
                cfg.maker_rebate_bps,
            );
        }
        if !pending_takers.is_empty() {
            process_pending_takers(
                event,
                &mut pending_takers,
                &mut cash,
                &mut yes_shares,
                &mut no_shares,
                &mut portfolio,
                &mut counters,
                &mut fills,
                cfg.taker_fee_bps,
                cfg.taker_slippage_bps,
            );
        }

        // Inventory imbalance circuit-breaker (applied to both lists).
        let imbalance = yes_shares - no_shares;
        if imbalance.abs() > cfg.max_inventory_imbalance_shares {
            let heavy_long_yes = imbalance > 0.0;
            let retain_fn = |r: &RestingOrder| {
                let adds_to_heavy = match (heavy_long_yes, r.side) {
                    (true, Side::BuyYes) => true,
                    (false, Side::BuyNo) => true,
                    _ => false,
                };
                !adds_to_heavy
            };
            resting_buy_yes_sell_no.retain(retain_fn);
            resting_sell_yes_buy_no.retain(retain_fn);
        }

        let pre_cash = cash;
        let pre_yes_shares = yes_shares;
        let pre_no_shares = no_shares;
        let pre_fill_count = fills.len();
        let pre_mtm = mark_to_market(cash, yes_shares, no_shares, last_mid);
        let secs_since_open = ((event.ts_ns - market_open_ts_ns).max(0) as f64) / 1e9;
        let canonical_model_eval = if let Some(shared) = &cfg.shared_model_state {
            let mut state = shared.lock().expect("shared model mutex poisoned");
            state.evaluate_detailed_with_market_context(
                event,
                spot,
                secs_since_open as f32,
                &model_cfg,
                cfg.model_market_context,
            )
        } else {
            model_state.evaluate_detailed_with_market_context(
                event,
                spot,
                secs_since_open as f32,
                &model_cfg,
                cfg.model_market_context,
            )
        };
        let canonical_prediction_is_yes = canonical_model_eval.output.direction_score >= 0.0;
        last_canonical_prediction_is_yes = Some(canonical_prediction_is_yes);
        let canonical_sample_point = (
            canonical_model_eval.attribution.meta_features,
            canonical_model_eval.attribution.side_probability_pre_meta,
            canonical_prediction_is_yes,
        );
        let meta_sample_bucket = (secs_since_open / 15.0).floor() as i64;
        if last_meta_sample_bucket != Some(meta_sample_bucket) {
            canonical_meta_sample_points.push(canonical_sample_point);
            last_meta_sample_bucket = Some(meta_sample_bucket);
        }
        last_canonical_sample_point = Some(canonical_sample_point);
        let whipsaw_snapshot = if spot.is_empty() {
            WhipsawRiskSnapshot::default()
        } else {
            WhipsawRiskSnapshot::from_history(event.ts_ns, spot)
        };
        let ctx = Ctx {
            events_seen: events_processed as u64,
            yes_shares,
            no_shares,
            cash_usdc: cash,
            market_yes_range_so_far,
            regime_whipsaw_score: whipsaw_snapshot.score,
            regime_path_efficiency: whipsaw_snapshot.path_efficiency,
            regime_reversal_pressure: whipsaw_snapshot.reversal_pressure,
            regime_sign_flip_rate: whipsaw_snapshot.sign_flip_rate,
            regime_realized_vol_180s_bps: whipsaw_snapshot.realized_vol_180s_bps,
            prior_market_range_1d: cfg.prior_market_range_1d,
            prior_market_range_3d: cfg.prior_market_range_3d,
            prior_market_range_7d: cfg.prior_market_range_7d,
            model_output: Some(canonical_model_eval.output),
            model_attribution: Some(canonical_model_eval.attribution),
            market_close_ns: cfg.market_close_ns,
            // Ladder exposure populated at walk-forward level for portfolio runs.
            // For now zeroed here; real values come from higher-level asset aggregation
            // (see future cross-market accounting work for BackToExplore).
            btc_net_exposure_shares: cfg.current_btc_net_shares,
            eth_net_exposure_shares: cfg.current_eth_net_shares,
            daily_start_cash_usdc: cfg.daily_start_cash_usdc,
            daily_loss_cap_pct: cfg.daily_loss_cap_pct,
            current_daily_loss_pct: cfg.current_daily_loss_pct,
            no_bid: 0.0,
            no_ask: 0.0,
            no_mid: 0.0,
        };
        let (output, strategy_model_output) = strategy.on_event_scored(event, &ctx, spot, trades);
        let strategy_emitted_model_output = strategy_model_output.is_some();
        let model_output = strategy_model_output.unwrap_or(canonical_model_eval.output);
        let model_attribution = canonical_model_eval.attribution;
        let has_model_attribution = true;
        let edge = edge_vs_mid(&model_output, event.yes_mid);
        let direction_score = model_output.direction_score;
        let confidence_score = model_output.confidence_score;
        let calibrated_p = model_output.calibrated_p;
        let risk_score = model_output.risk_score;
        let has_model_output = true;
        let orders_requested = output.orders.len();
        let mut order_tags = Vec::with_capacity(orders_requested);
        for req in &output.orders {
            order_tags.push(req.tag.to_string());
        }
        let mut requested_shares = 0.0;
        let mut requested_notional = 0.0;

        for req in output.orders {
            let yes_side = order_adds_yes_exposure(req.side);
            if cfg.enforce_model_gate && order_requires_model_gate(req.tag) {
                let side_edge = model_gate_edge_for_order(&model_output, event, &req)
                    .unwrap_or_else(|| {
                        pm_model::side_edge_vs_mid(&model_output, event.yes_mid, yes_side)
                    });
                if model_output.confidence_score < cfg.model_gate_min_confidence {
                    counters.orders_rejected_model_gate += 1;
                    counters.orders_rejected_model_gate_confidence += 1;
                    continue;
                }
                if model_output.risk_score > cfg.model_gate_max_risk {
                    counters.orders_rejected_model_gate += 1;
                    counters.orders_rejected_model_gate_risk += 1;
                    continue;
                }
                if side_edge < cfg.model_gate_min_edge {
                    counters.orders_rejected_model_gate += 1;
                    counters.orders_rejected_model_gate_edge += 1;
                    continue;
                }
            }
            counters.orders_submitted += 1;
            requested_shares += req.shares;
            let req_notional = order_request_notional_usdc(req, event).unwrap_or(0.0);
            requested_notional += req_notional;
            total_requested_shares += req.shares;
            total_requested_notional += req_notional;
            let fill_context = Some(FillModelContext::from_event(
                event,
                &model_output,
                req.side,
                market_yes_range_so_far,
                secs_since_open as f32,
                cfg.market_close_ns,
                whipsaw_snapshot,
                spot,
            ));
            match req.limit_price {
                None => {
                    submit_taker_order(
                        event,
                        &req,
                        &mut cash,
                        &mut yes_shares,
                        &mut no_shares,
                        &mut portfolio,
                        &mut counters,
                        &mut fills,
                        cfg.taker_fee_bps,
                        cfg.taker_slippage_bps,
                        cfg.taker_latency_ms,
                        &mut pending_takers,
                        fill_context,
                    );
                }
                Some(limit) => {
                    // Classify here so submit_maker_order keeps a simple signature.
                    let target_resting = match req.side {
                        Side::BuyYes | Side::SellNo => &mut resting_buy_yes_sell_no,
                        Side::SellYes | Side::BuyNo => &mut resting_sell_yes_buy_no,
                    };
                    submit_maker_order(
                        event,
                        &req,
                        limit,
                        target_resting,
                        &mut cash,
                        &mut yes_shares,
                        &mut no_shares,
                        &mut portfolio,
                        &mut counters,
                        &mut fills,
                        &mut total_rebates,
                        cfg.maker_rebate_bps,
                        cfg.taker_fee_bps,
                        cfg.taker_slippage_bps,
                        cfg.taker_latency_ms,
                        &mut pending_takers,
                        fill_context,
                    );
                }
            }
        }

        counters.resting_orders_active =
            resting_buy_yes_sell_no.len() + resting_sell_yes_buy_no.len();
        let mtm = mark_to_market(cash, yes_shares, no_shares, last_mid);
        portfolio.mark(mtm);

        if let Some(f) = curve_file.as_mut() {
            if idx % snap_every == 0 {
                let snap = portfolio.snapshot(event.ts_ns, mtm);
                writeln!(f, "{}", serde_json::to_string(&snap)?)?;
            }
        }

        let event_fill_notional = fills
            .iter()
            .skip(pre_fill_count)
            .map(|f| f.notional)
            .sum::<f64>();
        let event_fill_count = fills.len() - pre_fill_count;
        let event_fills_window = &fills[pre_fill_count..];
        let (window_notional, slippage_notional) =
            event_fills_window
                .iter()
                .fold((0.0f64, 0.0f32), |(n, slip), fill| {
                    (
                        n + fill.notional,
                        slip + fill.slippage_bps * fill.notional as f32,
                    )
                });
        let event_slippage_bps = if window_notional > 0.0 {
            slippage_notional / window_notional as f32
        } else {
            0.0
        };
        let event_cash_delta = cash - pre_cash;
        let event_mtm_after = mark_to_market(cash, yes_shares, no_shares, last_mid);
        let event_mtm_delta = event_mtm_after - pre_mtm;
        let decision_side = if direction_score >= 0.0 {
            Side::BuyYes
        } else {
            Side::BuyNo
        };
        let decision_flow = BinanceFlowFeatures::compute(spot, event.ts_ns, decision_side);
        let decision_regime_cluster = classify_market_regime_cluster(
            market_yes_range_so_far,
            whipsaw_snapshot.path_efficiency,
            whipsaw_snapshot.reversal_pressure,
            whipsaw_snapshot.sign_flip_rate,
            whipsaw_snapshot.realized_vol_180s_bps,
            Some(decision_flow.adverse_vol_30s as f32),
        )
        .as_str()
        .to_string();
        if let Some(f) = decision_file.as_mut() {
            if idx % decision_every == 0 {
                let row = DecisionLogRow {
                    strategy: cfg.strategy_name.clone(),
                    market_id: event.market_id.0,
                    event_idx: (idx + 1) as u64,
                    ts_ns: event.ts_ns,
                    market_mid: last_mid,
                    yes_mid: event.yes_mid,
                    yes_bid: event.yes_bid,
                    yes_ask: event.yes_ask,
                    cash_usdc_before: pre_cash,
                    yes_shares_before: pre_yes_shares,
                    no_shares_before: pre_no_shares,
                    direction_score,
                    confidence_score,
                    calibrated_p,
                    risk_score,
                    market_yes_range_so_far,
                    seconds_since_open: secs_since_open as f32,
                    seconds_to_close: ((cfg.market_close_ns - event.ts_ns).max(0) as f32) / 1e9,
                    regime_whipsaw_score: whipsaw_snapshot.score,
                    regime_path_efficiency: whipsaw_snapshot.path_efficiency,
                    regime_reversal_pressure: whipsaw_snapshot.reversal_pressure,
                    regime_sign_flip_rate: whipsaw_snapshot.sign_flip_rate,
                    regime_realized_vol_180s_bps: whipsaw_snapshot.realized_vol_180s_bps,
                    regime_cluster: decision_regime_cluster.clone(),
                    binance_flow_imbal_30s: decision_flow.flow_imbal_30s,
                    binance_adverse_vol_30s: decision_flow.adverse_vol_30s,
                    prior_market_range_1d: cfg.prior_market_range_1d,
                    prior_market_range_3d: cfg.prior_market_range_3d,
                    prior_market_range_7d: cfg.prior_market_range_7d,
                    feature_observed_yes_range_so_far: model_attribution.observed_yes_range_so_far,
                    feature_observed_range_high_cert_interaction: model_attribution
                        .observed_range_high_cert_interaction,
                    edge,
                    has_model_output,
                    strategy_emitted_model_output,
                    has_model_attribution,
                    side_is_yes: direction_score >= 0.0,
                    feature_momentum: model_attribution.direction.momentum,
                    feature_book_imbalance_top3: model_attribution.book_imbalance_top3,
                    feature_microprice_dev: model_attribution.direction.microprice_dev,
                    feature_microprice_spot_alignment: model_attribution
                        .direction
                        .microprice_spot_alignment,
                    feature_top3_delta_5s: model_attribution.direction.top3_delta_5s,
                    feature_top3_delta_15s: model_attribution.direction.top3_delta_15s,
                    feature_spot_score: model_attribution.spot_score,
                    feature_spot_fast_momentum: model_attribution.direction.spot_fast_momentum,
                    feature_spot_broad_momentum: model_attribution.direction.spot_broad_momentum,
                    feature_spot_momentum_600s: model_attribution.direction.spot_momentum_600s,
                    feature_spot_momentum_900s: model_attribution.direction.spot_momentum_900s,
                    feature_spot_momentum_1800s: model_attribution.direction.spot_momentum_1800s,
                    feature_spot_momentum_3600s: model_attribution.direction.spot_momentum_3600s,
                    feature_spot_momentum_7200s: model_attribution.direction.spot_momentum_7200s,
                    feature_spot_momentum_14400s: model_attribution.direction.spot_momentum_14400s,
                    feature_spot_1h_4h_alignment: model_attribution.direction.spot_1h_4h_alignment,
                    feature_spot_ultra_trend_consistency: model_attribution
                        .direction
                        .spot_ultra_trend_consistency,
                    feature_spot_ultra_acceleration: model_attribution
                        .direction
                        .spot_ultra_acceleration,
                    feature_spot_fast_long_alignment: model_attribution
                        .direction
                        .spot_fast_long_alignment,
                    feature_spot_broad_trend_consistency: model_attribution
                        .direction
                        .spot_broad_trend_consistency,
                    feature_spot_broad_acceleration: model_attribution
                        .direction
                        .spot_broad_acceleration,
                    feature_direction_raw: model_attribution.direction_raw,
                    feature_stability: model_attribution.confidence.stability,
                    feature_sign_persistence: model_attribution.confidence.sign_persistence,
                    feature_markov_persistence: model_attribution.confidence.markov_persistence,
                    feature_early_market_penalty: model_attribution.confidence.early_market_penalty,
                    feature_time_of_day_edge: model_attribution.time_of_day_edge,
                    feature_time_of_day_advantage: model_attribution
                        .confidence
                        .time_of_day_advantage,
                    feature_whipsaw: model_attribution.risk.whipsaw,
                    feature_liquidity: model_attribution.risk.liquidity,
                    feature_path_risk: model_attribution.risk.path_risk,
                    feature_imbalance_turn: model_attribution.risk.imbalance_turn,
                    feature_markov_reversal_risk: model_attribution.risk.markov_reversal_risk,
                    feature_skew_penalty: model_attribution.risk.skew_penalty,
                    feature_volatility_penalty: model_attribution.risk.volatility_penalty,
                    feature_time_of_day_penalty: model_attribution.risk.time_of_day_penalty,
                    feature_volatility_regime: model_attribution.volatility_regime,
                    feature_dir_flip_rate_8: model_attribution.sequence.dir_flip_rate_8,
                    feature_dir_std_8: model_attribution.sequence.dir_std_8,
                    feature_dir_abs_mean_8: model_attribution.sequence.dir_abs_mean_8,
                    feature_side_p_pre_meta: model_attribution.side_probability_pre_meta,
                    feature_side_p_post_meta: model_attribution.side_probability_post_meta,
                    meta_calibrator_updates: model_attribution.meta_calibrator_updates,
                    orders_requested,
                    requested_shares,
                    requested_notional_usdc: requested_notional,
                    order_tags,
                    event_fill_notional_usdc: event_fill_notional,
                    event_fills: event_fill_count,
                    event_slippage_bps,
                    event_cash_delta_usdc: event_cash_delta,
                    event_mtm_delta_usdc: event_mtm_delta,
                };
                if let Some(rows) = decision_rows.as_mut() {
                    rows.push(row.clone());
                }
                writeln!(f, "{}", serde_json::to_string(&row)?)?;
            }
        } else if let Some(rows) = decision_rows.as_mut() {
            if idx % decision_every == 0 {
                rows.push(DecisionLogRow {
                    strategy: cfg.strategy_name.clone(),
                    market_id: event.market_id.0,
                    event_idx: (idx + 1) as u64,
                    ts_ns: event.ts_ns,
                    market_mid: last_mid,
                    yes_mid: event.yes_mid,
                    yes_bid: event.yes_bid,
                    yes_ask: event.yes_ask,
                    cash_usdc_before: pre_cash,
                    yes_shares_before: pre_yes_shares,
                    no_shares_before: pre_no_shares,
                    direction_score,
                    confidence_score,
                    calibrated_p,
                    risk_score,
                    market_yes_range_so_far,
                    seconds_since_open: secs_since_open as f32,
                    seconds_to_close: ((cfg.market_close_ns - event.ts_ns).max(0) as f32) / 1e9,
                    regime_whipsaw_score: whipsaw_snapshot.score,
                    regime_path_efficiency: whipsaw_snapshot.path_efficiency,
                    regime_reversal_pressure: whipsaw_snapshot.reversal_pressure,
                    regime_sign_flip_rate: whipsaw_snapshot.sign_flip_rate,
                    regime_realized_vol_180s_bps: whipsaw_snapshot.realized_vol_180s_bps,
                    regime_cluster: decision_regime_cluster,
                    binance_flow_imbal_30s: decision_flow.flow_imbal_30s,
                    binance_adverse_vol_30s: decision_flow.adverse_vol_30s,
                    prior_market_range_1d: cfg.prior_market_range_1d,
                    prior_market_range_3d: cfg.prior_market_range_3d,
                    prior_market_range_7d: cfg.prior_market_range_7d,
                    feature_observed_yes_range_so_far: model_attribution.observed_yes_range_so_far,
                    feature_observed_range_high_cert_interaction: model_attribution
                        .observed_range_high_cert_interaction,
                    edge,
                    has_model_output,
                    strategy_emitted_model_output,
                    has_model_attribution,
                    side_is_yes: direction_score >= 0.0,
                    feature_momentum: model_attribution.direction.momentum,
                    feature_book_imbalance_top3: model_attribution.book_imbalance_top3,
                    feature_microprice_dev: model_attribution.direction.microprice_dev,
                    feature_microprice_spot_alignment: model_attribution
                        .direction
                        .microprice_spot_alignment,
                    feature_top3_delta_5s: model_attribution.direction.top3_delta_5s,
                    feature_top3_delta_15s: model_attribution.direction.top3_delta_15s,
                    feature_spot_score: model_attribution.spot_score,
                    feature_spot_fast_momentum: model_attribution.direction.spot_fast_momentum,
                    feature_spot_broad_momentum: model_attribution.direction.spot_broad_momentum,
                    feature_spot_momentum_600s: model_attribution.direction.spot_momentum_600s,
                    feature_spot_momentum_900s: model_attribution.direction.spot_momentum_900s,
                    feature_spot_momentum_1800s: model_attribution.direction.spot_momentum_1800s,
                    feature_spot_momentum_3600s: model_attribution.direction.spot_momentum_3600s,
                    feature_spot_momentum_7200s: model_attribution.direction.spot_momentum_7200s,
                    feature_spot_momentum_14400s: model_attribution.direction.spot_momentum_14400s,
                    feature_spot_1h_4h_alignment: model_attribution.direction.spot_1h_4h_alignment,
                    feature_spot_ultra_trend_consistency: model_attribution
                        .direction
                        .spot_ultra_trend_consistency,
                    feature_spot_ultra_acceleration: model_attribution
                        .direction
                        .spot_ultra_acceleration,
                    feature_spot_fast_long_alignment: model_attribution
                        .direction
                        .spot_fast_long_alignment,
                    feature_spot_broad_trend_consistency: model_attribution
                        .direction
                        .spot_broad_trend_consistency,
                    feature_spot_broad_acceleration: model_attribution
                        .direction
                        .spot_broad_acceleration,
                    feature_direction_raw: model_attribution.direction_raw,
                    feature_stability: model_attribution.confidence.stability,
                    feature_sign_persistence: model_attribution.confidence.sign_persistence,
                    feature_markov_persistence: model_attribution.confidence.markov_persistence,
                    feature_early_market_penalty: model_attribution.confidence.early_market_penalty,
                    feature_time_of_day_edge: model_attribution.time_of_day_edge,
                    feature_time_of_day_advantage: model_attribution
                        .confidence
                        .time_of_day_advantage,
                    feature_whipsaw: model_attribution.risk.whipsaw,
                    feature_liquidity: model_attribution.risk.liquidity,
                    feature_path_risk: model_attribution.risk.path_risk,
                    feature_imbalance_turn: model_attribution.risk.imbalance_turn,
                    feature_markov_reversal_risk: model_attribution.risk.markov_reversal_risk,
                    feature_skew_penalty: model_attribution.risk.skew_penalty,
                    feature_volatility_penalty: model_attribution.risk.volatility_penalty,
                    feature_time_of_day_penalty: model_attribution.risk.time_of_day_penalty,
                    feature_volatility_regime: model_attribution.volatility_regime,
                    feature_dir_flip_rate_8: model_attribution.sequence.dir_flip_rate_8,
                    feature_dir_std_8: model_attribution.sequence.dir_std_8,
                    feature_dir_abs_mean_8: model_attribution.sequence.dir_abs_mean_8,
                    feature_side_p_pre_meta: model_attribution.side_probability_pre_meta,
                    feature_side_p_post_meta: model_attribution.side_probability_post_meta,
                    meta_calibrator_updates: model_attribution.meta_calibrator_updates,
                    orders_requested,
                    requested_shares,
                    requested_notional_usdc: requested_notional,
                    order_tags,
                    event_fill_notional_usdc: event_fill_notional,
                    event_fills: event_fill_count,
                    event_slippage_bps,
                    event_cash_delta_usdc: event_cash_delta,
                    event_mtm_delta_usdc: event_mtm_delta,
                });
            }
        }
    }

    if last_window_idx < 0 {
        last_mid = events.last().map_or(0.0, |e| e.yes_mid);
    }

    counters.resting_orders_cancelled_eom =
        resting_buy_yes_sell_no.len() + resting_sell_yes_buy_no.len();
    counters.orders_rejected_no_liquidity += pending_takers.len();
    resting_buy_yes_sell_no.clear();
    resting_sell_yes_buy_no.clear();
    pending_takers.clear();

    let yes_resolved = cfg.resolved_yes.unwrap_or(last_mid >= 0.5);
    annotate_post_fill_paths(&mut fills, events, cfg.market_close_ns);
    let settlement_cash = if yes_resolved { yes_shares } else { no_shares };
    let end_cash = cash + settlement_cash;
    let filled_shares = fills.iter().map(|f| f.shares).sum::<f64>();
    let filled_notional_usdc = fills.iter().map(|f| f.notional).sum::<f64>();
    portfolio.mark(end_cash);

    let final_ts_ns = if last_window_idx >= 0 {
        events[last_window_idx as usize].ts_ns
    } else {
        events.last().map(|e| e.ts_ns).unwrap_or(0)
    };
    let final_snapshot = portfolio.snapshot(final_ts_ns, end_cash);
    let peak_equity_usdc = final_snapshot.peak_equity_usdc;
    let max_drawdown_pct = if peak_equity_usdc > 0.0 {
        1.0 - end_cash / peak_equity_usdc
    } else {
        0.0
    }
    .max(final_snapshot.drawdown_pct);

    strategy.on_market_resolved(last_mid, yes_resolved);
    if let Some(last) = last_canonical_sample_point {
        if canonical_meta_sample_points.last().copied() != Some(last) {
            canonical_meta_sample_points.push(last);
        }
    }
    let model_training_samples = canonical_meta_sample_points
        .into_iter()
        .map(
            |(features, base_side_probability, predicted_yes)| MetaTrainingSample {
                features,
                market_idx: 0,
                base_side_probability,
                side_observed: if predicted_yes {
                    yes_resolved
                } else {
                    !yes_resolved
                },
            },
        )
        .collect();
    if cfg.update_model_state_on_resolution {
        if let Some(predicted_yes) = last_canonical_prediction_is_yes {
            if let Some(shared) = &cfg.shared_model_state {
                let mut state = shared.lock().expect("shared model mutex poisoned");
                state.record_market_result(last_mid, predicted_yes, yes_resolved);
            } else {
                model_state.record_market_result(last_mid, predicted_yes, yes_resolved);
            }
        }
    }

    if let (Some(path), Some(rows)) = (cfg.decision_log_parquet.as_deref(), decision_rows.as_ref())
    {
        write_decision_rows_parquet(path, rows)?;
    }

    Ok(BacktestReport {
        events_processed,
        counters,
        start_equity_usdc: cfg.starting_cash_usdc,
        end_equity_usdc: end_cash,
        pnl_usdc: end_cash - cfg.starting_cash_usdc,
        maker_rebates_usdc: total_rebates,
        peak_equity_usdc,
        max_drawdown_pct,
        final_yes_shares: yes_shares,
        final_no_shares: no_shares,
        final_cash_usdc: cash,
        requested_shares: total_requested_shares,
        filled_shares,
        requested_notional_usdc: total_requested_notional,
        filled_notional_usdc,
        yes_resolved,
        last_yes_mid: last_mid,
        fills,
        final_portfolio: final_snapshot,
        model_training_samples,
    })
}
