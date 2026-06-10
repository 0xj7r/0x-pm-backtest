//! Perp-complex loaders: Binance USD-M futures metrics (5-minute open
//! interest) and funding events from the local cache parquets written by
//! scripts/binance_perp_fetch.py, plus perp taker prints via the standard
//! binance trades loader (channel `futures_agg_trades`).

use anyhow::{Context, Result};
use chrono::NaiveDateTime;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::RowAccessor;
use pm_alpha::PerpState;
use pm_telonex_loader::{TelonexStore, load_binance_agg_trades_async, resolve_binance_day};
use std::fs::File;
use std::path::{Path, PathBuf};

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
        let ts = row.get_string(ct)?;
        let oi: f64 = row.get_string(co)?.parse().unwrap_or(f64::NAN);
        if !oi.is_finite() {
            continue;
        }
        let dt = NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
            .with_context(|| format!("parse create_time {ts}"))?;
        out.push((dt.and_utc().timestamp_nanos_opt().unwrap_or(0), oi));
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

#[cfg(test)]
mod tests {
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
