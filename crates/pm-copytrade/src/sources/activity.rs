use crate::model::{LeaderFill, Side};
use serde_json::{Value};

/// Parse one Data API /activity row into a LeaderFill. Returns None for rows
/// that are not TRADEs or are missing required fields.
pub fn parse_activity_row(v: &Value) -> Option<LeaderFill> {
    if v.get("type").and_then(Value::as_str)? != "TRADE" { return None; }
    let side = match v.get("side").and_then(Value::as_str)? {
        "BUY" => Side::Buy, "SELL" => Side::Sell, _ => return None,
    };
    Some(LeaderFill {
        ts: v.get("timestamp")?.as_f64()? as i64,
        token_id: v.get("asset")?.as_str()?.to_string(),
        condition_id: v.get("conditionId")?.as_str()?.to_string(),
        slug: v.get("slug").and_then(Value::as_str).unwrap_or("").to_string(),
        outcome: v.get("outcome").and_then(Value::as_str).unwrap_or("").to_string(),
        outcome_index: v.get("outcomeIndex").and_then(Value::as_u64).unwrap_or(0) as u8,
        side,
        price: v.get("price")?.as_f64()?,
        size: v.get("size")?.as_f64()?,
        usdc: v.get("usdcSize").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

/// Dedupe by (ts, token_id, price, size) and sort ascending by ts.
pub fn dedupe_sort(mut fills: Vec<LeaderFill>) -> Vec<LeaderFill> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    fills.retain(|f| seen.insert((f.ts, f.token_id.clone(), f.price.to_bits(), f.size.to_bits())));
    fills.sort_by_key(|f| f.ts);
    fills
}

use anyhow::{Context, Result};

#[async_trait::async_trait]
pub trait FillSource: Send + Sync {
    async fn fetch_fills(&self, wallet: &str, start_ts: i64, end_ts: i64) -> Result<Vec<LeaderFill>>;
}

pub struct HttpFillSource {
    pub client: reqwest::Client,
    pub base: String,
    pub bucket_seconds: i64,
}

const OFFSET_CEILING: i64 = 3000;
const PAGE_LIMIT: i64 = 500;

#[async_trait::async_trait]
impl FillSource for HttpFillSource {
    async fn fetch_fills(&self, wallet: &str, start_ts: i64, end_ts: i64) -> Result<Vec<LeaderFill>> {
        let mut out = Vec::new();
        let mut win_start = start_ts;
        while win_start < end_ts {
            let win_end = (win_start + self.bucket_seconds).min(end_ts);
            let mut offset = 0;
            loop {
                let url = format!(
                    "{}/activity?user={}&type=TRADE&limit={}&offset={}&start={}&end={}",
                    self.base, wallet, PAGE_LIMIT, offset, win_start, win_end
                );
                let resp = self.client.get(&url).send().await
                    .with_context(|| format!("activity GET {url}"))?;
                if resp.status() == reqwest::StatusCode::BAD_REQUEST && offset > 0 { break; }
                let rows: Vec<Value> = resp.error_for_status()?.json().await
                    .context("decode activity page")?;
                if rows.is_empty() { break; }
                let n = rows.len() as i64;
                out.extend(rows.iter().filter_map(parse_activity_row));
                if n < PAGE_LIMIT || offset + PAGE_LIMIT > OFFSET_CEILING { break; }
                offset += PAGE_LIMIT;
            }
            win_start = win_end;
        }
        Ok(dedupe_sort(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_a_buy_trade_row() {
        let row = json!({
            "type": "TRADE", "timestamp": 1780920147, "asset": "9219", "conditionId": "0x74f3",
            "slug": "btc-updown-5m-1780920000", "outcome": "Up", "outcomeIndex": 0,
            "side": "BUY", "price": 0.09, "size": 56, "usdcSize": 5.36104
        });
        let f = parse_activity_row(&row).unwrap();
        assert_eq!(f.side, Side::Buy);
        assert_eq!(f.token_id, "9219");
        assert_eq!(f.outcome_index, 0);
        assert!((f.usdc - 5.36104).abs() < 1e-6);
    }

    #[test]
    fn skips_non_trade_rows() {
        let row = json!({"type": "REDEEM", "timestamp": 1, "asset": "x", "conditionId": "c", "price": 1.0, "size": 1.0});
        assert!(parse_activity_row(&row).is_none());
    }

    #[test]
    fn dedupes_and_sorts() {
        let a = parse_activity_row(&json!({"type":"TRADE","timestamp":200,"asset":"t","conditionId":"c","side":"BUY","price":0.5,"size":2,"usdcSize":1})).unwrap();
        let b = parse_activity_row(&json!({"type":"TRADE","timestamp":100,"asset":"t","conditionId":"c","side":"BUY","price":0.5,"size":2,"usdcSize":1})).unwrap();
        let dup = a.clone();
        let out = dedupe_sort(vec![a, b, dup]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].ts, 100);
    }
}
