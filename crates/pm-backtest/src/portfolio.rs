//! Portfolio-mode state: spot/perp caches, drawdown and exposure clip
//! sizing, loss-streak cooldowns, and the perp-complex cache loaders
//! (moved whole from pm-app's `perp.rs`).

use anyhow::{Context, Result, anyhow};
use chrono::{Duration, NaiveDate, NaiveDateTime};
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use pm_alpha::PerpState;
use pm_model::{MarketAsset, MetaTrainingConfig, ModelMarketContext};
use pm_telonex_loader::{TelonexStore, load_binance_agg_trades_async, resolve_binance_day};
use pm_types::{SpotHistory, SpotTick};
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config::{MarketHandle, WalkForwardConfig, spot_cache_key, spot_symbol_for_market};
use crate::engine::StratId;

fn metrics_path(cache_dir: &Path, symbol: &str, date: &str) -> PathBuf {
    cache_dir.join(format!(
        "raw/binance/exchange=binance/channel=futures_metrics/symbol={symbol}/date={date}/{symbol}-metrics-{date}.parquet"
    ))
}

fn funding_dir(cache_dir: &Path, symbol: &str) -> PathBuf {
    cache_dir.join(format!(
        "raw/binance/exchange=binance/channel=futures_funding/symbol={symbol}"
    ))
}

/// Read one day's 5-minute OI series: (ts_ns, sum_open_interest).
fn read_metrics_day(path: &Path) -> Result<Vec<(i64, f64)>> {
    let reader = SerializedFileReader::new(File::open(path)?)?;
    let schema = reader.metadata().file_metadata().schema_descr();
    let mut col_time = None;
    let mut col_oi = None;
    for (i, c) in schema.columns().iter().enumerate() {
        match c.name() {
            "create_time" => col_time = Some(i),
            "sum_open_interest" => col_oi = Some(i),
            _ => {}
        }
    }
    let (ct, co) = (
        col_time.context("create_time column")?,
        col_oi.context("sum_open_interest column")?,
    );
    let mut out = Vec::new();
    for row in reader.get_row_iter(None)? {
        let row = row?;
        // pyarrow may have written create_time as either a string or a
        // timestamp column depending on CSV inference; accept both.
        let ts_ns = if let Ok(ts) = row.get_string(ct) {
            NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
                .with_context(|| format!("parse create_time {ts}"))?
                .and_utc()
                .timestamp_nanos_opt()
                .unwrap_or(0)
        } else if let Ok(ms) = row.get_timestamp_millis(ct) {
            ms.saturating_mul(1_000_000)
        } else if let Ok(us) = row.get_timestamp_micros(ct) {
            us.saturating_mul(1_000)
        } else {
            continue;
        };
        let oi: f64 = match (row.get_string(co), row.get_double(co)) {
            (Ok(v), _) => v.parse().unwrap_or(f64::NAN),
            (_, Ok(v)) => v,
            _ => continue,
        };
        if !oi.is_finite() {
            continue;
        }
        out.push((ts_ns, oi));
    }
    Ok(out)
}

/// Read all funding events for a symbol: (ts_ns, rate).
fn read_funding(cache_dir: &Path, symbol: &str) -> Result<Vec<(i64, f64)>> {
    let mut out = Vec::new();
    let dir = funding_dir(cache_dir, symbol);
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir)? {
        let p = entry?.path();
        if p.extension().and_then(|e| e.to_str()) != Some("parquet") {
            continue;
        }
        let reader = SerializedFileReader::new(File::open(&p)?)?;
        let schema = reader.metadata().file_metadata().schema_descr();
        let mut col_time = None;
        let mut col_rate = None;
        for (i, c) in schema.columns().iter().enumerate() {
            match c.name() {
                "calc_time" => col_time = Some(i),
                "last_funding_rate" => col_rate = Some(i),
                _ => {}
            }
        }
        let (ct, cr) = (
            col_time.context("calc_time column")?,
            col_rate.context("last_funding_rate column")?,
        );
        for row in reader.get_row_iter(None)? {
            let row = row?;
            let ts_ms = row.get_long(ct)?;
            let rate = row.get_double(cr)?;
            out.push((ts_ms.saturating_mul(1_000_000), rate));
        }
    }
    out.sort_by_key(|(t, _)| *t);
    Ok(out)
}

/// Load the full PerpState for one symbol over a list of dates (plus the
/// prior date's trades for lookback warmup, matching SpotCache behavior).
pub async fn load_perp_state(
    store: &TelonexStore,
    cache_dir: &Path,
    symbol: &str,
    dates: &[String],
) -> Result<PerpState> {
    let mut ticks = Vec::new();
    for date in dates {
        match resolve_binance_day(store, "futures_agg_trades", symbol, date).await {
            Ok(path) => {
                let (day_ticks, _) = load_binance_agg_trades_async(store.store(), path).await?;
                ticks.extend(day_ticks);
            }
            Err(err) => {
                tracing::warn!(symbol, date, error = %err, "perp trades day missing");
            }
        }
    }
    let mut oi = Vec::new();
    for date in dates {
        let p = metrics_path(cache_dir, symbol, date);
        if p.exists() {
            oi.extend(read_metrics_day(&p)?);
        }
    }
    oi.sort_by_key(|(t, _)| *t);
    let funding = read_funding(cache_dir, symbol)?;
    tracing::info!(
        symbol,
        days = dates.len(),
        trades = ticks.len(),
        oi_rows = oi.len(),
        funding_rows = funding.len(),
        "perp state loaded"
    );
    Ok(PerpState {
        trades: pm_types::SpotHistory::new(ticks),
        oi,
        funding,
    })
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub enum VolatilityBand {
    Low,
    High,
}


impl VolatilityBand {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "range_le_threshold",
            Self::High => "range_gt_threshold",
        }
    }
}


#[derive(Debug, Clone, Serialize)]
pub struct SharedRunConfig {
    pub starting_cash_usdc: f64,
    pub kelly_fraction: f64,
    pub max_clip_usdc: f64,
    pub max_order_clip_multiplier: f64,
    pub max_per_market_exposure_usdc: f64,
    pub max_per_market_exposure_frac: Option<f64>,
    pub spot_symbol_mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spot_symbol_override: Option<String>,
    pub portfolio_mode: bool,
    pub max_concurrent_fetches: usize,
    pub replay_sample_ms: u64,
    pub taker_latency_ms: u64,
    pub replay_event_cache_dir: Option<String>,
    pub load_pm_trades: bool,
    pub clip_fraction_of_equity: Option<f64>,
    pub clip_drawdown_soft_pct: f64,
    pub clip_drawdown_hard_pct: f64,
    pub clip_drawdown_min_multiplier: f64,
    pub clip_session_drawdown_soft_pct: f64,
    pub clip_session_drawdown_hard_pct: f64,
    pub clip_session_drawdown_min_multiplier: f64,
    pub daily_loss_cap_pct: f64,
    pub loss_streak_cooldown_after: usize,
    pub loss_streak_cooldown_markets: usize,
    pub loss_streak_loss_threshold_usdc: f64,
    pub enforce_model_gate: bool,
    pub model_gate_min_confidence: f32,
    pub model_gate_max_risk: f32,
    pub model_gate_min_edge: f32,
    #[serde(rename = "model_spot_whipsaw_risk_weight")]
    pub model_btc_whipsaw_risk_weight: f32,
    #[serde(rename = "model_spot_path_inefficiency_risk_weight")]
    pub model_btc_path_inefficiency_risk_weight: f32,
    #[serde(rename = "model_spot_reversal_pressure_risk_weight")]
    pub model_btc_reversal_pressure_risk_weight: f32,
    pub enable_market_context_features: bool,
    pub volatility_regime_threshold: f64,
    pub walk_forward_folds: Option<usize>,
    pub fold_size: Option<usize>,
    pub purge_markets: usize,
    pub use_outcome_label: bool,
    pub maker_rebate_bps: f64,
    pub taker_fee_bps: f64,
    pub maker_rebates: Option<f64>,
    pub decision_log_every_n: usize,
    pub portfolio_checkpoint_every_markets: usize,
    pub decision_log_jsonl: Option<String>,
    pub checkpoint_markets_out: Option<String>,
    pub checkpoint_summary_out: Option<String>,
    pub meta_max_fit_samples: usize,
    pub meta_max_validation_samples: usize,
    pub meta_max_samples_per_market: usize,
    pub meta_max_oos_evaluation_samples: usize,
    pub meta_train_min_base_p: f32,
    pub meta_train_max_early_penalty: f32,
    pub meta_train_min_mid_distance: f32,
    pub min_train_markets: usize,
    pub meta_training_config: MetaTrainingConfig,
    pub meta_training_samples_cache: Option<String>,
    pub meta_calibrator_snapshot_in: Option<String>,
    pub meta_calibrator_snapshot_out: Option<String>,
    pub enable_meta_calibration: bool,
    pub forbid_meta_training: bool,
}


impl From<&WalkForwardConfig> for SharedRunConfig {
    fn from(cfg: &WalkForwardConfig) -> Self {
        Self {
            starting_cash_usdc: cfg.starting_cash_usdc,
            kelly_fraction: cfg.kelly_fraction,
            max_clip_usdc: cfg.max_clip_usdc,
            max_order_clip_multiplier: cfg.max_order_clip_multiplier,
            max_per_market_exposure_usdc: cfg.max_per_market_exposure_usdc,
            max_per_market_exposure_frac: cfg.max_per_market_exposure_frac,
            spot_symbol_mode: spot_symbol_mode(&cfg.spot_symbol).to_string(),
            spot_symbol_override: spot_symbol_override(&cfg.spot_symbol),
            portfolio_mode: cfg.portfolio_mode,
            max_concurrent_fetches: cfg.max_concurrent_fetches,
            replay_sample_ms: cfg.replay_sample_ms,
            taker_latency_ms: cfg.taker_latency_ms,
            replay_event_cache_dir: cfg
                .replay_event_cache_dir
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            load_pm_trades: cfg.load_pm_trades,
            clip_fraction_of_equity: cfg.clip_fraction_of_equity,
            clip_drawdown_soft_pct: cfg.clip_drawdown_soft_pct,
            clip_drawdown_hard_pct: cfg.clip_drawdown_hard_pct,
            clip_drawdown_min_multiplier: cfg.clip_drawdown_min_multiplier,
            clip_session_drawdown_soft_pct: cfg.clip_session_drawdown_soft_pct,
            clip_session_drawdown_hard_pct: cfg.clip_session_drawdown_hard_pct,
            clip_session_drawdown_min_multiplier: cfg.clip_session_drawdown_min_multiplier,
            daily_loss_cap_pct: cfg.daily_loss_cap_pct,
            loss_streak_cooldown_after: cfg.loss_streak_cooldown_after,
            loss_streak_cooldown_markets: cfg.loss_streak_cooldown_markets,
            loss_streak_loss_threshold_usdc: cfg.loss_streak_loss_threshold_usdc,
            enforce_model_gate: cfg.enforce_model_gate,
            model_gate_min_confidence: cfg.model_gate_min_confidence,
            model_gate_max_risk: cfg.model_gate_max_risk,
            model_gate_min_edge: cfg.model_gate_min_edge,
            model_btc_whipsaw_risk_weight: cfg.model_btc_whipsaw_risk_weight,
            model_btc_path_inefficiency_risk_weight: cfg.model_btc_path_inefficiency_risk_weight,
            model_btc_reversal_pressure_risk_weight: cfg.model_btc_reversal_pressure_risk_weight,
            enable_market_context_features: cfg.enable_market_context_features,
            volatility_regime_threshold: cfg.volatility_regime_threshold,
            walk_forward_folds: cfg.walk_forward_folds,
            fold_size: cfg.fold_size,
            purge_markets: cfg.purge_markets,
            meta_max_fit_samples: cfg.meta_max_fit_samples,
            meta_max_validation_samples: cfg.meta_max_validation_samples,
            meta_max_samples_per_market: cfg.meta_max_samples_per_market,
            meta_max_oos_evaluation_samples: cfg.meta_max_oos_evaluation_samples,
            meta_train_min_base_p: cfg.meta_train_min_base_p,
            meta_train_max_early_penalty: cfg.meta_train_max_early_penalty,
            meta_train_min_mid_distance: cfg.meta_train_min_mid_distance,
            min_train_markets: cfg.min_train_markets,
            meta_training_config: cfg.meta_training_config,
            meta_training_samples_cache: cfg
                .meta_training_samples_cache
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            meta_calibrator_snapshot_in: cfg
                .meta_calibrator_snapshot_in
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            meta_calibrator_snapshot_out: cfg
                .meta_calibrator_snapshot_out
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            enable_meta_calibration: cfg.enable_meta_calibration,
            forbid_meta_training: cfg.forbid_meta_training,
            decision_log_every_n: cfg.decision_log_every_n,
            portfolio_checkpoint_every_markets: cfg.portfolio_checkpoint_every_markets,
            decision_log_jsonl: cfg
                .decision_log_jsonl
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            checkpoint_markets_out: cfg
                .checkpoint_markets_out
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            checkpoint_summary_out: cfg
                .checkpoint_summary_out
                .as_ref()
                .map(|path| path.to_string_lossy().to_string()),
            use_outcome_label: cfg.use_outcome_label,
            maker_rebate_bps: cfg.maker_rebate_bps,
            taker_fee_bps: cfg.taker_fee_bps,
            maker_rebates: None,
        }
    }
}


pub fn spot_symbol_mode(configured: &str) -> &'static str {
    if configured.is_empty() {
        "disabled"
    } else if configured.eq_ignore_ascii_case("auto") {
        "auto"
    } else {
        "override"
    }
}


pub fn spot_symbol_override(configured: &str) -> Option<String> {
    if spot_symbol_mode(configured) == "override" {
        Some(configured.to_string())
    } else {
        None
    }
}


fn needs_perp_for_strategies(strategies: &[StratId]) -> bool {
    strategies
        .iter()
        .any(|s| matches!(s, StratId::ExoFade | StratId::MayJuneFade))
}


/// Resolve the perp symbol for walk-forward exo_fade parity with the alpha path.
pub fn resolve_perp_symbol(cfg: &WalkForwardConfig) -> Option<String> {
    if let Some(sym) = &cfg.perp_symbol {
        return Some(sym.clone());
    }
    if needs_perp_for_strategies(&cfg.strategies)
        && !cfg.spot_symbol.eq_ignore_ascii_case("auto")
        && !cfg.spot_symbol.is_empty()
    {
        return Some(cfg.spot_symbol.clone());
    }
    None
}


pub async fn load_walkforward_perp(
    store: &TelonexStore,
    cfg: &WalkForwardConfig,
    markets: &[MarketHandle],
) -> Result<Option<Arc<PerpState>>> {
    let Some(symbol) = resolve_perp_symbol(cfg) else {
        return Ok(None);
    };
    let mut dates: Vec<String> = markets.iter().map(|m| m.date.clone()).collect();
    dates.sort();
    dates.dedup();
    let cache_root = cfg
        .perp_cache_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("data/cache"));
    let perp = load_perp_state(store, &cache_root, &symbol, &dates)
        .await
        .with_context(|| format!("load perp state {symbol}"))?;
    tracing::info!(symbol, days = dates.len(), "walk-forward perp state loaded");
    Ok(Some(Arc::new(perp)))
}


pub fn market_volatility_range(events: &[pm_types::ReplayEvent]) -> f64 {
    if events.is_empty() {
        return 0.0;
    }
    let mut low = f64::INFINITY;
    let mut high = f64::NEG_INFINITY;
    for e in events {
        if !e.yes_mid.is_finite() {
            continue;
        }
        let v = e.yes_mid as f64;
        if v < low {
            low = v;
        }
        if v > high {
            high = v;
        }
    }
    if !low.is_finite() {
        return 0.0;
    }
    high - low
}


pub fn volatility_band(range: f64, threshold: f64) -> VolatilityBand {
    if range.is_nan() {
        return VolatilityBand::Low;
    }
    if range > threshold {
        VolatilityBand::High
    } else {
        VolatilityBand::Low
    }
}


pub fn compounded_clip(bankroll: f64, frac: f64) -> f64 {
    if !bankroll.is_finite() || !frac.is_finite() || bankroll <= 0.0 || frac <= 0.0 {
        return 0.0;
    }
    let raw = bankroll * frac;
    let cap = (bankroll * 0.10).max(0.0);
    if cap <= 0.50 {
        raw.min(cap).max(0.0)
    } else {
        raw.clamp(0.50, cap)
    }
}


pub fn per_market_exposure_cap(cfg: &WalkForwardConfig, bankroll: f64) -> f64 {
    match cfg.max_per_market_exposure_frac {
        Some(frac) if frac.is_finite() && frac >= 0.0 => {
            cfg.max_per_market_exposure_usdc.min(bankroll * frac)
        }
        _ => cfg.max_per_market_exposure_usdc,
    }
}


pub fn drawdown_clip_multiplier(
    drawdown_pct: f64,
    soft_pct: f64,
    hard_pct: f64,
    min_multiplier: f64,
) -> f64 {
    if !drawdown_pct.is_finite()
        || !soft_pct.is_finite()
        || !hard_pct.is_finite()
        || soft_pct >= hard_pct
    {
        return 1.0;
    }
    let floor = if min_multiplier.is_finite() {
        min_multiplier.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if drawdown_pct <= soft_pct {
        1.0
    } else if drawdown_pct >= hard_pct {
        floor
    } else {
        let progress = (drawdown_pct - soft_pct) / (hard_pct - soft_pct);
        1.0 - progress * (1.0 - floor)
    }
}


pub fn daily_remaining_loss_budget_usdc(
    daily_start_equity: f64,
    current_equity: f64,
    daily_loss_cap_pct: f64,
) -> Option<f64> {
    if daily_loss_cap_pct >= 1.0 || daily_start_equity <= 0.0 {
        return None;
    }
    let max_loss = daily_start_equity * daily_loss_cap_pct.max(0.0);
    let realized_loss = (daily_start_equity - current_equity).max(0.0);
    Some((max_loss - realized_loss).max(0.0))
}


#[derive(Debug, Clone, Default)]
pub struct LossStreakCooldownState {
    pub consecutive_losses: usize,
    cooldown_remaining_markets: usize,
}


impl LossStreakCooldownState {
    pub fn is_active(&self) -> bool {
        self.cooldown_remaining_markets > 0
    }

    pub fn consume_cooldown_market(&mut self) {
        self.cooldown_remaining_markets = self.cooldown_remaining_markets.saturating_sub(1);
    }

    pub fn record_completed_market(
        &mut self,
        traded: bool,
        pnl_usdc: f64,
        loss_threshold_usdc: f64,
        cooldown_after: usize,
        cooldown_markets: usize,
    ) {
        if cooldown_after == 0 || cooldown_markets == 0 || !traded {
            return;
        }
        if pnl_usdc <= loss_threshold_usdc {
            self.consecutive_losses += 1;
            if self.consecutive_losses >= cooldown_after {
                self.consecutive_losses = 0;
                self.cooldown_remaining_markets = cooldown_markets;
            }
        } else {
            self.consecutive_losses = 0;
        }
    }
}


/// Per-market spot-history cache so we don't re-download the same Binance day.
#[derive(Default)]
pub struct SpotCache {
    pub inner: HashMap<String, Arc<SpotHistory>>,
    raw_days: HashMap<String, Arc<Vec<SpotTick>>>,
}


impl SpotCache {
    pub async fn load_raw_day(
        &mut self,
        store: &TelonexStore,
        symbol: &str,
        date: &str,
        required: bool,
    ) -> Result<Option<Arc<Vec<SpotTick>>>> {
        let key = spot_cache_key(symbol, date);
        if let Some(ticks) = self.raw_days.get(&key) {
            return Ok(Some(ticks.clone()));
        }

        let path = match resolve_binance_day(store, "agg_trades", symbol, date).await {
            Ok(path) => path,
            Err(err) if required => {
                return Err(err).with_context(|| format!("resolve spot {symbol} {date}"));
            }
            Err(err) => {
                tracing::warn!(
                    symbol,
                    date,
                    error = %err,
                    "optional prior spot day unavailable"
                );
                return Ok(None);
            }
        };
        let (ticks, stats) = load_binance_agg_trades_async(store.store(), path).await?;
        tracing::info!(symbol, date, ticks = stats.rows_emitted, "spot day loaded");
        let ticks = Arc::new(ticks);
        self.raw_days.insert(key, ticks.clone());
        Ok(Some(ticks))
    }

    pub async fn get_or_load(
        &mut self,
        store: &TelonexStore,
        symbol: &str,
        date: &str,
    ) -> Result<Arc<SpotHistory>> {
        let key = spot_cache_key(symbol, date);
        if let Some(s) = self.inner.get(&key) {
            return Ok(s.clone());
        }
        let current = self
            .load_raw_day(store, symbol, date, true)
            .await?
            .ok_or_else(|| anyhow!("missing required spot day {symbol} {date}"))?;

        let mut ticks = Vec::new();
        if let Some(prev_date) = previous_date(date)? {
            if let Some(prev) = self
                .load_raw_day(store, symbol, &prev_date, false)
                .await
                .with_context(|| format!("load optional prior spot {prev_date}"))?
            {
                ticks.reserve(prev.len() + current.len());
                ticks.extend_from_slice(&prev);
            }
        }
        ticks.extend_from_slice(&current);
        let h = Arc::new(SpotHistory::new(ticks));
        self.inner.insert(key, h.clone());
        Ok(h)
    }
}


pub fn previous_date(date: &str) -> Result<Option<String>> {
    let parsed = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .with_context(|| format!("parse market date {date}"))?;
    Ok(parsed
        .checked_sub_signed(Duration::days(1))
        .map(|d| d.format("%Y-%m-%d").to_string()))
}


pub fn spot_history_for_market(
    spot_map: &HashMap<String, Arc<SpotHistory>>,
    empty_spot: &Arc<SpotHistory>,
    cfg: &WalkForwardConfig,
    market: &MarketHandle,
) -> Arc<SpotHistory> {
    let Some(symbol) = spot_symbol_for_market(&cfg.spot_symbol, &market.slug)
        .ok()
        .flatten()
    else {
        return empty_spot.clone();
    };
    spot_map
        .get(&spot_cache_key(&symbol, &market.date))
        .cloned()
        .unwrap_or_else(|| empty_spot.clone())
}


pub fn model_market_context_for_slug(slug: &str) -> ModelMarketContext {
    let slug = slug.to_ascii_lowercase();
    let asset = if slug.starts_with("btc-updown-") {
        MarketAsset::Btc
    } else if slug.starts_with("eth-updown-") {
        MarketAsset::Eth
    } else {
        MarketAsset::Unknown
    };
    let window_seconds = if slug.contains("-5m-") {
        300
    } else if slug.contains("-15m-") {
        900
    } else {
        0
    };
    ModelMarketContext {
        asset,
        window_seconds,
    }
}


pub fn model_market_context_for_cfg(
    cfg: &WalkForwardConfig,
    market: &MarketHandle,
) -> ModelMarketContext {
    if cfg.enable_market_context_features {
        model_market_context_for_slug(&market.slug)
    } else {
        ModelMarketContext::default()
    }
}



#[cfg(test)]
mod perp_tests {
    use super::*;

    #[test]
    fn metrics_day_parses_real_file_when_present() {
        let p = metrics_path(
            Path::new("data/cache"),
            "BTCUSDT",
            "2026-06-03",
        );
        if !p.exists() {
            return; // data-dependent test; skip when cache absent
        }
        let rows = read_metrics_day(&p).expect("parse metrics");
        assert!(rows.len() >= 280, "expect ~288 5-minute rows, got {}", rows.len());
        assert!(rows.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(rows[0].1 > 0.0);
    }

    #[test]
    fn funding_parses_real_files_when_present() {
        let rows = read_funding(Path::new("data/cache"), "BTCUSDT").expect("funding");
        if rows.is_empty() {
            return;
        }
        assert!(rows.windows(2).all(|w| w[0].0 <= w[1].0));
        assert!(rows.iter().all(|(_, r)| r.abs() < 0.01));
    }
}
