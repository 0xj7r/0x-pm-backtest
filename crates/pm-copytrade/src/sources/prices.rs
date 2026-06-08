use crate::model::PriceTag;
use anyhow::Result;

#[derive(Clone, Copy, Debug)]
pub struct PricePoint { pub ts: i64, pub price: f64 }

/// Pick the print closest in time to `target_ts`. Returns None on empty input.
pub fn nearest_price(points: &[PricePoint], target_ts: i64) -> Option<PricePoint> {
    points.iter().copied().min_by_key(|p| (p.ts - target_ts).abs())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picks_closest_in_time() {
        let pts = vec![
            PricePoint { ts: 100, price: 0.50 },
            PricePoint { ts: 105, price: 0.60 },
            PricePoint { ts: 130, price: 0.90 },
        ];
        // target 104 -> closest is ts=105
        assert!((nearest_price(&pts, 104).unwrap().price - 0.60).abs() < 1e-9);
    }
    #[test]
    fn empty_returns_none() {
        assert!(nearest_price(&[], 10).is_none());
    }
}

use serde_json::Value;
use anyhow::Context;

#[async_trait::async_trait]
pub trait PriceSource: Send + Sync {
    /// Price of `token_id` nearest to `target_ts`, with provenance tag.
    async fn price_at(&self, token_id: &str, condition_id: &str, target_ts: i64)
        -> Result<Option<(f64, PriceTag)>>;
}

pub struct HttpPriceSource {
    pub client: reqwest::Client,
    pub data_base: String,
    pub clob_base: String,
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

    async fn prices_history(&self, token_id: &str, target_ts: i64) -> Result<Vec<PricePoint>> {
        let url = format!("{}/prices-history?market={}&startTs={}&endTs={}&fidelity=1",
            self.clob_base, token_id, target_ts - self.window_s, target_ts + self.window_s);
        let body: Value = self.client.get(&url).send().await
            .with_context(|| format!("prices-history GET {url}"))?
            .error_for_status()?.json().await.context("decode prices-history")?;
        let hist = body.get("history").and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(hist.iter().filter_map(|h| Some(PricePoint {
            ts: h.get("t")?.as_f64()? as i64,
            price: h.get("p")?.as_f64()?,
        })).collect())
    }
}

#[async_trait::async_trait]
impl PriceSource for HttpPriceSource {
    async fn price_at(&self, token_id: &str, condition_id: &str, target_ts: i64)
        -> Result<Option<(f64, PriceTag)>> {
        let prints = self.market_prints(token_id, condition_id).await.unwrap_or_default();
        let in_window: Vec<_> = prints.iter().copied()
            .filter(|p| (p.ts - target_ts).abs() <= self.window_s).collect();
        if let Some(p) = nearest_price(&in_window, target_ts) {
            return Ok(Some((p.price, PriceTag::MarketPrint)));
        }
        let hist = self.prices_history(token_id, target_ts).await.unwrap_or_default();
        if let Some(p) = nearest_price(&hist, target_ts) {
            return Ok(Some((p.price, PriceTag::PricesHistory)));
        }
        Ok(None) // caller falls back to leader fill price, tagged LeaderFill
    }
}
