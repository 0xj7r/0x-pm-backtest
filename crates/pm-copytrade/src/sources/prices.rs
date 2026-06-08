use crate::model::PriceTag;
use anyhow::Result;

#[derive(Clone, Copy, Debug)]
pub struct PricePoint { pub ts: i64, pub price: f64 }

/// Among points with `target_ts <= p.ts <= target_ts + max_wait_s`, return
/// the one with the smallest ts (earliest forward print). None if none qualify.
pub fn first_forward_price(points: &[PricePoint], target_ts: i64, max_wait_s: i64) -> Option<PricePoint> {
    points.iter().copied()
        .filter(|p| p.ts >= target_ts && p.ts <= target_ts + max_wait_s)
        .min_by_key(|p| p.ts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_returns_earliest_qualifying() {
        let pts = vec![
            PricePoint { ts: 98,  price: 0.30 },
            PricePoint { ts: 104, price: 0.55 },
            PricePoint { ts: 140, price: 0.80 },
        ];
        // target=100, max_wait=60 -> window [100,160]; qualifying: ts=104,140; earliest is 104
        let p = first_forward_price(&pts, 100, 60).unwrap();
        assert_eq!(p.ts, 104);
        assert!((p.price - 0.55).abs() < 1e-9);
    }

    #[test]
    fn forward_excludes_past_prints() {
        let pts = vec![PricePoint { ts: 90, price: 0.50 }];
        // target=100 -> ts=90 is before target, must be excluded
        assert!(first_forward_price(&pts, 100, 60).is_none());
    }

    #[test]
    fn forward_excludes_prints_beyond_max_wait() {
        let pts = vec![PricePoint { ts: 200, price: 0.50 }];
        // target=100, max_wait=60 -> window [100,160]; ts=200 is outside
        assert!(first_forward_price(&pts, 100, 60).is_none());
    }

    #[test]
    fn forward_empty_returns_none() {
        assert!(first_forward_price(&[], 100, 60).is_none());
    }
}

use serde_json::Value;
use anyhow::Context;

#[async_trait::async_trait]
pub trait PriceSource: Send + Sync {
    /// Price of `token_id` at or after `target_ts` within `window_s` seconds, with provenance tag.
    async fn price_at(&self, token_id: &str, condition_id: &str, target_ts: i64)
        -> Result<Option<(f64, PriceTag)>>;
}

pub struct HttpPriceSource {
    pub client: reqwest::Client,
    pub data_base: String,
    pub clob_base: String,
    /// Max seconds to wait for a forward trade print at or after the copy timestamp.
    pub window_s: i64,
    cache: tokio::sync::Mutex<std::collections::HashMap<String, Vec<PricePoint>>>,
}

impl HttpPriceSource {
    pub fn new(client: reqwest::Client, data_base: String, clob_base: String, window_s: i64) -> Self {
        Self { client, data_base, clob_base, window_s, cache: tokio::sync::Mutex::new(std::collections::HashMap::new()) }
    }

    async fn market_prints(&self, token_id: &str, condition_id: &str) -> Result<Vec<PricePoint>> {
        {
            let guard = self.cache.lock().await;
            if let Some(pts) = guard.get(condition_id) {
                return Ok(pts.clone());
            }
        }
        // /trades?market=<conditionId> returns all traders' prints for the market;
        // keep only the leader's outcome token, paginate up to the offset ceiling.
        let mut pts = Vec::new();
        let mut offset = 0;
        loop {
            let url = format!("{}/trades?market={}&limit=500&offset={}", self.data_base, condition_id, offset);
            let rows: Vec<Value> = self.client.get(&url).send().await
                .with_context(|| format!("trades GET {url}"))?
                .error_for_status()?.json().await.context("decode trades")?;
            if rows.is_empty() { break; }
            let n = rows.len();
            for r in &rows {
                if r.get("asset").and_then(Value::as_str) == Some(token_id) {
                    if let (Some(ts), Some(p)) = (
                        r.get("timestamp").and_then(Value::as_f64),
                        r.get("price").and_then(Value::as_f64),
                    ) { pts.push(PricePoint { ts: ts as i64, price: p }); }
                }
            }
            if n < 500 || offset + 500 > 3000 { break; }
            offset += 500;
        }
        self.cache.lock().await.insert(condition_id.to_string(), pts.clone());
        Ok(pts)
    }
}

#[async_trait::async_trait]
impl PriceSource for HttpPriceSource {
    async fn price_at(&self, token_id: &str, condition_id: &str, target_ts: i64)
        -> Result<Option<(f64, PriceTag)>> {
        let prints = self.market_prints(token_id, condition_id).await?;
        Ok(first_forward_price(&prints, target_ts, self.window_s)
            .map(|p| (p.price, PriceTag::MarketPrint)))
    }
}
