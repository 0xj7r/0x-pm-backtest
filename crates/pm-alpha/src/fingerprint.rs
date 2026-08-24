//! Config fingerprinting: a stable, canonical hash of a fully-resolved run
//! config so every backtest summary and shadow banner line can be tied back to
//! the exact configuration that produced it.
//!
//! The fingerprint is the 16-hex-char prefix of the sha256 over the canonical
//! (recursively sorted-keys) JSON serialization of the config. Sorting makes it
//! independent of field declaration or construction order, so two configs with
//! identical values fingerprint identically regardless of how they were built.

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Recursively sort every JSON map's keys so the serialization is independent
/// of construction order. `serde_json::Map` is backed by a BTreeMap (sorted)
/// unless the `preserve_order` feature is on; sort explicitly to be safe.
fn sort_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            // serde_json::Map does not expose in-place key sorting, so rebuild
            // it from a BTreeMap-sorted entry list.
            let mut entries: Vec<(String, Value)> =
                map.iter_mut().map(|(k, v)| (k.clone(), v.take())).collect();
            for (_, v) in &mut entries {
                sort_value(v);
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            map.clear();
            for (k, v) in entries {
                map.insert(k, v);
            }
        }
        Value::Array(items) => {
            for item in items {
                sort_value(item);
            }
        }
        _ => {}
    }
}

/// 16-hex prefix of the sha256 over canonical (sorted-keys) JSON of a resolved
/// config. Stable across construction paths; changes when any field value
/// changes.
pub fn config_fingerprint<T: Serialize>(cfg: &T) -> String {
    let mut value = serde_json::to_value(cfg).unwrap_or(Value::Null);
    sort_value(&mut value);
    let canon = serde_json::to_string(&value).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canon.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|b| format!("{:02x}", b)).collect::<String>()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct SampleCfg {
        spot_symbol: String,
        starting_cash_usdc: f64,
        strategies: Vec<String>,
        nested: NestedCfg,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct NestedCfg {
        threshold: f64,
        enabled: bool,
    }

    fn sample() -> SampleCfg {
        SampleCfg {
            spot_symbol: "BTCUSDT".to_string(),
            starting_cash_usdc: 1000.0,
            strategies: vec!["noop".to_string()],
            nested: NestedCfg {
                threshold: 0.12,
                enabled: true,
            },
        }
    }

    #[test]
    fn stable_across_identical_instances() {
        let a = config_fingerprint(&sample());
        let b = config_fingerprint(&sample());
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sensitive_to_field_changes() {
        let base = config_fingerprint(&sample());

        let mut changed_spot = sample();
        changed_spot.spot_symbol = "ETHUSDT".to_string();
        assert_ne!(config_fingerprint(&changed_spot), base);

        let mut changed_threshold = sample();
        changed_threshold.nested.threshold = 0.20;
        assert_ne!(config_fingerprint(&changed_threshold), base);

        let mut changed_cash = sample();
        changed_cash.starting_cash_usdc = 2000.0;
        assert_ne!(config_fingerprint(&changed_cash), base);
    }

    #[test]
    fn canonical_order_does_not_matter() {
        // Same config values, different key order in the JSON source.
        let ordered = r#"{
            "spot_symbol": "BTCUSDT",
            "starting_cash_usdc": 1000.0,
            "strategies": ["noop"],
            "nested": { "threshold": 0.12, "enabled": true }
        }"#;
        let reversed = r#"{
            "nested": { "enabled": true, "threshold": 0.12 },
            "strategies": ["noop"],
            "starting_cash_usdc": 1000.0,
            "spot_symbol": "BTCUSDT"
        }"#;

        let a: SampleCfg = serde_json::from_str(ordered).unwrap();
        let b: SampleCfg = serde_json::from_str(reversed).unwrap();
        assert_eq!(a, b);
        assert_eq!(config_fingerprint(&a), config_fingerprint(&b));
    }
}
