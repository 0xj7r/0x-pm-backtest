use crate::model::Resolution;
use serde_json::Value;
use anyhow::Result;

/// Parse a Gamma /markets element. `outcomePrices` is a JSON-encoded string
/// like "[\"1\", \"0\"]"; the winning index is the one priced at ~1.0.
pub fn parse_gamma_market(v: &Value) -> Option<Resolution> {
    let condition_id = v.get("conditionId")?.as_str()?.to_string();
    let closed = v.get("closed").and_then(Value::as_bool).unwrap_or(false);
    let end_ts = v.get("endDate").and_then(Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.timestamp());
    let prices: Vec<f64> = v.get("outcomePrices").and_then(Value::as_str)
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .map(|v| v.iter().filter_map(|x| x.parse::<f64>().ok()).collect())
        .unwrap_or_default();
    let winning_index = if closed {
        prices.iter().position(|&p| p > 0.5).map(|i| i as u8)
    } else { None };
    Some(Resolution { condition_id, resolved: closed && winning_index.is_some(), winning_index, end_ts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn resolved_up_winner() {
        let m = json!({"conditionId":"0xabc","closed":true,
            "outcomes":"[\"Up\", \"Down\"]","outcomePrices":"[\"1\", \"0\"]",
            "endDate":"2026-06-08T12:05:00Z"});
        let r = parse_gamma_market(&m).unwrap();
        assert!(r.resolved);
        assert_eq!(r.winning_index, Some(0));
        assert_eq!(r.end_ts, Some(1780920300));
    }
    #[test]
    fn open_market_unresolved() {
        let m = json!({"conditionId":"0xabc","closed":false,
            "outcomes":"[\"Up\", \"Down\"]","outcomePrices":"[\"0.855\", \"0.145\"]"});
        let r = parse_gamma_market(&m).unwrap();
        assert!(!r.resolved);
        assert_eq!(r.winning_index, None);
    }
}

use std::collections::HashMap;
use tokio::sync::Mutex;
use anyhow::Context;

#[async_trait::async_trait]
pub trait ResolutionSource: Send + Sync {
    async fn resolution(&self, condition_id: &str) -> Result<Option<Resolution>>;
}

pub struct HttpResolutionSource {
    pub client: reqwest::Client,
    pub gamma_base: String,  // https://gamma-api.polymarket.com
    cache: Mutex<HashMap<String, Resolution>>,
}

impl HttpResolutionSource {
    pub fn new(client: reqwest::Client, gamma_base: String) -> Self {
        Self { client, gamma_base, cache: Mutex::new(HashMap::new()) }
    }
}

#[async_trait::async_trait]
impl ResolutionSource for HttpResolutionSource {
    async fn resolution(&self, condition_id: &str) -> Result<Option<Resolution>> {
        if let Some(r) = self.cache.lock().await.get(condition_id) {
            if r.resolved { return Ok(Some(r.clone())); }
        }
        let url = format!("{}/markets?condition_ids={}", self.gamma_base, condition_id);
        let rows: Vec<Value> = self.client.get(&url).send().await
            .with_context(|| format!("gamma GET {url}"))?
            .error_for_status()?.json().await.context("decode gamma markets")?;
        let res = rows.first().and_then(parse_gamma_market);
        if let Some(r) = &res {
            if r.resolved { self.cache.lock().await.insert(condition_id.to_string(), r.clone()); }
        }
        Ok(res)
    }
}
