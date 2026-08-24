//! The event-loop backtest engine: `run_backtest` steps a strategy through
//! replay events, matching orders against the maker/taker fill logic in
//! [`crate::fills`] and producing a [`crate::accounting::BacktestReport`].

use anyhow::{Context, Result, anyhow};
use futures::StreamExt;
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

use pm_model::{MetaTrainingConfig, MetaTrainingStats, ModelMarketContext, OnlineMetaCalibrator, OnlineMetaCalibratorSnapshot};
use pm_risk::PortfolioLimits;
use pm_strategy::{ExoFadeStrategy, NoopStrategy, exo_fade::ExoFadeConfig};
use pm_telonex_loader::{
    Channel, TelonexStore, load_book_snapshot_async, load_pm_trades_async, resolve_pm_trades_day,
};
use pm_alpha::PerpState;
use pm_types::MarketId;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::accounting::{MarketResult, StrategyMarketResult, market_close_ns, market_close_ts, market_duration_secs_from_slug, market_open_ns, outcome_label_resolved_yes, prior_market_range_mean};
use crate::config::{MarketHandle, WalkForwardConfig, spot_symbol_for_market};
use crate::portfolio::{LossStreakCooldownState, SpotCache, compounded_clip, daily_remaining_loss_budget_usdc, drawdown_clip_multiplier, load_walkforward_perp, market_volatility_range, model_market_context_for_cfg, model_market_context_for_slug, per_market_exposure_cap, spot_history_for_market, volatility_band};
use crate::scorecard::{CalibrationBin, CalibrationBinAccumulator, MarketCalibrationAccumulator, MetaCalibrationReport, MetaCandidateEvaluation, MetaEvaluationSummary, PredictionDistribution, WalkForwardFoldSummary, WalkForwardSummary, aggregate, binary_log_loss, meta_calibration_report, prediction_distribution, summary_run_config, write_portfolio_checkpoint};
use crate::fingerprint::config_fingerprint;
use crate::jitter::{build_jitter_report, jittered_run_latencies};

pub const DEFAULT_META_MAX_FIT_SAMPLES: usize = 120_000;

pub const DEFAULT_META_MAX_VALIDATION_SAMPLES: usize = 60_000;

pub const DEFAULT_META_MAX_OOS_EVALUATION_SAMPLES: usize = 120_000;

pub const DEFAULT_META_MAX_SAMPLES_PER_MARKET: usize = 64;


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum StratId {
    ExoFade,
    MayJuneFade,
    Noop,
}


impl StratId {
    pub const ACTIVE: [Self; 3] = [Self::ExoFade, Self::MayJuneFade, Self::Noop];

    pub const ALL: [Self; 3] = [Self::ExoFade, Self::MayJuneFade, Self::Noop];

    pub fn from_name(value: &str) -> Option<Self> {
        match value {
            "exo_fade" => Some(Self::ExoFade),
            "mayjune_fade" => Some(Self::MayJuneFade),
            "noop" => Some(Self::Noop),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            StratId::ExoFade => "exo_fade",
            StratId::MayJuneFade => "mayjune_fade",
            StratId::Noop => "noop",
        }
    }

    pub fn all_names() -> Vec<&'static str> {
        Self::ALL.iter().map(|strat| strat.name()).collect()
    }
}


fn sample_replay_events(
    events: &[pm_types::ReplayEvent],
    sample_ms: u64,
) -> Vec<pm_types::ReplayEvent> {
    if sample_ms == 0 || events.len() <= 2 {
        return events.to_vec();
    }
    let sample_ns = (sample_ms as i64).saturating_mul(1_000_000).max(1);
    let mut sampled = Vec::with_capacity(events.len().min(320));
    sampled.push(events[0]);

    let first_ns = events[0].ts_ns;
    let mut current_bucket = None::<i64>;
    let mut pending = None::<pm_types::ReplayEvent>;
    for event in events.iter().skip(1).take(events.len().saturating_sub(2)) {
        let bucket = event.ts_ns.saturating_sub(first_ns) / sample_ns;
        if current_bucket != Some(bucket) {
            if let Some(previous) = pending.take() {
                sampled.push(previous);
            }
            current_bucket = Some(bucket);
        }
        pending = Some(*event);
    }
    if let Some(previous) = pending {
        if sampled
            .last()
            .is_none_or(|last| last.ts_ns != previous.ts_ns)
        {
            sampled.push(previous);
        }
    }

    let last = *events.last().expect("events length checked");
    if sampled.last().is_none_or(|event| event.ts_ns != last.ts_ns) {
        sampled.push(last);
    }
    sampled
}


fn read_replay_event_cache(path: &Path) -> Result<Vec<ReplayEvent>> {
    let file =
        File::open(path).with_context(|| format!("open replay event cache {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line =
            line.with_context(|| format!("read line {} from {}", idx + 1, path.display()))?;
        if line.trim().is_empty() {
            continue;
        }
        let event: ReplayEvent = serde_json::from_str(&line)
            .with_context(|| format!("decode line {} from {}", idx + 1, path.display()))?;
        events.push(event);
    }
    Ok(events)
}


fn write_replay_event_cache(path: &Path, events: &[ReplayEvent]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create replay event cache dir {}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    let file = File::create(&tmp)
        .with_context(|| format!("create replay event cache {}", tmp.display()))?;
    let mut writer = BufWriter::new(file);
    for event in events {
        writeln!(writer, "{}", serde_json::to_string(event)?)
            .with_context(|| format!("write replay event cache {}", tmp.display()))?;
    }
    writer
        .flush()
        .with_context(|| format!("flush replay event cache {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} to {}", tmp.display(), path.display()))?;
    Ok(())
}


fn rebind_replay_event_market_ids(events: &mut [ReplayEvent], market_id: MarketId) {
    for event in events {
        event.market_id = market_id;
    }
}


fn replay_event_cache_path(cache_dir: Option<&Path>, market: &MarketHandle) -> Option<PathBuf> {
    cache_dir.map(|dir| {
        dir.join(&market.date)
            .join(format!("{}.jsonl", market.asset_id))
    })
}


pub async fn load_replay_events_for_market(
    store: &TelonexStore,
    store_inner: Arc<dyn object_store::ObjectStore>,
    market: &MarketHandle,
    market_id: MarketId,
    cache_dir: Option<&Path>,
) -> Result<Vec<ReplayEvent>> {
    let cache_path = replay_event_cache_path(cache_dir, market);
    if let Some(ref path) = cache_path {
        match read_replay_event_cache(path) {
            Ok(mut events) if !events.is_empty() => {
                rebind_replay_event_market_ids(&mut events, market_id);
                tracing::info!(
                    market = %market.slug,
                    n = events.len(),
                    "replay events from disk cache"
                );
                return Ok(events);
            }
            Ok(_) => {}
            Err(err) if path.exists() => {
                tracing::warn!(
                    market = %market.slug,
                    cache = %path.display(),
                    error = %err,
                    "replay event cache read failed; falling back to source tape"
                );
            }
            Err(_) => {}
        }
    }

    let source_path = store
        .resolve_asset_day(
            "polymarket",
            Channel::BookSnapshot25,
            &market.date,
            &market.asset_id,
        )
        .await
        .with_context(|| format!("resolve tape path for {}", market.slug))?;
    let (events, _stats) = load_book_snapshot_async(store_inner, source_path, market_id)
        .await
        .with_context(|| format!("load tape for {}", market.slug))?;
    if let Some(ref path) = cache_path {
        if let Err(err) = write_replay_event_cache(path, &events) {
            tracing::warn!(
                market = %market.slug,
                cache = %path.display(),
                error = %err,
                "replay event cache write failed"
            );
        }
    }
    Ok(events)
}


pub async fn run_walkforward(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
) -> Result<(Vec<MarketResult>, WalkForwardSummary)> {
    // Jittered replay: when --jitter N is set, run the full walk-forward N
    // times at seeded perturbed latencies and report the P&L spread instead of
    // a point estimate. Dispatches to run_jittered, which calls back into
    // run_walkforward with jitter=0 (so there is no recursion). N runs are
    // serial; a 288-market day at N=5 is ~5x the single-run wall time.
    if cfg.jitter > 0 {
        return run_jittered(store, markets, cfg).await;
    }
    // Always sort by normalized close so portfolio mode is well-defined and
    // parallel mode logs read sensibly. Some older local market lists stored
    // the slug/open timestamp in `close_ts`; normalize those to open + 300s.
    let mut markets_sorted: Vec<MarketHandle> = markets.to_vec();
    markets_sorted.sort_by_key(market_close_ts);
    let markets = &markets_sorted[..];

    let mut spot_cache = SpotCache::default();
    // Preload all distinct spot symbol/date pairs up front to amortize downloads.
    // Use `--spot-symbol auto` for mixed BTC/ETH market lists.
    let unique_spot_days: Vec<(String, String)> = markets
        .iter()
        .map(|m| {
            spot_symbol_for_market(&cfg.spot_symbol, &m.slug)
                .map(|symbol| symbol.map(|symbol| (symbol, m.date.clone())))
        })
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    for (symbol, date) in &unique_spot_days {
        spot_cache
            .get_or_load(store, symbol, date)
            .await
            .with_context(|| format!("preload spot {symbol} {date}"))?;
    }
    let spot_map_top: HashMap<String, Arc<SpotHistory>> = spot_cache.inner.clone();
    let perp = load_walkforward_perp(store, cfg, markets).await?;
    if cfg.portfolio_mode && (cfg.walk_forward_folds.is_some() || cfg.fold_size.is_some()) {
        return Err(anyhow!(
            "walk-forward fold configuration is not supported in portfolio mode"
        ));
    }

    if cfg.portfolio_mode {
        let preloaded_snapshot = if cfg.enable_meta_calibration {
            match cfg.meta_calibrator_snapshot_in.as_deref() {
                Some(path) => Some(read_meta_snapshot(path)?),
                None => None,
            }
        } else {
            None
        };
        if cfg.min_train_markets > 0 {
            if cfg.min_train_markets >= markets.len() {
                return Err(anyhow!(
                    "min_train_markets={} leaves no markets for portfolio evaluation",
                    cfg.min_train_markets
                ));
            }
            let mut meta_report = None;
            let meta_snapshot = if !cfg.enable_meta_calibration {
                tracing::info!(
                    min_train_markets = cfg.min_train_markets,
                    "meta-calibration disabled; preserving train/eval split without training snapshot"
                );
                None
            } else if let Some(snapshot) = preloaded_snapshot.clone() {
                meta_report = Some(MetaCalibrationReport {
                    train_markets: 0,
                    raw_train_samples: 0,
                    train_samples: 0,
                    train_updates: snapshot.updates,
                    train_log_loss: None,
                    selected_training_config: None,
                    candidate_evaluations: Vec::new(),
                    raw_validation_samples: 0,
                    validation_samples: 0,
                    validation: None,
                    selected: true,
                    rejected_reason: None,
                    oos_samples: 0,
                    oos_evaluation_samples: 0,
                    oos: None,
                    beta_enabled: snapshot.beta_enabled(),
                    beta_coefficients: snapshot.beta_coefficients(),
                    top_feature_weights: snapshot.top_feature_weights(12),
                });
                Some(snapshot)
            } else if cfg.forbid_meta_training {
                return Err(anyhow!(
                    "meta training is forbidden but no meta-calibrator snapshot was loaded"
                ));
            } else {
                let training_samples = load_or_collect_training_samples(
                    store,
                    &markets[..cfg.min_train_markets],
                    cfg,
                    &spot_map_top,
                )
                .await?;
                if training_samples.is_empty() {
                    None
                } else {
                    let selected = train_validated_meta_calibrator(
                        cfg.min_train_markets,
                        &training_samples,
                        cfg.meta_training_config,
                        MetaSampleLimits::from_config(cfg),
                        cfg.meta_calibrator_snapshot_out.as_deref(),
                    )?;
                    meta_report = Some(selected.report);
                    Some(selected.snapshot)
                }
            };
            return run_portfolio(
                store,
                &markets[cfg.min_train_markets..],
                cfg,
                &spot_map_top,
                perp,
                meta_snapshot,
                meta_report,
            )
            .await;
        }
        let meta_report = preloaded_snapshot
            .as_ref()
            .map(|snapshot| MetaCalibrationReport {
                train_markets: 0,
                raw_train_samples: 0,
                train_samples: 0,
                train_updates: snapshot.updates,
                train_log_loss: None,
                selected_training_config: None,
                candidate_evaluations: Vec::new(),
                raw_validation_samples: 0,
                validation_samples: 0,
                validation: None,
                selected: true,
                rejected_reason: None,
                oos_samples: 0,
                oos_evaluation_samples: 0,
                oos: None,
                beta_enabled: snapshot.beta_enabled(),
                beta_coefficients: snapshot.beta_coefficients(),
                top_feature_weights: snapshot.top_feature_weights(12),
            });
        return run_portfolio(
            store,
            markets,
            cfg,
            &spot_map_top,
            perp,
            preloaded_snapshot,
            meta_report,
        )
        .await;
    }

    let mut fold_summaries = Vec::new();
    let fold_plan = build_fold_plan(markets.len(), cfg)?;
    let use_folds = cfg.walk_forward_folds.is_some() || cfg.fold_size.is_some();

    let mut results = Vec::new();
    let mut training_samples = Vec::new();
    let mut training_loaded_until = 0usize;
    for (fold_idx, (train_end_exclusive, test_start, test_end)) in fold_plan.iter().enumerate() {
        let mut meta_train_samples = 0usize;
        let mut meta_train_log_loss = None;
        let meta_snapshot = if cfg.enable_meta_calibration && use_folds && *train_end_exclusive > 0
        {
            if *train_end_exclusive > training_loaded_until {
                let new_samples = collect_training_samples(
                    store,
                    &markets[training_loaded_until..*train_end_exclusive],
                    cfg,
                    &spot_map_top,
                )
                .await?;
                training_samples.extend(new_samples);
                training_loaded_until = *train_end_exclusive;
            }
            if training_samples.is_empty() {
                None
            } else {
                let mut state = ModelState::new();
                let stats = state.fit_meta_calibrator(&training_samples, cfg.meta_training_config);
                meta_train_samples = stats.samples;
                meta_train_log_loss = Some(stats.log_loss);
                tracing::info!(
                    fold = fold_idx,
                    samples = stats.samples,
                    updates = stats.updates,
                    train_markets = training_loaded_until,
                    log_loss = stats.log_loss,
                    "trained fold meta-calibrator"
                );
                Some(state.meta_calibrator_snapshot())
            }
        } else {
            None
        };
        let meta_oos = if cfg.enable_meta_calibration && use_folds {
            if let Some(snapshot) = meta_snapshot.as_ref() {
                let test_samples = collect_training_samples(
                    store,
                    &markets[*test_start..*test_end],
                    cfg,
                    &spot_map_top,
                )
                .await?;
                Some(evaluate_meta_calibration(snapshot, &test_samples))
            } else {
                None
            }
        } else {
            None
        };
        let fold_markets = &markets[*test_start..*test_end];
        let fold_markets = run_markets(
            store,
            fold_markets,
            cfg,
            &spot_map_top,
            perp.clone(),
            *test_start,
            cfg.max_concurrent_fetches,
            meta_snapshot,
        )
        .await?;
        if use_folds {
            let mut fold_summary = aggregate(&fold_markets, &cfg.strategies);
            fold_summary.config_fingerprint = Some(config_fingerprint(cfg));
            fold_summary.run_config = Some(summary_run_config(cfg));
            fold_summaries.push(WalkForwardFoldSummary {
                fold_idx,
                train_end_exclusive: *train_end_exclusive,
                purge_markets: cfg.purge_markets,
                test_start: *test_start,
                test_end: *test_end,
                meta_train_samples,
                meta_train_log_loss,
                meta_oos,
                fold_results: fold_summary,
            });
        }
        results.extend(fold_markets);
    }

    let mut summary = aggregate(&results, &cfg.strategies);
    summary.config_fingerprint = Some(config_fingerprint(cfg));
    summary.run_config = Some(summary_run_config(cfg));
    if use_folds {
        summary.fold_summaries = fold_summaries;
    }
    Ok((results, summary))
}


/// Run the walk-forward once per jittered latency and attach a p10/p50/p90 P&L
/// spread to the summary. Each run reuses the same markets/config with only
/// `taker_latency_ms` perturbed (and `jitter` cleared so the inner
/// `run_walkforward` call does not recurse). Runs are serial; the spot/perp
/// caches are rebuilt inside each `run_walkforward` call.
async fn run_jittered(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
) -> Result<(Vec<MarketResult>, WalkForwardSummary)> {
    let latencies = jittered_run_latencies(cfg);
    let n = latencies.len();
    tracing::info!(runs = n, base_latency_ms = cfg.taker_latency_ms, "jittered replay start");
    let mut net_pnls = Vec::with_capacity(n);
    let mut last_results: Vec<MarketResult> = Vec::new();
    let mut last_summary: Option<WalkForwardSummary> = None;
    for (i, lat) in latencies.iter().enumerate() {
        let mut jcfg = cfg.clone();
        jcfg.jitter = 0;
        jcfg.taker_latency_ms = *lat;
        tracing::info!(run = i, latency_ms = lat, "jitter run");
        // Box::pin breaks the run_walkforward -> run_jittered -> run_walkforward
        // async recursion (otherwise the future is infinitely sized).
        let (results, summary) = Box::pin(run_walkforward(store, markets, &jcfg)).await?;
        let total_net_pnl: f64 = summary
            .per_strategy
            .values()
            .map(|agg| agg.total_pnl_usdc)
            .sum();
        net_pnls.push(total_net_pnl);
        last_results = results;
        last_summary = Some(summary);
    }
    let mut summary = last_summary.ok_or_else(|| anyhow!("jitter produced no runs"))?;
    summary.jitter = build_jitter_report(cfg, latencies, net_pnls);
    Ok((last_results, summary))
}


fn build_fold_plan(total: usize, cfg: &WalkForwardConfig) -> Result<Vec<(usize, usize, usize)>> {
    if cfg.walk_forward_folds.is_some() && cfg.fold_size.is_some() {
        return Err(anyhow!(
            "cannot set both --walk-forward-folds and --fold-size"
        ));
    }
    if total == 0 {
        return Ok(vec![(0, 0, 0)]);
    }
    if let Some(folds) = cfg.walk_forward_folds {
        if folds == 0 {
            return Err(anyhow!("walk-forward-folds must be >= 1"));
        }
        if folds > total {
            return Err(anyhow!(
                "walk-forward-folds ({folds}) cannot exceed number of markets ({total})"
            ));
        }
        let base = total / folds;
        let remainder = total % folds;
        let mut test_start = 0usize;
        let mut out = Vec::with_capacity(folds);
        for fold_idx in 0..folds {
            let size = base + usize::from(fold_idx < remainder);
            let test_end = (test_start + size).min(total);
            if size == 0 || test_start >= total || test_end <= test_start {
                break;
            }
            let train_end_exclusive = test_start.saturating_sub(cfg.purge_markets);
            if train_end_exclusive >= cfg.min_train_markets {
                out.push((train_end_exclusive, test_start, test_end));
            }
            test_start = test_end;
        }
        if out.is_empty() {
            return Err(anyhow!(
                "no walk-forward folds satisfy min_train_markets={} with total markets={total}",
                cfg.min_train_markets
            ));
        }
        Ok(out)
    } else if let Some(fold_size) = cfg.fold_size {
        if fold_size == 0 {
            return Err(anyhow!("fold-size must be >= 1"));
        }
        let mut out = Vec::new();
        let mut test_start = 0usize;
        while test_start < total {
            let test_end = (test_start + fold_size).min(total);
            let train_end_exclusive = test_start.saturating_sub(cfg.purge_markets);
            if train_end_exclusive >= cfg.min_train_markets {
                out.push((train_end_exclusive, test_start, test_end));
            }
            test_start = test_end;
        }
        if out.is_empty() {
            return Err(anyhow!(
                "no walk-forward folds satisfy min_train_markets={} with total markets={total}",
                cfg.min_train_markets
            ));
        }
        Ok(out)
    } else {
        Ok(vec![(0, 0, total)])
    }
}


async fn load_or_collect_training_samples(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
) -> Result<Vec<MetaTrainingSample>> {
    if let Some(path) = cfg.meta_training_samples_cache.as_deref() {
        if path.exists() {
            let file = File::open(path)
                .with_context(|| format!("open meta training samples cache {}", path.display()))?;
            match serde_json::from_reader::<_, Vec<MetaTrainingSample>>(BufReader::new(file)) {
                Ok(samples) => {
                    tracing::info!(
                        samples = samples.len(),
                        path = %path.display(),
                        "loaded meta training samples cache"
                    );
                    return Ok(samples);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %path.display(),
                        %error,
                        "ignoring incompatible meta training samples cache"
                    );
                }
            }
        }

        let samples = collect_training_samples(store, markets, cfg, spot_map).await?;
        write_meta_training_samples(path, &samples)?;
        return Ok(samples);
    }

    collect_training_samples(store, markets, cfg, spot_map).await
}


fn write_meta_training_samples(
    path: &std::path::Path,
    samples: &[MetaTrainingSample],
) -> Result<()> {
    ensure_parent_dir(path)?;
    let file = File::create(path)
        .with_context(|| format!("create meta training samples cache {}", path.display()))?;
    serde_json::to_writer(BufWriter::new(file), samples)
        .with_context(|| format!("write meta training samples cache {}", path.display()))?;
    tracing::info!(
        samples = samples.len(),
        path = %path.display(),
        "wrote meta training samples cache"
    );
    Ok(())
}


fn read_meta_snapshot(path: &std::path::Path) -> Result<OnlineMetaCalibratorSnapshot> {
    let file = File::open(path)
        .with_context(|| format!("open meta-calibrator snapshot {}", path.display()))?;
    let snapshot = serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("read meta-calibrator snapshot {}", path.display()))?;
    tracing::info!(
        path = %path.display(),
        "loaded meta-calibrator snapshot"
    );
    Ok(snapshot)
}


fn write_meta_snapshot(
    path: &std::path::Path,
    snapshot: &OnlineMetaCalibratorSnapshot,
) -> Result<()> {
    ensure_parent_dir(path)?;
    let file = File::create(path)
        .with_context(|| format!("create meta-calibrator snapshot {}", path.display()))?;
    serde_json::to_writer_pretty(BufWriter::new(file), snapshot)
        .with_context(|| format!("write meta-calibrator snapshot {}", path.display()))?;
    tracing::info!(
        updates = snapshot.updates,
        path = %path.display(),
        "wrote meta-calibrator snapshot"
    );
    Ok(())
}


fn ensure_parent_dir(path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent directory {}", parent.display()))?;
    }
    Ok(())
}


struct SelectedMetaCalibrator {
    snapshot: OnlineMetaCalibratorSnapshot,
    report: MetaCalibrationReport,
}


const META_FEATURE_EARLY_MARKET_PENALTY: usize = 20;

const META_FEATURE_MID_DISTANCE_FROM_HALF: usize = 38;


#[derive(Debug, Clone, Copy)]
pub struct MetaSampleLimits {
    max_fit_samples: usize,
    max_validation_samples: usize,
    max_samples_per_market: usize,
    min_base_p: f32,
    max_early_penalty: f32,
    min_mid_distance: f32,
}


impl MetaSampleLimits {
    pub fn from_config(cfg: &WalkForwardConfig) -> Self {
        Self {
            max_fit_samples: cfg.meta_max_fit_samples,
            max_validation_samples: cfg.meta_max_validation_samples,
            max_samples_per_market: cfg.meta_max_samples_per_market,
            min_base_p: cfg.meta_train_min_base_p,
            max_early_penalty: cfg.meta_train_max_early_penalty,
            min_mid_distance: cfg.meta_train_min_mid_distance,
        }
    }
}


pub fn filter_meta_samples_for_training(
    samples: &[MetaTrainingSample],
    limits: MetaSampleLimits,
) -> Vec<MetaTrainingSample> {
    let min_base_p = limits.min_base_p.clamp(0.0, 1.0);
    let max_early_penalty = limits.max_early_penalty.clamp(0.0, 1.0);
    let min_mid_distance = limits.min_mid_distance.clamp(0.0, 1.0);
    if min_base_p <= 0.0 && max_early_penalty >= 1.0 && min_mid_distance <= 0.0 {
        return samples.to_vec();
    }
    samples
        .iter()
        .copied()
        .filter(|sample| {
            sample.base_side_probability >= min_base_p
                && sample.features.values[META_FEATURE_EARLY_MARKET_PENALTY] <= max_early_penalty
                && sample.features.values[META_FEATURE_MID_DISTANCE_FROM_HALF] >= min_mid_distance
        })
        .collect()
}


fn train_validated_meta_calibrator(
    train_markets: usize,
    training_samples: &[MetaTrainingSample],
    training_config: MetaTrainingConfig,
    limits: MetaSampleLimits,
    snapshot_out: Option<&std::path::Path>,
) -> Result<SelectedMetaCalibrator> {
    let filtered_training_samples = filter_meta_samples_for_training(training_samples, limits);
    if filtered_training_samples.len() != training_samples.len() {
        tracing::info!(
            raw_samples = training_samples.len(),
            filtered_samples = filtered_training_samples.len(),
            min_base_p = limits.min_base_p,
            max_early_penalty = limits.max_early_penalty,
            min_mid_distance = limits.min_mid_distance,
            "filtered meta training samples"
        );
    }
    let training_samples = filtered_training_samples.as_slice();
    if training_samples.len() < 2 {
        let snapshot = OnlineMetaCalibrator::default().snapshot();
        if let Some(path) = snapshot_out {
            write_meta_snapshot(path, &snapshot)?;
        }
        let stats = MetaTrainingStats {
            samples: 0,
            epochs: 0,
            updates: 0,
            log_loss: 0.0,
        };
        return Ok(SelectedMetaCalibrator {
            report: meta_calibration_report(
                train_markets,
                &[],
                &stats,
                &snapshot,
                None,
                Vec::new(),
                0,
                0,
                None,
                0,
                false,
                Some("not enough training samples".to_string()),
            ),
            snapshot,
        });
    }
    let (raw_fit_samples, raw_validation_samples) =
        split_meta_samples_by_market(training_samples, 0.80);
    let fit_samples = market_balanced_meta_samples(
        &raw_fit_samples,
        limits.max_fit_samples,
        limits.max_samples_per_market,
    );
    let validation_samples = market_balanced_meta_samples(
        &raw_validation_samples,
        limits.max_validation_samples,
        limits.max_samples_per_market,
    );
    tracing::info!(
        raw_fit_samples = raw_fit_samples.len(),
        fit_samples = fit_samples.len(),
        raw_validation_samples = raw_validation_samples.len(),
        validation_samples = validation_samples.len(),
        max_samples_per_market = limits.max_samples_per_market,
        "selected market-balanced meta-calibrator samples"
    );

    let candidates = meta_training_candidates(training_config);
    let mut candidate_evaluations = Vec::with_capacity(candidates.len());
    let mut best: Option<(
        MetaTrainingConfig,
        MetaTrainingStats,
        OnlineMetaCalibratorSnapshot,
        MetaEvaluationSummary,
    )> = None;
    for candidate_cfg in candidates {
        let mut state = ModelState::new();
        let stats = state.fit_meta_calibrator(&fit_samples, candidate_cfg);
        let snapshot = state.meta_calibrator_snapshot();
        let validation = evaluate_meta_calibration(&snapshot, &validation_samples);
        let validation_passed = validation.calibrated_log_loss < validation.base_log_loss
            && validation.calibrated_log_loss < validation.prior_log_loss
            && validation.calibrated_brier <= validation.base_brier
            && validation.calibrated_brier <= validation.prior_brier
            && validation.market_equal_weighted_calibrated_log_loss
                < validation.market_equal_weighted_base_log_loss
            && validation.market_equal_weighted_calibrated_log_loss
                < validation.market_equal_weighted_prior_log_loss
            && validation.market_equal_weighted_calibrated_brier
                <= validation.market_equal_weighted_base_brier
            && validation.market_equal_weighted_calibrated_brier
                <= validation.market_equal_weighted_prior_brier;
        if validation_passed {
            let replace = best
                .as_ref()
                .map(|(_, _, _, best_validation)| {
                    validation.calibrated_log_loss < best_validation.calibrated_log_loss
                })
                .unwrap_or(true);
            if replace {
                best = Some((candidate_cfg, stats, snapshot.clone(), validation.clone()));
            }
        }
        candidate_evaluations.push(MetaCandidateEvaluation {
            training_config: candidate_cfg,
            train_log_loss: stats.log_loss,
            updates: stats.updates,
            beta_enabled: snapshot.beta_enabled(),
            beta_coefficients: snapshot.beta_coefficients(),
            isotonic_bins: snapshot.isotonic_bins(),
            tree_count: snapshot.tree_count(),
            tree_split_count: snapshot.tree_split_count(),
            top_feature_weights: snapshot.top_feature_weights(8),
            validation,
            selected: false,
        });
    }
    let (selected_training_config, stats, selected_snapshot, validation, validation_passed) =
        if let Some((selected_cfg, stats, snapshot, validation)) = best {
            if let Some(candidate) = candidate_evaluations
                .iter_mut()
                .find(|candidate| candidate.training_config == selected_cfg)
            {
                candidate.selected = true;
            }
            (Some(selected_cfg), stats, snapshot, validation, true)
        } else {
            let stats = MetaTrainingStats {
                samples: fit_samples.len(),
                epochs: training_config.epochs,
                updates: 0,
                log_loss: 0.0,
            };
            let snapshot = OnlineMetaCalibrator::default().snapshot();
            let validation = evaluate_meta_calibration(&snapshot, &validation_samples);
            (None, stats, snapshot, validation, false)
        };
    if let Some(path) = snapshot_out {
        write_meta_snapshot(path, &selected_snapshot)?;
    }
    tracing::info!(
        fit_samples = stats.samples,
        validation_samples = validation.samples,
        updates = stats.updates,
        train_markets,
        train_log_loss = stats.log_loss,
        validation_market_count = validation.market_count,
        validation_positive_rate = validation.positive_rate,
        validation_market_equal_weighted_positive_rate =
            validation.market_equal_weighted_positive_rate,
        validation_prior_log_loss = validation.prior_log_loss,
        validation_base_log_loss = validation.base_log_loss,
        validation_calibrated_log_loss = validation.calibrated_log_loss,
        validation_market_equal_weighted_prior_log_loss =
            validation.market_equal_weighted_prior_log_loss,
        validation_market_equal_weighted_base_log_loss =
            validation.market_equal_weighted_base_log_loss,
        validation_market_equal_weighted_calibrated_log_loss =
            validation.market_equal_weighted_calibrated_log_loss,
        validation_calibrated_mean = validation.calibrated_distribution.mean,
        validation_calibrated_p50 = validation.calibrated_distribution.p50,
        validation_calibrated_p90 = validation.calibrated_distribution.p90,
        validation_calibrated_share_ge_60 = validation.calibrated_distribution.share_ge_60,
        validation_calibrated_share_ge_65 = validation.calibrated_distribution.share_ge_65,
        validation_prior_brier = validation.prior_brier,
        validation_base_brier = validation.base_brier,
        validation_calibrated_brier = validation.calibrated_brier,
        selected = validation_passed,
        ?selected_training_config,
        "validated portfolio meta-calibrator"
    );
    let rejected_reason = if validation_passed {
        None
    } else {
        Some(
            "validation log loss or brier did not improve over sample and market-equal base/prior baselines"
                .to_string(),
        )
    };
    let report = meta_calibration_report(
        train_markets,
        &fit_samples,
        &stats,
        &selected_snapshot,
        selected_training_config,
        candidate_evaluations,
        raw_fit_samples.len(),
        validation_samples.len(),
        Some(validation),
        raw_validation_samples.len(),
        validation_passed,
        rejected_reason,
    );
    Ok(SelectedMetaCalibrator {
        snapshot: selected_snapshot,
        report,
    })
}


fn split_meta_samples_by_market(
    samples: &[MetaTrainingSample],
    fit_fraction: f64,
) -> (Vec<MetaTrainingSample>, Vec<MetaTrainingSample>) {
    let groups = group_meta_samples_by_market(samples);
    if groups.len() < 2 {
        return (samples.to_vec(), Vec::new());
    }

    let split_at = ((groups.len() as f64) * fit_fraction).round() as usize;
    let split_at = split_at.clamp(1, groups.len().saturating_sub(1));
    let mut fit = Vec::new();
    let mut validation = Vec::new();
    for (idx, (_market_idx, market_samples)) in groups.into_iter().enumerate() {
        if idx < split_at {
            fit.extend(market_samples);
        } else {
            validation.extend(market_samples);
        }
    }
    (fit, validation)
}


pub fn market_balanced_meta_samples(
    samples: &[MetaTrainingSample],
    max_samples: usize,
    max_samples_per_market: usize,
) -> Vec<MetaTrainingSample> {
    if samples.is_empty() || max_samples == 0 {
        return Vec::new();
    }
    if samples.len() <= max_samples && max_samples_per_market == 0 {
        return samples.to_vec();
    }

    let groups = group_meta_samples_by_market(samples);
    if groups.is_empty() {
        return Vec::new();
    }

    let configured_per_market = if max_samples_per_market == 0 {
        usize::MAX
    } else {
        max_samples_per_market
    };
    let per_market_cap = if max_samples >= groups.len() {
        configured_per_market.min((max_samples / groups.len()).max(1))
    } else {
        1
    };
    let mut selected = Vec::with_capacity(samples.len().min(max_samples));
    for (_market_idx, market_samples) in groups {
        let take = market_samples.len().min(per_market_cap);
        extend_evenly_sampled(&market_samples, take, &mut selected);
    }

    if selected.len() <= max_samples {
        selected
    } else {
        let mut bounded = Vec::with_capacity(max_samples);
        extend_evenly_sampled(&selected, max_samples, &mut bounded);
        bounded
    }
}


fn group_meta_samples_by_market(
    samples: &[MetaTrainingSample],
) -> Vec<(u32, Vec<MetaTrainingSample>)> {
    let mut groups: BTreeMap<u32, Vec<MetaTrainingSample>> = BTreeMap::new();
    for sample in samples {
        groups.entry(sample.market_idx).or_default().push(*sample);
    }
    groups.into_iter().collect()
}


fn extend_evenly_sampled<T: Copy>(items: &[T], take: usize, out: &mut Vec<T>) {
    if take == 0 || items.is_empty() {
        return;
    }
    if take >= items.len() {
        out.extend_from_slice(items);
        return;
    }
    if take == 1 {
        out.push(items[items.len() / 2]);
        return;
    }
    let last = items.len() - 1;
    let denom = take - 1;
    for i in 0..take {
        let idx = (i * last + denom / 2) / denom;
        out.push(items[idx]);
    }
}


fn meta_training_candidates(primary: MetaTrainingConfig) -> Vec<MetaTrainingConfig> {
    let candidates = [
        primary,
        MetaTrainingConfig {
            epochs: 4,
            learning_rate: 0.005,
            l2: 0.01,
            weight_clip: 0.25,
            reset_before_fit: true,
        },
        MetaTrainingConfig {
            epochs: 8,
            learning_rate: 0.01,
            l2: 0.01,
            weight_clip: 0.50,
            reset_before_fit: true,
        },
        MetaTrainingConfig {
            epochs: 12,
            learning_rate: 0.02,
            l2: 0.005,
            weight_clip: 0.75,
            reset_before_fit: true,
        },
        MetaTrainingConfig {
            epochs: 16,
            learning_rate: 0.02,
            l2: 0.01,
            weight_clip: 1.00,
            reset_before_fit: true,
        },
    ];

    let mut deduped = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        if !deduped.contains(&candidate) {
            deduped.push(candidate);
        }
    }
    deduped
}


pub fn evaluate_meta_calibration(
    snapshot: &OnlineMetaCalibratorSnapshot,
    samples: &[MetaTrainingSample],
) -> MetaEvaluationSummary {
    let calibrator = OnlineMetaCalibrator::from_snapshot(snapshot.clone());
    if samples.is_empty() {
        return MetaEvaluationSummary {
            samples: 0,
            market_count: 0,
            positive_rate: 0.0,
            market_equal_weighted_positive_rate: 0.0,
            base_distribution: PredictionDistribution::default(),
            calibrated_distribution: PredictionDistribution::default(),
            prior_log_loss: 0.0,
            base_log_loss: 0.0,
            calibrated_log_loss: 0.0,
            log_loss_delta: 0.0,
            prior_log_loss_delta: 0.0,
            prior_brier: 0.0,
            base_brier: 0.0,
            calibrated_brier: 0.0,
            brier_delta: 0.0,
            prior_brier_delta: 0.0,
            market_equal_weighted_prior_log_loss: 0.0,
            market_equal_weighted_base_log_loss: 0.0,
            market_equal_weighted_calibrated_log_loss: 0.0,
            market_equal_weighted_prior_brier: 0.0,
            market_equal_weighted_base_brier: 0.0,
            market_equal_weighted_calibrated_brier: 0.0,
            base_accuracy: 0.0,
            calibrated_accuracy: 0.0,
            calibrated_ece: 0.0,
            calibration_bins: Vec::new(),
        };
    }

    let mut base_log_loss = 0.0f32;
    let mut calibrated_log_loss = 0.0f32;
    let mut base_brier = 0.0f32;
    let mut calibrated_brier = 0.0f32;
    let mut base_correct = 0usize;
    let mut calibrated_correct = 0usize;
    let mut observed_count = 0usize;
    let mut base_predictions = Vec::with_capacity(samples.len());
    let mut calibrated_predictions = Vec::with_capacity(samples.len());
    let mut bins = [CalibrationBinAccumulator::default(); 10];
    let mut market_accumulators: std::collections::BTreeMap<u32, MarketCalibrationAccumulator> =
        std::collections::BTreeMap::new();

    for sample in samples {
        let observed = sample.side_observed;
        let target = if observed { 1.0 } else { 0.0 };
        observed_count += usize::from(observed);
        let base = sample.base_side_probability.clamp(1.0e-6, 1.0 - 1.0e-6);
        let calibrated = calibrator
            .predict_side_win_probability(base, &sample.features)
            .clamp(1.0e-6, 1.0 - 1.0e-6);

        base_log_loss += binary_log_loss(base, observed);
        calibrated_log_loss += binary_log_loss(calibrated, observed);
        base_brier += (base - target) * (base - target);
        calibrated_brier += (calibrated - target) * (calibrated - target);
        let market_acc = market_accumulators.entry(sample.market_idx).or_default();
        market_acc.samples += 1;
        market_acc.observed += usize::from(observed);
        market_acc.base_log_loss += binary_log_loss(base, observed);
        market_acc.calibrated_log_loss += binary_log_loss(calibrated, observed);
        market_acc.base_brier += (base - target) * (base - target);
        market_acc.calibrated_brier += (calibrated - target) * (calibrated - target);
        base_predictions.push(base);
        calibrated_predictions.push(calibrated);
        base_correct += usize::from((base >= 0.5) == observed);
        calibrated_correct += usize::from((calibrated >= 0.5) == observed);
        let bin = ((calibrated * bins.len() as f32).floor() as usize).min(bins.len() - 1);
        bins[bin].samples += 1;
        bins[bin].sum_predicted += calibrated;
        bins[bin].observed += usize::from(observed);
    }

    let n = samples.len() as f32;
    let positive_rate = observed_count as f32 / n;
    let prior = positive_rate.clamp(1.0e-6, 1.0 - 1.0e-6);
    let prior_log_loss = -(positive_rate * prior.ln() + (1.0 - positive_rate) * (1.0 - prior).ln());
    let prior_brier = samples
        .iter()
        .map(|sample| {
            let target = if sample.side_observed { 1.0 } else { 0.0 };
            (prior - target) * (prior - target)
        })
        .sum::<f32>()
        / n;
    let base_log_loss = base_log_loss / n;
    let calibrated_log_loss = calibrated_log_loss / n;
    let base_brier = base_brier / n;
    let calibrated_brier = calibrated_brier / n;
    let market_count = market_accumulators.len();
    let mut market_positive_rate = 0.0f32;
    let mut market_prior_log_loss = 0.0f32;
    let mut market_base_log_loss = 0.0f32;
    let mut market_calibrated_log_loss = 0.0f32;
    let mut market_prior_brier = 0.0f32;
    let mut market_base_brier = 0.0f32;
    let mut market_calibrated_brier = 0.0f32;
    for market in market_accumulators.values() {
        let market_n = market.samples.max(1) as f32;
        let observed_rate = market.observed as f32 / market_n;
        market_positive_rate += observed_rate;
        market_prior_log_loss +=
            -(observed_rate * prior.ln() + (1.0 - observed_rate) * (1.0 - prior).ln());
        market_base_log_loss += market.base_log_loss / market_n;
        market_calibrated_log_loss += market.calibrated_log_loss / market_n;
        market_prior_brier +=
            observed_rate * (1.0 - prior) * (1.0 - prior) + (1.0 - observed_rate) * prior * prior;
        market_base_brier += market.base_brier / market_n;
        market_calibrated_brier += market.calibrated_brier / market_n;
    }
    let market_denom = market_count.max(1) as f32;
    let market_positive_rate = market_positive_rate / market_denom;
    let market_prior_log_loss = market_prior_log_loss / market_denom;
    let market_base_log_loss = market_base_log_loss / market_denom;
    let market_calibrated_log_loss = market_calibrated_log_loss / market_denom;
    let market_prior_brier = market_prior_brier / market_denom;
    let market_base_brier = market_base_brier / market_denom;
    let market_calibrated_brier = market_calibrated_brier / market_denom;
    let mut calibrated_ece = 0.0f32;
    let calibration_bins: Vec<CalibrationBin> = bins
        .iter()
        .enumerate()
        .filter_map(|(idx, bin)| {
            if bin.samples == 0 {
                return None;
            }
            let samples_f = bin.samples as f32;
            let avg_predicted = bin.sum_predicted / samples_f;
            let observed_rate = bin.observed as f32 / samples_f;
            calibrated_ece += (samples_f / n) * (avg_predicted - observed_rate).abs();
            Some(CalibrationBin {
                lower: idx as f32 / 10.0,
                upper: (idx + 1) as f32 / 10.0,
                samples: bin.samples,
                avg_predicted,
                observed_rate,
            })
        })
        .collect();
    MetaEvaluationSummary {
        samples: samples.len(),
        market_count,
        positive_rate,
        market_equal_weighted_positive_rate: market_positive_rate,
        base_distribution: prediction_distribution(&mut base_predictions),
        calibrated_distribution: prediction_distribution(&mut calibrated_predictions),
        prior_log_loss,
        base_log_loss,
        calibrated_log_loss,
        log_loss_delta: calibrated_log_loss - base_log_loss,
        prior_log_loss_delta: calibrated_log_loss - prior_log_loss,
        prior_brier,
        base_brier,
        calibrated_brier,
        brier_delta: calibrated_brier - base_brier,
        prior_brier_delta: calibrated_brier - prior_brier,
        market_equal_weighted_prior_log_loss: market_prior_log_loss,
        market_equal_weighted_base_log_loss: market_base_log_loss,
        market_equal_weighted_calibrated_log_loss: market_calibrated_log_loss,
        market_equal_weighted_prior_brier: market_prior_brier,
        market_equal_weighted_base_brier: market_base_brier,
        market_equal_weighted_calibrated_brier: market_calibrated_brier,
        base_accuracy: base_correct as f32 / n,
        calibrated_accuracy: calibrated_correct as f32 / n,
        calibrated_ece,
        calibration_bins,
    }
}


async fn collect_training_samples(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
) -> Result<Vec<MetaTrainingSample>> {
    if markets.is_empty() {
        return Ok(Vec::new());
    }

    let store_inner = store.store();
    let empty_spot = Arc::new(SpotHistory::default());
    let total = markets.len();
    let completed = Arc::new(AtomicUsize::new(0));
    let concurrency = cfg.max_concurrent_fetches.max(1);
    tracing::info!(markets = total, concurrency, "collecting training samples");

    let mut indexed_samples =
        futures::stream::iter(markets.iter().cloned().enumerate().map(|(idx, m)| {
            let store = store.clone();
            let store_inner = store_inner.clone();
            let spot = spot_history_for_market(spot_map, &empty_spot, cfg, &m);
            let use_outcome_label = cfg.use_outcome_label;
            let starting_cash_usdc = cfg.starting_cash_usdc;
            let replay_sample_ms = cfg.replay_sample_ms;
            let replay_event_cache_dir = cfg.replay_event_cache_dir.clone();
            let enable_market_context_features = cfg.enable_market_context_features;
            let completed = completed.clone();
            async move {
                let samples = collect_training_samples_for_market(
                    &store,
                    store_inner,
                    idx,
                    &m,
                    spot,
                    use_outcome_label,
                    starting_cash_usdc,
                    replay_sample_ms,
                    replay_event_cache_dir.as_deref(),
                    enable_market_context_features,
                )
                .await;
                let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
                if done % 100 == 0 || done == total {
                    tracing::info!(done, total, "training sample progress");
                }
                (idx, samples)
            }
        }))
        .buffer_unordered(concurrency)
        .collect::<Vec<_>>()
        .await;

    indexed_samples.sort_by_key(|(idx, _)| *idx);
    let mut samples = Vec::with_capacity(markets.len());
    for (_, market_samples) in indexed_samples {
        samples.extend(market_samples);
    }
    Ok(samples)
}


async fn collect_training_samples_for_market(
    store: &TelonexStore,
    store_inner: Arc<dyn object_store::ObjectStore>,
    idx: usize,
    m: &MarketHandle,
    spot: Arc<SpotHistory>,
    use_outcome_label: bool,
    starting_cash_usdc: f64,
    replay_sample_ms: u64,
    replay_event_cache_dir: Option<&Path>,
    enable_market_context_features: bool,
) -> Vec<MetaTrainingSample> {
    let events = match load_replay_events_for_market(
        store,
        store_inner,
        m,
        MarketId(idx as u32 + 1),
        replay_event_cache_dir,
    )
    .await
    {
        Ok(events) => events,
        Err(e) => {
            tracing::warn!(market = %m.slug, error = %e, "training tape load failed");
            return Vec::new();
        }
    };
    let sampled_events;
    let events_for_run = if replay_sample_ms > 0 {
        sampled_events = sample_replay_events(&events, replay_sample_ms);
        sampled_events.as_slice()
    } else {
        events.as_slice()
    };
    let resolved_yes = if use_outcome_label {
        Some(
            outcome_label_resolved_yes(&m.outcome)
                .expect("outcome labels are validated before replay"),
        )
    } else {
        None
    };
    let runner_cfg = RunnerConfig {
        starting_cash_usdc,
        market_open_ns: market_open_ns(m),
        market_close_ns: market_close_ns(m),
        resolved_yes,
        portfolio_limits: PortfolioLimits::default(),
        equity_curve_jsonl: None,
        snapshot_every_n: 1_000_000,
        maker_rebate_bps: 0.0,
        taker_fee_bps: 0.0,
        decision_log_jsonl: None,
        decision_log_parquet: None,
        strategy_name: "meta_training".to_string(),
        shared_model_state: None,
        update_model_state_on_resolution: true,
        meta_calibrator_snapshot: None,
        enable_meta_calibration: true,
        current_btc_net_shares: 0.0,
        current_eth_net_shares: 0.0,
        daily_start_cash_usdc: 0.0,
        daily_loss_cap_pct: 1.0,
        model_market_context: if enable_market_context_features {
            model_market_context_for_slug(&m.slug)
        } else {
            ModelMarketContext::default()
        },
        prior_market_range_1d: 0.0,
        prior_market_range_3d: 0.0,
        prior_market_range_7d: 0.0,
        model_btc_whipsaw_risk_weight: 0.16,
        model_btc_path_inefficiency_risk_weight: 0.10,
        model_btc_reversal_pressure_risk_weight: 0.12,
        decision_log_every_n: 1_000_000,
        max_inventory_imbalance_shares: 1.5,
        taker_slippage_bps: 0.0,
        taker_latency_ms: 0,
        enforce_model_gate: false,
        model_gate_min_confidence: 0.68,
        model_gate_max_risk: 0.72,
        model_gate_min_edge: 0.05,
        current_daily_loss_pct: 0.0,
    };
    let mut strat = NoopStrategy;
    match run_backtest(
        events_for_run,
        &spot,
        &TradeHistory::default(),
        &mut strat,
        &runner_cfg,
    ) {
        Ok(report) => {
            let mut samples = report.model_training_samples;
            for sample in &mut samples {
                sample.market_idx = idx as u32;
            }
            samples
        }
        Err(e) => {
            tracing::warn!(market = %m.slug, error = %e, "training sample extraction failed");
            Vec::new()
        }
    }
}


async fn run_markets(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
    perp: Option<Arc<PerpState>>,
    market_id_offset: usize,
    max_concurrent_fetches: usize,
    meta_calibrator_snapshot: Option<OnlineMetaCalibratorSnapshot>,
) -> Result<Vec<MarketResult>> {
    if markets.is_empty() {
        return Ok(Vec::new());
    }

    let spot_empty = Arc::new(SpotHistory::default());
    let store_inner = store.store();
    let cfg_arc = Arc::new(cfg.clone());
    let perp = perp.clone();

    // Phase 1: bounded async I/O — only load raw data + build runner config.
    // No strategy execution here.
    let load_stream = futures::stream::iter(markets.iter().enumerate().map(|(idx, m)| {
        let store_inner = store_inner.clone();
        let spot_map = spot_map.clone();
        let spot_empty = spot_empty.clone();
        let cfg_arc = cfg_arc.clone();
        let meta_calibrator_snapshot = meta_calibrator_snapshot.clone();
        let store_for_resolve = store.clone();

        async move {
            let market_id = MarketId((idx + market_id_offset) as u32 + 1);
            let events = match load_replay_events_for_market(
                &store_for_resolve,
                store_inner.clone(),
                &m,
                market_id,
                cfg_arc.replay_event_cache_dir.as_deref(),
            )
            .await
            {
                Ok(events) => events,
                Err(e) => {
                    tracing::warn!(market = %m.slug, error = %e, "tape load failed");
                    return None;
                }
            };

            let sampled_events;
            let events_for_run = if cfg_arc.replay_sample_ms > 0 {
                sampled_events = sample_replay_events(&events, cfg_arc.replay_sample_ms);
                sampled_events.as_slice().to_vec()
            } else {
                events.clone()
            };

            let trades = if cfg_arc.load_pm_trades {
                match resolve_pm_trades_day(&store_for_resolve, &m.date, &m.asset_id).await {
                    Ok(tp) => match load_pm_trades_async(store_inner.clone(), tp).await {
                        Ok((ticks, _)) => Arc::new(TradeHistory::new(ticks)),
                        Err(e) => {
                            tracing::debug!(market = %m.slug, error = %e, "trades load failed");
                            Arc::new(TradeHistory::default())
                        }
                    },
                    Err(_) => Arc::new(TradeHistory::default()),
                }
            } else {
                Arc::new(TradeHistory::default())
            };

            let spot = spot_history_for_market(&spot_map, &spot_empty, &cfg_arc, &m);

            let resolved_yes = if cfg_arc.use_outcome_label {
                Some(
                    outcome_label_resolved_yes(&m.outcome)
                        .expect("outcome labels are validated before replay"),
                )
            } else {
                None
            };

            let runner_cfg = RunnerConfig {
                starting_cash_usdc: cfg_arc.starting_cash_usdc,
                market_open_ns: market_open_ns(&m),
                market_close_ns: market_close_ns(&m),
                resolved_yes,
                portfolio_limits: PortfolioLimits {
                    max_clip_usdc: cfg_arc.max_clip_usdc * cfg_arc.max_order_clip_multiplier,
                    max_per_market_exposure_usdc: per_market_exposure_cap(
                        &cfg_arc,
                        cfg_arc.starting_cash_usdc,
                    ),
                    ..PortfolioLimits::default()
                },
                equity_curve_jsonl: None,
                snapshot_every_n: 1_000_000,
                maker_rebate_bps: cfg_arc.maker_rebate_bps,
                taker_fee_bps: cfg_arc.taker_fee_bps,
                decision_log_jsonl: None,
                decision_log_parquet: None,
                strategy_name: "parallel_walkforward".to_string(),
                shared_model_state: None,
                update_model_state_on_resolution: meta_calibrator_snapshot.is_none(),
                meta_calibrator_snapshot,
                enable_meta_calibration: cfg_arc.enable_meta_calibration,
                model_market_context: model_market_context_for_cfg(&cfg_arc, &m),
                prior_market_range_1d: 0.0,
                prior_market_range_3d: 0.0,
                prior_market_range_7d: 0.0,
                model_btc_whipsaw_risk_weight: cfg_arc.model_btc_whipsaw_risk_weight,
                model_btc_path_inefficiency_risk_weight: cfg_arc
                    .model_btc_path_inefficiency_risk_weight,
                model_btc_reversal_pressure_risk_weight: cfg_arc
                    .model_btc_reversal_pressure_risk_weight,
                decision_log_every_n: 1_000_000,
                max_inventory_imbalance_shares: 1.5,
                taker_slippage_bps: 15.0,
                taker_latency_ms: cfg_arc.taker_latency_ms,
                enforce_model_gate: cfg_arc.enforce_model_gate,
                model_gate_min_confidence: cfg_arc.model_gate_min_confidence,
                model_gate_max_risk: cfg_arc.model_gate_max_risk,
                model_gate_min_edge: cfg_arc.model_gate_min_edge,
                current_btc_net_shares: 0.0,
                current_eth_net_shares: 0.0,
                daily_start_cash_usdc: 0.0,
                daily_loss_cap_pct: 1.0,
                current_daily_loss_pct: 0.0,
            };

            Some((m.clone(), events_for_run, spot, trades, runner_cfg, idx))
        }
    }))
    .buffer_unordered(max_concurrent_fetches);

    let mut loaded: Vec<_> = Vec::new();
    let mut load_stream = std::pin::pin!(load_stream);
    while let Some(item) = load_stream.next().await {
        if let Some(x) = item {
            loaded.push(x);
        }
    }

    let mut results = Vec::with_capacity(loaded.len());

    if cfg.portfolio_mode {
        loaded.sort_by_key(|(_, _, _, _, _, idx)| *idx);

        // Must stay strictly serial: each market's starting equity depends on previous.
        for (m, events_for_run, spot, trades, runner_cfg, idx) in loaded {
            let mut per_strategy = HashMap::new();
            for &strat in &cfg.strategies {
                match run_one_strategy(
                    strat,
                    cfg,
                    &m.slug,
                    &events_for_run,
                    &spot,
                    &trades,
                    &runner_cfg,
                    cfg.starting_cash_usdc,
                    cfg.max_clip_usdc,
                    perp.clone(),
                ) {
                    Ok(mut r) => {
                        for sample in &mut r.model_training_samples {
                            sample.market_idx = (idx + market_id_offset) as u32;
                        }
                        per_strategy.insert(strat.name(), r);
                    }
                    Err(e) => {
                        tracing::warn!(market = %m.slug, strategy = strat.name(), error = %e, "strategy run failed");
                    }
                }
            }
            let volatility_range = market_volatility_range(&events_for_run);
            let volatility_band =
                volatility_band(volatility_range, cfg.volatility_regime_threshold);
            let close_ts = market_close_ts(&m);

            results.push(MarketResult {
                asset_id: m.asset_id,
                slug: m.slug,
                close_ts,
                outcome_label: m.outcome,
                volatility_range,
                volatility_band,
                per_strategy,
            });
        }
    } else {
        // Phase 2: full parallel execution across markets/days using rayon.
        // This is the main grid-scale win for independent (non-portfolio) runs.
        use rayon::prelude::*;

        let parallel_results: Vec<_> = loaded
            .into_par_iter()
            .map(|(m, events_for_run, spot, trades, runner_cfg, idx)| {
                let mut per_strategy = HashMap::new();
                for &strat in &cfg.strategies {
                    match run_one_strategy(
                        strat,
                        cfg,
                        &m.slug,
                        &events_for_run,
                        &spot,
                        &trades,
                        &runner_cfg,
                        cfg.starting_cash_usdc,
                        cfg.max_clip_usdc,
                        perp.clone(),
                    ) {
                        Ok(mut r) => {
                            for sample in &mut r.model_training_samples {
                                sample.market_idx = (idx + market_id_offset) as u32;
                            }
                            per_strategy.insert(strat.name(), r);
                        }
                        Err(e) => {
                            tracing::warn!(market = %m.slug, strategy = strat.name(), error = %e, "strategy run failed");
                        }
                    }
                }
                let volatility_range = market_volatility_range(&events_for_run);
                let volatility_band =
                    volatility_band(volatility_range, cfg.volatility_regime_threshold);
                let close_ts = market_close_ts(&m);

                MarketResult {
                    asset_id: m.asset_id,
                    slug: m.slug,
                    close_ts,
                    outcome_label: m.outcome,
                    volatility_range,
                    volatility_band,
                    per_strategy,
                }
            })
            .collect();

        results.extend(parallel_results);
    }

    results.sort_by_key(|r| r.close_ts);
    Ok(results)
}


fn run_one_strategy(
    strat: StratId,
    cfg: &WalkForwardConfig,
    market_slug: &str,
    events: &[pm_types::ReplayEvent],
    spot: &SpotHistory,
    trades: &TradeHistory,
    runner_cfg: &RunnerConfig,
    bankroll: f64,
    clip: f64,
    perp: Option<Arc<PerpState>>,
) -> Result<StrategyMarketResult> {
    let report = match strat {
        StratId::ExoFade | StratId::MayJuneFade => {
            let slug = market_slug.to_ascii_lowercase();
            let token = if slug.starts_with("eth-updown-") {
                "eth"
            } else if slug.starts_with("sol-updown-") {
                "sol"
            } else if slug.starts_with("xrp-updown-") {
                "xrp"
            } else {
                "btc"
            };
            let window_secs = market_duration_secs_from_slug(market_slug).max(300) as u32;
            let mut base = match strat {
                StratId::MayJuneFade => ExoFadeConfig::mayjune_btc5m(),
                _ => ExoFadeConfig::champion_1k(),
            };
            base.directional_tilt_strength = cfg.directional_tilt_strength;
            let mut s = ExoFadeStrategy::new(ExoFadeConfig {
                bankroll_usdc: bankroll,
                clip_usdc: clip,
                kelly_fraction: cfg.kelly_fraction,
                token: token.into(),
                window_secs,
                ..base
            });
            if let Some(p) = perp {
                s = s.with_perp(p);
            }
            run_backtest(events, spot, trades, &mut s, runner_cfg)?
        }
        StratId::Noop => {
            let mut s = NoopStrategy;
            run_backtest(events, spot, trades, &mut s, runner_cfg)?
        }
    };
    let filled_notional_usdc = report.filled_notional_usdc;
    let avg_slippage_bps = if filled_notional_usdc > 0.0 {
        report
            .fills
            .iter()
            .map(|fill| fill.slippage_bps as f64 * fill.notional)
            .sum::<f64>()
            / filled_notional_usdc
    } else {
        0.0
    };
    Ok(StrategyMarketResult {
        orders_submitted: report.counters.orders_submitted,
        orders_filled: report.counters.orders_filled_taker + report.counters.orders_filled_maker,
        orders_filled_taker: report.counters.orders_filled_taker,
        orders_filled_maker: report.counters.orders_filled_maker,
        orders_rejected_model_gate: report.counters.orders_rejected_model_gate,
        orders_rejected_model_gate_confidence: report
            .counters
            .orders_rejected_model_gate_confidence,
        orders_rejected_model_gate_risk: report.counters.orders_rejected_model_gate_risk,
        orders_rejected_model_gate_edge: report.counters.orders_rejected_model_gate_edge,
        pnl_usdc: report.pnl_usdc,
        start_equity_usdc: bankroll,
        end_equity_usdc: report.end_equity_usdc,
        max_drawdown_pct: report.max_drawdown_pct,
        fills: report.fills.len(),
        maker_rebates_usdc: report.maker_rebates_usdc,
        requested_shares: report.requested_shares,
        filled_shares: report.filled_shares,
        fill_shares_ratio: if report.requested_shares > 0.0 {
            report.filled_shares / report.requested_shares
        } else {
            0.0
        },
        requested_notional_usdc: report.requested_notional_usdc,
        filled_notional_usdc,
        fill_notional_ratio: if report.requested_notional_usdc > 0.0 {
            filled_notional_usdc / report.requested_notional_usdc
        } else {
            0.0
        },
        avg_slippage_bps,
        clip_used_usdc: clip,
        yes_resolved: report.yes_resolved,
        fills_detail: report.fills,
        model_training_samples: report.model_training_samples,
    })
}


/// Portfolio-mode walk-forward: sequential, chronological, compounding equity.
/// Each strategy maintains its own running bankroll; per-market max_clip can
/// scale with equity via `cfg.clip_fraction_of_equity`.
async fn run_portfolio(
    store: &TelonexStore,
    markets: &[MarketHandle],
    cfg: &WalkForwardConfig,
    spot_map: &HashMap<String, Arc<SpotHistory>>,
    perp: Option<Arc<PerpState>>,
    meta_calibrator_snapshot: Option<OnlineMetaCalibratorSnapshot>,
    mut meta_report: Option<MetaCalibrationReport>,
) -> Result<(Vec<MarketResult>, WalkForwardSummary)> {
    let store_inner = store.store();
    let empty_spot = Arc::new(SpotHistory::default());
    let mut equity_by_strategy: HashMap<&'static str, f64> = cfg
        .strategies
        .iter()
        .map(|s| (s.name(), cfg.starting_cash_usdc))
        .collect();
    let mut peak_equity_by_strategy = equity_by_strategy.clone();
    let mut session_peak_equity_by_strategy = equity_by_strategy.clone();
    let mut session_date_by_strategy: HashMap<&'static str, String> = cfg
        .strategies
        .iter()
        .map(|s| (s.name(), String::new()))
        .collect();
    let mut daily_start_equity_by_strategy = equity_by_strategy.clone();
    let mut loss_streak_cooldown_by_strategy: HashMap<&'static str, LossStreakCooldownState> = cfg
        .strategies
        .iter()
        .map(|s| (s.name(), LossStreakCooldownState::default()))
        .collect();
    let mut shared_model_states: HashMap<&'static str, Arc<Mutex<ModelState>>> = HashMap::new();
    for strat in &cfg.strategies {
        shared_model_states.insert(
            strat.name(),
            Arc::new(Mutex::new(model_state_with_snapshot(
                meta_calibrator_snapshot.as_ref(),
            ))),
        );
    }
    let mut results: Vec<MarketResult> = Vec::with_capacity(markets.len());
    let mut oos_meta_samples: Vec<MetaTrainingSample> = Vec::with_capacity(markets.len());
    if let Some(path) = cfg.decision_log_jsonl.as_deref() {
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }

    for (idx, m) in markets.iter().enumerate() {
        let events = match load_replay_events_for_market(
            store,
            store_inner.clone(),
            m,
            MarketId(idx as u32 + 1),
            cfg.replay_event_cache_dir.as_deref(),
        )
        .await
        {
            Ok(events) => events,
            Err(e) => {
                tracing::warn!(market = %m.slug, error = %e, "tape load failed");
                continue;
            }
        };
        let sampled_events;
        let events_for_run = if cfg.replay_sample_ms > 0 {
            sampled_events = sample_replay_events(&events, cfg.replay_sample_ms);
            sampled_events.as_slice()
        } else {
            events.as_slice()
        };
        let trades = if cfg.load_pm_trades {
            match resolve_pm_trades_day(store, &m.date, &m.asset_id).await {
                Ok(tp) => match load_pm_trades_async(store_inner.clone(), tp).await {
                    Ok((ticks, _)) => Arc::new(TradeHistory::new(ticks)),
                    Err(_) => Arc::new(TradeHistory::default()),
                },
                Err(_) => Arc::new(TradeHistory::default()),
            }
        } else {
            Arc::new(TradeHistory::default())
        };
        let spot = spot_history_for_market(spot_map, &empty_spot, cfg, m);
        let resolved_yes = if cfg.use_outcome_label {
            Some(
                outcome_label_resolved_yes(&m.outcome)
                    .expect("outcome labels are validated before replay"),
            )
        } else {
            None
        };

        let mut per_strategy = HashMap::new();
        let mut captured_meta_sample_for_market = false;
        for &strat in &cfg.strategies {
            let bankroll = *equity_by_strategy
                .get(strat.name())
                .unwrap_or(&cfg.starting_cash_usdc);
            let peak_equity = *peak_equity_by_strategy
                .get(strat.name())
                .unwrap_or(&cfg.starting_cash_usdc);
            let drawdown_pct = if peak_equity > 0.0 {
                1.0 - bankroll / peak_equity
            } else {
                0.0
            };
            let global_clip_multiplier = drawdown_clip_multiplier(
                drawdown_pct,
                cfg.clip_drawdown_soft_pct,
                cfg.clip_drawdown_hard_pct,
                cfg.clip_drawdown_min_multiplier,
            );
            let session_date = session_date_by_strategy.entry(strat.name()).or_default();
            if session_date != &m.date {
                *session_date = m.date.clone();
                session_peak_equity_by_strategy.insert(strat.name(), bankroll);
                daily_start_equity_by_strategy.insert(strat.name(), bankroll);
            }
            let session_peak_equity = *session_peak_equity_by_strategy
                .get(strat.name())
                .unwrap_or(&bankroll);
            let session_drawdown_pct = if session_peak_equity > 0.0 {
                1.0 - bankroll / session_peak_equity
            } else {
                0.0
            };
            let session_clip_multiplier = drawdown_clip_multiplier(
                session_drawdown_pct,
                cfg.clip_session_drawdown_soft_pct,
                cfg.clip_session_drawdown_hard_pct,
                cfg.clip_session_drawdown_min_multiplier,
            );
            let daily_start_equity = *daily_start_equity_by_strategy
                .get(strat.name())
                .unwrap_or(&bankroll);
            let daily_loss_pct = if daily_start_equity > 0.0 {
                (daily_start_equity - bankroll) / daily_start_equity
            } else {
                0.0
            };
            let remaining_daily_loss_budget = daily_remaining_loss_budget_usdc(
                daily_start_equity,
                bankroll,
                cfg.daily_loss_cap_pct,
            );
            let daily_clip_multiplier = if remaining_daily_loss_budget == Some(0.0)
                || (cfg.daily_loss_cap_pct < 1.0 && daily_loss_pct >= cfg.daily_loss_cap_pct)
            {
                0.0
            } else {
                1.0
            };
            let loss_streak_cooldown_active = loss_streak_cooldown_by_strategy
                .get(strat.name())
                .is_some_and(LossStreakCooldownState::is_active);
            let loss_streak_clip_multiplier = if loss_streak_cooldown_active {
                0.0
            } else {
                1.0
            };
            let max_per_market_exposure_usdc = remaining_daily_loss_budget
                .map(|remaining| per_market_exposure_cap(cfg, bankroll).min(remaining))
                .unwrap_or_else(|| per_market_exposure_cap(cfg, bankroll));
            let clip_multiplier = global_clip_multiplier
                .min(session_clip_multiplier)
                .min(daily_clip_multiplier)
                .min(loss_streak_clip_multiplier);
            // Per-market clip: a fraction of current equity (compounds), else
            // static fallback. Hard floor + ceiling for sanity.
            let clip = match cfg.clip_fraction_of_equity {
                Some(frac) => compounded_clip(bankroll, frac),
                None => cfg.max_clip_usdc,
            } * clip_multiplier;
            let runner_cfg = RunnerConfig {
                starting_cash_usdc: bankroll,
                market_open_ns: market_open_ns(&m),
                market_close_ns: market_close_ns(&m),
                resolved_yes,
                portfolio_limits: PortfolioLimits {
                    max_clip_usdc: clip * cfg.max_order_clip_multiplier,
                    max_per_market_exposure_usdc,
                    max_daily_exposure_usdc: bankroll * 5.0,
                    ..PortfolioLimits::default()
                },
                current_btc_net_shares: 0.0,
                current_eth_net_shares: 0.0,
                equity_curve_jsonl: None,
                snapshot_every_n: 1_000_000,
                maker_rebate_bps: cfg.maker_rebate_bps,
                taker_fee_bps: cfg.taker_fee_bps,
                decision_log_jsonl: cfg.decision_log_jsonl.clone(),
                decision_log_parquet: None,
                strategy_name: strat.name().to_string(),
                shared_model_state: shared_model_states.get(strat.name()).cloned(),
                update_model_state_on_resolution: meta_calibrator_snapshot.is_none(),
                meta_calibrator_snapshot: meta_calibrator_snapshot.clone(),
                enable_meta_calibration: cfg.enable_meta_calibration,
                model_market_context: model_market_context_for_cfg(cfg, &m),
                prior_market_range_1d: prior_market_range_mean(&results, 288),
                prior_market_range_3d: prior_market_range_mean(&results, 3 * 288),
                prior_market_range_7d: prior_market_range_mean(&results, 7 * 288),
                model_btc_whipsaw_risk_weight: cfg.model_btc_whipsaw_risk_weight,
                model_btc_path_inefficiency_risk_weight: cfg
                    .model_btc_path_inefficiency_risk_weight,
                model_btc_reversal_pressure_risk_weight: cfg
                    .model_btc_reversal_pressure_risk_weight,
                decision_log_every_n: cfg.decision_log_every_n,
                max_inventory_imbalance_shares: 1.5,
                taker_slippage_bps: 15.0,
                taker_latency_ms: cfg.taker_latency_ms,
                enforce_model_gate: cfg.enforce_model_gate,
                model_gate_min_confidence: cfg.model_gate_min_confidence,
                model_gate_max_risk: cfg.model_gate_max_risk,
                model_gate_min_edge: cfg.model_gate_min_edge,
                daily_start_cash_usdc: daily_start_equity,
                daily_loss_cap_pct: cfg.daily_loss_cap_pct,
                current_daily_loss_pct: daily_loss_pct,
            };
            match run_one_strategy(
                strat,
                cfg,
                &m.slug,
                events_for_run,
                &spot,
                &trades,
                &runner_cfg,
                bankroll,
                clip,
                perp.clone(),
            ) {
                Ok(mut r) => {
                    for sample in &mut r.model_training_samples {
                        sample.market_idx = idx as u32;
                    }
                    if !captured_meta_sample_for_market && !r.model_training_samples.is_empty() {
                        oos_meta_samples.extend(r.model_training_samples.iter().copied());
                        captured_meta_sample_for_market = true;
                    }
                    equity_by_strategy.insert(strat.name(), r.end_equity_usdc);
                    peak_equity_by_strategy
                        .entry(strat.name())
                        .and_modify(|peak| *peak = peak.max(r.end_equity_usdc))
                        .or_insert(r.end_equity_usdc);
                    session_peak_equity_by_strategy
                        .entry(strat.name())
                        .and_modify(|peak| *peak = peak.max(r.end_equity_usdc))
                        .or_insert(r.end_equity_usdc);
                    if let Some(state) = loss_streak_cooldown_by_strategy.get_mut(strat.name()) {
                        if loss_streak_cooldown_active {
                            state.consume_cooldown_market();
                        } else {
                            state.record_completed_market(
                                r.orders_filled > 0,
                                r.pnl_usdc,
                                cfg.loss_streak_loss_threshold_usdc,
                                cfg.loss_streak_cooldown_after,
                                cfg.loss_streak_cooldown_markets,
                            );
                        }
                    }
                    per_strategy.insert(strat.name(), r);
                }
                Err(e) => {
                    tracing::warn!(market = %m.slug, strategy = strat.name(), error = %e, "strategy run failed");
                }
            }
        }

        let volatility_range = market_volatility_range(events_for_run);
        let volatility_band = volatility_band(volatility_range, cfg.volatility_regime_threshold);

        if (idx + 1) % 50 == 0 {
            let equity_strs: Vec<String> = cfg
                .strategies
                .iter()
                .map(|s| {
                    format!(
                        "{}={:.2}",
                        s.name(),
                        equity_by_strategy.get(s.name()).copied().unwrap_or(0.0)
                    )
                })
                .collect();
            tracing::info!(
                done = idx + 1,
                total = markets.len(),
                equity = %equity_strs.join(" "),
                "portfolio progress",
            );
        }

        results.push(MarketResult {
            asset_id: m.asset_id.clone(),
            slug: m.slug.clone(),
            close_ts: market_close_ts(&m),
            outcome_label: m.outcome.clone(),
            volatility_range,
            volatility_band,
            per_strategy,
        });

        if cfg.portfolio_checkpoint_every_markets > 0
            && results.len() % cfg.portfolio_checkpoint_every_markets == 0
        {
            write_portfolio_checkpoint(
                cfg,
                &results,
                meta_report.as_ref(),
                meta_calibrator_snapshot.as_ref(),
                &oos_meta_samples,
            )
            .with_context(|| {
                format!("write portfolio checkpoint after {} markets", results.len())
            })?;
        }
    }

    let mut summary = aggregate(&results, &cfg.strategies);
    summary.config_fingerprint = Some(config_fingerprint(cfg));
    summary.run_config = Some(summary_run_config(cfg));
    if let Some(report) = meta_report.as_mut() {
        report.oos_samples = oos_meta_samples.len();
        if let Some(snapshot) = meta_calibrator_snapshot.as_ref() {
            let filtered_oos_samples = filter_meta_samples_for_training(
                &oos_meta_samples,
                MetaSampleLimits::from_config(cfg),
            );
            let evaluation_samples = market_balanced_meta_samples(
                &filtered_oos_samples,
                cfg.meta_max_oos_evaluation_samples,
                cfg.meta_max_samples_per_market,
            );
            report.oos_evaluation_samples = evaluation_samples.len();
            report.oos = Some(evaluate_meta_calibration(snapshot, &evaluation_samples));
        }
        summary.meta_calibration = meta_report;
    }
    Ok((results, summary))
}


fn model_state_with_snapshot(snapshot: Option<&OnlineMetaCalibratorSnapshot>) -> ModelState {
    let mut state = ModelState::new();
    if let Some(snapshot) = snapshot {
        state.load_meta_calibrator_snapshot(snapshot.clone());
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounting::{MarketResult, StrategyMarketResult, market_close_ts, market_open_ts, validate_outcome_labels};
    use crate::config::{MarketHandle, WalkForwardConfig};
    use crate::portfolio::{LossStreakCooldownState, VolatilityBand, compounded_clip, daily_remaining_loss_budget_usdc, drawdown_clip_multiplier, previous_date, resolve_perp_symbol};
    use crate::scorecard::{aggregate, aggregate_for_strategy, summary_run_config};
    use std::collections::HashMap;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tmp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{nanos}"))
    }

    #[test]
    fn market_close_ts_normalizes_legacy_open_timestamp_rows() {
        let mut market = MarketHandle {
            asset_id: "asset".to_string(),
            slug: "btc-updown-5m-1778763000".to_string(),
            close_ts: 1_778_763_000,
            outcome: "Up".to_string(),
            date: "2026-05-14".to_string(),
        };

        assert_eq!(market_open_ts(&market), 1_778_763_000);
        assert_eq!(market_close_ts(&market), 1_778_763_300);

        market.close_ts = 1_778_763_300;
        assert_eq!(market_close_ts(&market), 1_778_763_300);
    }

    #[test]
    fn market_close_ts_uses_slug_duration_for_longer_horizons() {
        let mut market = MarketHandle {
            asset_id: "asset".to_string(),
            slug: "btc-updown-15m-1778763000".to_string(),
            close_ts: 1_778_763_000,
            outcome: "Up".to_string(),
            date: "2026-05-14".to_string(),
        };

        assert_eq!(market_open_ts(&market), 1_778_763_000);
        assert_eq!(market_close_ts(&market), 1_778_763_900);

        market.close_ts = 1_778_763_900;
        assert_eq!(market_close_ts(&market), 1_778_763_900);

        market.slug = "eth-updown-4h-1778760000".to_string();
        market.close_ts = 1_778_760_000;
        assert_eq!(market_open_ts(&market), 1_778_760_000);
        assert_eq!(market_close_ts(&market), 1_778_774_400);
    }

    #[test]
    fn outcome_label_validation_rejects_unknown_labels() {
        let markets = vec![MarketHandle {
            asset_id: "asset".to_string(),
            slug: "btc-updown-5m-1778763000".to_string(),
            close_ts: 1_778_763_300,
            outcome: "Unknown".to_string(),
            date: "2026-05-14".to_string(),
        }];

        let err = validate_outcome_labels(&markets).expect_err("Unknown must fail closed");
        assert!(err.to_string().contains("requires explicit Up/Down"));
    }

    #[test]
    fn outcome_label_validation_accepts_yes_no_and_up_down() {
        let markets = ["Up", "Down", "Yes", "No"]
            .into_iter()
            .enumerate()
            .map(|(idx, outcome)| MarketHandle {
                asset_id: format!("asset-{idx}"),
                slug: format!("btc-updown-5m-{}", 1_778_763_000 + idx as i64 * 300),
                close_ts: 1_778_763_300 + idx as i64 * 300,
                outcome: outcome.to_string(),
                date: "2026-05-14".to_string(),
            })
            .collect::<Vec<_>>();

        validate_outcome_labels(&markets).expect("explicit labels should pass");
    }

    #[test]
    fn summary_run_config_keeps_archived_knobs_out_of_shared_config() {
        let mut cfg = WalkForwardConfig::default();
        cfg.strategies = vec![StratId::ExoFade];

        let value = serde_json::to_value(summary_run_config(&cfg)).unwrap();
        let shared = value.get("shared").expect("shared config missing");
        let strategies = value
            .get("strategies")
            .and_then(|v| v.as_array())
            .expect("strategy config missing");

        assert!(shared.get("spot_symbol").is_none());
        assert!(shared.get("model_btc_whipsaw_risk_weight").is_none());
        assert!(
            shared
                .get("model_btc_path_inefficiency_risk_weight")
                .is_none()
        );
        assert!(
            shared
                .get("model_btc_reversal_pressure_risk_weight")
                .is_none()
        );
        assert_eq!(shared.get("spot_symbol_mode").unwrap(), "auto");
        assert!(shared.get("spot_symbol_override").is_none());
        assert!(shared.get("model_spot_whipsaw_risk_weight").is_some());
        assert!(
            shared
                .get("model_spot_path_inefficiency_risk_weight")
                .is_some()
        );
        assert!(
            shared
                .get("model_spot_reversal_pressure_risk_weight")
                .is_some()
        );
        assert_eq!(strategies[0].get("strategy").unwrap(), "exo_fade");
        assert!(strategies[0].get("config").is_none());
    }

    #[test]
    fn replay_event_cache_round_trips_jsonl() {
        let root = unique_tmp_dir("pm-replay-cache-test");
        let path = root.join("events.jsonl");
        let event = pm_types::ReplayEvent {
            ts_ns: 123,
            market_id: MarketId(7),
            yes_mid: 0.51,
            yes_bid: 0.50,
            yes_ask: 0.52,
            volume: 10.0,
            bids: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            asks: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            spot_price: 105_000.0,
            flags: pm_types::ReplayFlags::BOOK_UPDATE,
        };

        write_replay_event_cache(&path, &[event]).expect("write replay cache");
        let loaded = read_replay_event_cache(&path).expect("read replay cache");
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(loaded, vec![event]);
    }

    #[test]
    fn replay_event_cache_market_ids_are_rebound_for_current_run() {
        let mut events = vec![pm_types::ReplayEvent {
            ts_ns: 123,
            market_id: MarketId(7),
            yes_mid: 0.51,
            yes_bid: 0.50,
            yes_ask: 0.52,
            volume: 10.0,
            bids: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            asks: [pm_types::BookLevel::default(); pm_types::tape::TAPE_DEPTH],
            spot_price: 105_000.0,
            flags: pm_types::ReplayFlags::BOOK_UPDATE,
        }];

        rebind_replay_event_market_ids(&mut events, MarketId(99));

        assert_eq!(events[0].market_id, MarketId(99));
    }

    #[test]
    fn aggregate_handles_per_strategy_metrics() {
        let mut result_reactive = HashMap::new();
        result_reactive.insert(
            StratId::ExoFade.name(),
            StrategyMarketResult {
                orders_submitted: 10,
                orders_filled: 6,
                orders_filled_taker: 4,
                orders_filled_maker: 2,
                orders_rejected_model_gate: 0,
                orders_rejected_model_gate_confidence: 0,
                orders_rejected_model_gate_risk: 0,
                orders_rejected_model_gate_edge: 0,
                pnl_usdc: 4.0,
                start_equity_usdc: 100.0,
                end_equity_usdc: 104.0,
                max_drawdown_pct: 0.12,
                fills: 6,
                maker_rebates_usdc: 0.1,
                requested_shares: 12.0,
                filled_shares: 10.0,
                fill_shares_ratio: 10.0 / 12.0,
                requested_notional_usdc: 60.0,
                filled_notional_usdc: 54.0,
                fill_notional_ratio: 0.9,
                avg_slippage_bps: 12.0,
                clip_used_usdc: 2.0,
                yes_resolved: true,
                fills_detail: vec![],
                model_training_samples: vec![],
            },
        );

        let mut result_noop = HashMap::new();
        result_noop.insert(
            StratId::Noop.name(),
            StrategyMarketResult {
                orders_submitted: 3,
                orders_filled: 1,
                orders_filled_taker: 1,
                orders_filled_maker: 0,
                orders_rejected_model_gate: 0,
                orders_rejected_model_gate_confidence: 0,
                orders_rejected_model_gate_risk: 0,
                orders_rejected_model_gate_edge: 0,
                pnl_usdc: -2.0,
                start_equity_usdc: 100.0,
                end_equity_usdc: 98.0,
                max_drawdown_pct: 0.05,
                fills: 1,
                maker_rebates_usdc: 0.0,
                requested_shares: 2.0,
                filled_shares: 2.0,
                fill_shares_ratio: 1.0,
                requested_notional_usdc: 10.0,
                filled_notional_usdc: 10.0,
                fill_notional_ratio: 1.0,
                avg_slippage_bps: 15.0,
                clip_used_usdc: 1.5,
                yes_resolved: false,
                fills_detail: vec![],
                model_training_samples: vec![],
            },
        );

        let mut result_no_order = HashMap::new();
        result_no_order.insert(
            StratId::Noop.name(),
            StrategyMarketResult {
                orders_submitted: 0,
                orders_filled: 0,
                orders_filled_taker: 0,
                orders_filled_maker: 0,
                orders_rejected_model_gate: 0,
                orders_rejected_model_gate_confidence: 0,
                orders_rejected_model_gate_risk: 0,
                orders_rejected_model_gate_edge: 0,
                pnl_usdc: 0.0,
                start_equity_usdc: 100.0,
                end_equity_usdc: 100.0,
                max_drawdown_pct: 0.0,
                fills: 0,
                maker_rebates_usdc: 0.0,
                requested_shares: 0.0,
                filled_shares: 0.0,
                fill_shares_ratio: 0.0,
                requested_notional_usdc: 0.0,
                filled_notional_usdc: 0.0,
                fill_notional_ratio: 0.0,
                avg_slippage_bps: 0.0,
                clip_used_usdc: 3.0,
                yes_resolved: true,
                fills_detail: vec![],
                model_training_samples: vec![],
            },
        );

        let results = vec![
            MarketResult {
                asset_id: "1".to_string(),
                slug: "a".to_string(),
                close_ts: 0,
                outcome_label: "Yes".to_string(),
                volatility_range: 0.02,
                volatility_band: VolatilityBand::Low,
                per_strategy: result_reactive,
            },
            MarketResult {
                asset_id: "2".to_string(),
                slug: "b".to_string(),
                close_ts: 0,
                outcome_label: "No".to_string(),
                volatility_range: 0.20,
                volatility_band: VolatilityBand::High,
                per_strategy: result_noop,
            },
            MarketResult {
                asset_id: "3".to_string(),
                slug: "c".to_string(),
                close_ts: 0,
                outcome_label: "Yes".to_string(),
                volatility_range: 0.10,
                volatility_band: VolatilityBand::Low,
                per_strategy: result_no_order,
            },
        ];

        let summary = aggregate(&results, &[StratId::ExoFade, StratId::Noop]);
        assert_eq!(summary.markets_attempted, 3);
        assert_eq!(summary.markets_succeeded, 3);

        let reactive = summary
            .per_strategy
            .get(StratId::ExoFade.name())
            .expect("exo_fade missing");
        assert_eq!(reactive.markets_with_orders, 1);
        assert_eq!(reactive.total_orders_filled, 6);
        assert_eq!(reactive.total_orders_filled_taker, 4);
        assert_eq!(reactive.total_orders_filled_maker, 2);
        assert!((reactive.fill_shares_ratio - 10.0 / 12.0).abs() < f64::EPSILON);
        assert!((reactive.fill_notional_ratio - 0.9).abs() < f64::EPSILON);
        assert!((reactive.avg_slippage_bps - 12.0).abs() < f64::EPSILON);
        assert_eq!(reactive.best_market_pnl, 4.0);
        assert_eq!(reactive.worst_market_pnl, 4.0);
        assert!((reactive.hit_rate - 1.0).abs() < f64::EPSILON);

        let no_order = summary
            .per_strategy
            .get(StratId::Noop.name())
            .expect("noop missing");
        assert_eq!(no_order.markets_with_orders, 1);
        assert_eq!(no_order.total_orders_filled, 1);

        let low = summary
            .by_volatility_band
            .get(&VolatilityBand::Low)
            .expect("low band missing");
        let low_reactive = low
            .get(StratId::ExoFade.name())
            .expect("low exo_fade missing");
        assert_eq!(low_reactive.total_pnl_usdc, 4.0);

        let high = summary
            .by_volatility_band
            .get(&VolatilityBand::High)
            .expect("high band missing");
        let high_noop = high
            .get(StratId::Noop.name())
            .expect("high noop missing");
        assert_eq!(high_noop.total_pnl_usdc, -2.0);
        assert_eq!(reactive.sharpe_ratio, 0.0);
    }

    fn test_fill(
        side: &str,
        side_model_p: f32,
        notional: f64,
        range: f32,
        whipsaw: f32,
    ) -> Fill {
        Fill {
            ts_ns: 0,
            side: side.to_string(),
            shares: 10.0,
            price: (notional / 10.0) as f32,
            notional,
            tag: "test".to_string(),
            maker: false,
            rebate_usdc: 0.0,
            slippage_bps: 0.0,
            yes_mid: Some(0.50),
            yes_bid: Some(0.49),
            yes_ask: Some(0.51),
            side_model_p: Some(side_model_p),
            side_edge_vs_mid: Some(side_model_p - 0.50),
            side_edge_vs_fill: Some(side_model_p - (notional / 10.0) as f32),
            direction_score: Some(0.40),
            confidence_score: Some(0.75),
            calibrated_p: Some(side_model_p),
            risk_score: Some(0.30),
            market_yes_range_so_far: Some(range),
            seconds_since_open: Some(240.0),
            seconds_to_close: Some(60.0),
            regime_whipsaw_score: Some(whipsaw),
            regime_path_efficiency: Some(0.70),
            regime_reversal_pressure: Some(0.20),
            regime_sign_flip_rate: Some(0.10),
            regime_realized_vol_180s_bps: Some(25.0),
            binance_flow_imbal_5s: None,
            binance_flow_imbal_15s: None,
            binance_flow_imbal_30s: None,
            binance_adverse_vol_5s: None,
            binance_adverse_vol_15s: None,
            binance_adverse_vol_30s: None,
            binance_large_adverse_count_10s: None,
            binance_trade_intensity_15s: None,
            spot_ret_5s: None,
            spot_ret_15s: None,
            spot_ret_30s: None,
            spot_accel_15s_vs_30s: None,
            spot_accel_5s_vs_15s: None,
            post_fill_path: None,
        }
    }

    fn strategy_result_with_fills(
        yes_resolved: bool,
        fills_detail: Vec<Fill>,
    ) -> StrategyMarketResult {
        let filled_notional_usdc = fills_detail.iter().map(|fill| fill.notional).sum::<f64>();
        let filled_shares = fills_detail.iter().map(|fill| fill.shares).sum::<f64>();
        StrategyMarketResult {
            orders_submitted: fills_detail.len(),
            orders_filled: fills_detail.len(),
            orders_filled_taker: fills_detail.len(),
            orders_filled_maker: 0,
            orders_rejected_model_gate: 0,
            orders_rejected_model_gate_confidence: 0,
            orders_rejected_model_gate_risk: 0,
            orders_rejected_model_gate_edge: 0,
            pnl_usdc: 0.0,
            start_equity_usdc: 100.0,
            end_equity_usdc: 100.0,
            max_drawdown_pct: 0.0,
            fills: fills_detail.len(),
            maker_rebates_usdc: 0.0,
            requested_shares: filled_shares,
            filled_shares,
            fill_shares_ratio: if filled_shares > 0.0 { 1.0 } else { 0.0 },
            requested_notional_usdc: filled_notional_usdc,
            filled_notional_usdc,
            fill_notional_ratio: if filled_notional_usdc > 0.0 { 1.0 } else { 0.0 },
            avg_slippage_bps: 0.0,
            clip_used_usdc: filled_notional_usdc,
            yes_resolved,
            fills_detail,
            model_training_samples: vec![],
        }
    }

    #[test]
    fn aggregate_reports_model_fill_quality() {
        let win_yes = test_fill("BuyYes", 0.80, 8.0, 0.60, 0.40);
        let win_no = test_fill("BuyNo", 0.70, 7.0, 0.40, 0.20);
        let lose_yes = test_fill("BuyYes", 0.90, 9.0, 0.60, 0.50);
        let first = strategy_result_with_fills(true, vec![win_yes]);
        let second = strategy_result_with_fills(false, vec![win_no, lose_yes]);
        let records = vec![&first, &second];

        let agg = aggregate_for_strategy(&records);
        let quality = &agg.model_fill_quality;

        assert_eq!(quality.all.fills, 3);
        assert!((quality.all.hit_rate - 2.0 / 3.0).abs() < 1e-12);
        assert!((quality.all.avg_predicted_p - 0.80).abs() < 1e-6);
        assert!((quality.all.brier - ((0.04 + 0.09 + 0.81) / 3.0)).abs() < 1e-6);
        assert!(quality.all.log_loss > 0.95 && quality.all.log_loss < 0.97);
        assert_eq!(quality.range_ge_050.fills, 2);
        assert_eq!(quality.range_lt_050.fills, 1);
        assert_eq!(quality.whipsaw_ge_035.fills, 2);
        assert_eq!(quality.whipsaw_lt_035.fills, 1);
        assert_eq!(quality.market_samples, 2);
        assert!((quality.market_majority_side_accuracy - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn drawdown_clip_multiplier_can_keep_recovery_floor() {
        assert!((drawdown_clip_multiplier(0.10, 0.20, 0.40, 0.10) - 1.0).abs() < 1e-12);
        assert!((drawdown_clip_multiplier(0.30, 0.20, 0.40, 0.10) - 0.55).abs() < 1e-12);
        assert!((drawdown_clip_multiplier(0.50, 0.20, 0.40, 0.10) - 0.10).abs() < 1e-12);
    }

    #[test]
    fn drawdown_clip_multiplier_preserves_zero_hard_stop_by_default() {
        assert!((drawdown_clip_multiplier(0.50, 0.20, 0.40, 0.0) - 0.0).abs() < 1e-12);
    }

    #[test]
    fn daily_remaining_loss_budget_caps_next_market_exposure() {
        assert_eq!(daily_remaining_loss_budget_usdc(2700.0, 2700.0, 1.0), None);
        assert_eq!(
            daily_remaining_loss_budget_usdc(2700.0, 2700.0, 0.05),
            Some(135.0)
        );
        assert!(
            (daily_remaining_loss_budget_usdc(2700.0, 2573.0, 0.05).unwrap() - 8.0).abs() < 1e-12
        );
        assert_eq!(
            daily_remaining_loss_budget_usdc(2700.0, 2542.0, 0.05),
            Some(0.0)
        );
    }

    #[test]
    fn loss_streak_cooldown_uses_only_completed_traded_markets() {
        let mut state = LossStreakCooldownState::default();
        state.record_completed_market(true, -1.0, 0.0, 2, 2);
        assert!(!state.is_active());
        state.record_completed_market(false, -10.0, 0.0, 2, 2);
        assert!(!state.is_active());
        state.record_completed_market(true, -0.01, 0.0, 2, 2);
        assert!(state.is_active());

        state.consume_cooldown_market();
        assert!(state.is_active());
        state.consume_cooldown_market();
        assert!(!state.is_active());
        state.record_completed_market(true, 0.01, 0.0, 2, 2);
        assert_eq!(state.consecutive_losses, 0);
    }

    #[test]
    fn fold_plan_with_fold_size() {
        let cfg = WalkForwardConfig {
            fold_size: Some(3),
            ..WalkForwardConfig::default()
        };
        let plan = build_fold_plan(10, &cfg).expect("plan");
        assert_eq!(plan, vec![(0, 0, 3), (3, 3, 6), (6, 6, 9), (9, 9, 10)]);
    }

    #[test]
    fn fold_plan_skips_until_min_train_markets() {
        let cfg = WalkForwardConfig {
            fold_size: Some(3),
            min_train_markets: 6,
            ..WalkForwardConfig::default()
        };
        let plan = build_fold_plan(10, &cfg).expect("plan");
        assert_eq!(plan, vec![(6, 6, 9), (9, 9, 10)]);
    }

    #[test]
    fn fold_plan_errors_when_min_train_markets_impossible() {
        let cfg = WalkForwardConfig {
            fold_size: Some(3),
            min_train_markets: 12,
            ..WalkForwardConfig::default()
        };
        let err = build_fold_plan(10, &cfg).expect_err("should reject impossible min train");
        assert!(
            err.to_string().contains("no walk-forward folds satisfy"),
            "{err}"
        );
    }

    #[test]
    fn fold_plan_with_folds_and_purge() {
        let cfg = WalkForwardConfig {
            walk_forward_folds: Some(2),
            purge_markets: 2,
            ..WalkForwardConfig::default()
        };
        let plan = build_fold_plan(10, &cfg).expect("plan");
        assert_eq!(plan, vec![(0, 0, 5), (3, 5, 10)]);
    }

    #[test]
    fn no_fold_config_uses_single_window() {
        let cfg = WalkForwardConfig::default();
        let plan = build_fold_plan(7, &cfg).expect("plan");
        assert_eq!(plan, vec![(0, 0, 7)]);
    }

    #[test]
    fn compounded_clip_does_not_panic_on_tiny_bankroll() {
        assert_eq!(compounded_clip(0.16, 0.02), 0.0032);
        assert_eq!(compounded_clip(0.0, 0.02), 0.0);
        assert_eq!(compounded_clip(1000.0, 0.02), 20.0);
    }

    #[test]
    fn sample_replay_events_keeps_latest_tick_per_interval() {
        fn event(ts_ms: i64, mid: f32) -> pm_types::ReplayEvent {
            pm_types::ReplayEvent {
                ts_ns: ts_ms * 1_000_000,
                market_id: pm_types::MarketId(1),
                yes_mid: mid,
                yes_bid: mid - 0.01,
                yes_ask: mid + 0.01,
                volume: 0.0,
                bids: [pm_types::BookLevel::default(); pm_types::TAPE_DEPTH],
                asks: [pm_types::BookLevel::default(); pm_types::TAPE_DEPTH],
                spot_price: 100.0,
                flags: pm_types::ReplayFlags::BOOK_UPDATE,
            }
        }

        let events = vec![
            event(0, 0.50),
            event(100, 0.51),
            event(900, 0.52),
            event(1100, 0.53),
            event(1900, 0.54),
            event(2100, 0.55),
            event(3000, 0.56),
        ];

        let sampled = sample_replay_events(&events, 1000);
        let mids: Vec<_> = sampled.iter().map(|event| event.yes_mid).collect();
        assert_eq!(mids, vec![0.50, 0.52, 0.54, 0.55, 0.56]);
    }

    #[test]
    fn market_balanced_meta_samples_caps_each_market_and_total() {
        let mut samples = Vec::new();
        for market_idx in 0..5 {
            for sample_idx in 0..10 {
                let mut features = pm_model::MetaFeatures::default();
                features.values[0] = sample_idx as f32;
                samples.push(MetaTrainingSample {
                    features,
                    market_idx,
                    base_side_probability: 0.5,
                    side_observed: market_idx % 2 == 0,
                });
            }
        }

        let selected = market_balanced_meta_samples(&samples, 12, 4);
        assert_eq!(selected.len(), 10);
        for market_idx in 0..5 {
            let count = selected
                .iter()
                .filter(|sample| sample.market_idx == market_idx)
                .count();
            assert_eq!(count, 2);
        }
    }

    #[test]
    fn split_meta_samples_by_market_uses_chronological_markets() {
        let samples: Vec<MetaTrainingSample> = (0..4)
            .flat_map(|market_idx| {
                (0..3).map(move |_| MetaTrainingSample {
                    features: pm_model::MetaFeatures::default(),
                    market_idx,
                    base_side_probability: 0.5,
                    side_observed: market_idx % 2 == 0,
                })
            })
            .collect();

        let (fit, validation) = split_meta_samples_by_market(&samples, 0.5);
        assert_eq!(fit.len(), 6);
        assert_eq!(validation.len(), 6);
        assert!(fit.iter().all(|sample| sample.market_idx <= 1));
        assert!(validation.iter().all(|sample| sample.market_idx >= 2));
    }

    #[test]
    fn previous_date_handles_month_boundary() {
        assert_eq!(
            previous_date("2026-03-01").unwrap(),
            Some("2026-02-28".to_string())
        );
    }

    #[test]
    fn resolve_perp_symbol_defaults_to_spot_for_exo_fade() {
        let mut cfg = WalkForwardConfig::default();
        cfg.spot_symbol = "BTCUSDT".into();
        cfg.strategies = vec![StratId::ExoFade];
        assert_eq!(resolve_perp_symbol(&cfg).as_deref(), Some("BTCUSDT"));

        cfg.perp_symbol = Some("ETHUSDT".into());
        assert_eq!(resolve_perp_symbol(&cfg).as_deref(), Some("ETHUSDT"));

        cfg.perp_symbol = None;
        cfg.spot_symbol = "auto".into();
        assert!(resolve_perp_symbol(&cfg).is_none());

        cfg.strategies = vec![StratId::Noop];
        cfg.spot_symbol = "BTCUSDT".into();
        assert!(resolve_perp_symbol(&cfg).is_none());
    }
}
