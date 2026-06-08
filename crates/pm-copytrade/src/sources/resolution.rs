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
use anyhow::{anyhow, Context};

#[async_trait::async_trait]
pub trait ResolutionSource: Send + Sync {
    async fn resolution(&self, condition_id: &str) -> Result<Option<Resolution>>;

    /// Fetch resolutions for a batch of condition_ids. Returns a map of only the
    /// resolved entries. Default impl loops over `resolution()` for compatibility.
    async fn resolutions_batch(&self, ids: &[String]) -> Result<HashMap<String, Resolution>> {
        let mut out = HashMap::new();
        for id in ids {
            if let Some(r) = self.resolution(id).await? {
                out.insert(r.condition_id.clone(), r);
            }
        }
        Ok(out)
    }
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

    async fn fetch_closed_batch(&self, ids: &[String]) -> Result<Vec<Value>> {
        // Build URL with repeated condition_ids query params and explicit limit.
        let id_params: String = ids.iter()
            .map(|id| format!("&condition_ids={}", id))
            .collect();
        let url = format!(
            "{}/markets?limit={}&closed=true{}",
            self.gamma_base, ids.len(), id_params
        );
        let mut backoff = std::time::Duration::from_secs(1);
        for attempt in 0..5u32 {
            let resp = self.client.get(&url).send().await
                .with_context(|| format!("gamma batch GET {url}"))?;
            if resp.status().as_u16() == 429 {
                if attempt == 4 {
                    return Err(anyhow!("gamma 429 (max retries) for batch of {}", ids.len()));
                }
                let retry_after = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(std::time::Duration::from_secs);
                tokio::time::sleep(retry_after.unwrap_or(backoff)).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                continue;
            }
            resp.error_for_status_ref()
                .with_context(|| format!("gamma batch non-2xx for {} ids", ids.len()))?;
            return resp.json::<Vec<Value>>().await.context("decode gamma batch");
        }
        Err(anyhow!("gamma batch exhausted retries"))
    }
}

#[async_trait::async_trait]
impl ResolutionSource for HttpResolutionSource {
    async fn resolution(&self, condition_id: &str) -> Result<Option<Resolution>> {
        if let Some(r) = self.cache.lock().await.get(condition_id) {
            if r.resolved { return Ok(Some(r.clone())); }
        }
        // Try open markets first (condition_ids plural works for active markets).
        let open_url = format!("{}/markets?condition_ids={}", self.gamma_base, condition_id);
        let rows: Vec<Value> = self.client.get(&open_url).send().await
            .with_context(|| format!("gamma GET {open_url}"))?
            .error_for_status()?.json().await.context("decode gamma markets")?;
        let res = rows.into_iter().find(|r| r.get("conditionId").and_then(Value::as_str) == Some(condition_id))
            .and_then(|r| parse_gamma_market(&r));
        if let Some(ref r) = res {
            if r.resolved {
                self.cache.lock().await.insert(condition_id.to_string(), r.clone());
                return Ok(Some(r.clone()));
            }
        }
        // Fall back to closed market query.
        let closed_url = format!("{}/markets?closed=true&condition_ids={}", self.gamma_base, condition_id);
        let rows2: Vec<Value> = self.client.get(&closed_url).send().await
            .with_context(|| format!("gamma GET {closed_url}"))?
            .error_for_status()?.json().await.context("decode gamma closed markets")?;
        let res2 = rows2.into_iter().find(|r| r.get("conditionId").and_then(Value::as_str) == Some(condition_id))
            .and_then(|r| parse_gamma_market(&r));
        if let Some(ref r) = res2 {
            if r.resolved { self.cache.lock().await.insert(condition_id.to_string(), r.clone()); }
        }
        Ok(res2.or(res))
    }

    async fn resolutions_batch(&self, ids: &[String]) -> Result<HashMap<String, Resolution>> {
        // Check cache first; only fetch those not already resolved.
        let mut out: HashMap<String, Resolution> = HashMap::new();
        let mut to_fetch: Vec<String> = Vec::new();
        {
            let guard = self.cache.lock().await;
            for id in ids {
                if let Some(r) = guard.get(id) {
                    if r.resolved { out.insert(id.clone(), r.clone()); continue; }
                }
                to_fetch.push(id.clone());
            }
        }
        if to_fetch.is_empty() { return Ok(out); }

        let rows = self.fetch_closed_batch(&to_fetch).await?;
        let mut guard = self.cache.lock().await;
        for v in rows {
            if let Some(r) = parse_gamma_market(&v) {
                if r.resolved {
                    out.insert(r.condition_id.clone(), r.clone());
                    guard.insert(r.condition_id.clone(), r);
                }
            }
        }
        Ok(out)
    }
}
