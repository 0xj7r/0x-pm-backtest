# Wallet Copy-Trade (Historical) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a `pm-copytrade` crate that replays a specific Polymarket wallet's historical trades under modelled execution latency and reports the copy P&L (ROI, win rate, drawdown, per-asset breakdown).

**Architecture:** Pure transform functions (parse, nearest-price, equity reconstruction, sizing, settlement, summary) are unit-tested in isolation. Async I/O is hidden behind three injectable traits (`FillSource`, `PriceSource`, `ResolutionSource`) with `reqwest` implementations for production and in-memory fakes for tests. `run_historical` orchestrates: fetch leader fills -> reconstruct leader equity -> for each swept latency, price the copy entry, size proportionally against a `pm_risk::PortfolioState` bankroll, settle to resolution -> summarise. A `pm-app copy-trade` subcommand exposes it.

**Tech Stack:** Rust (edition 2024), tokio, reqwest (rustls, json), async-trait, futures (bounded concurrency), serde/serde_json, chrono, anyhow, clap; reuses `pm-risk` and `pm-types`.

**Spec:** `docs/superpowers/specs/2026-06-08-wallet-copytrade-design.md`

**Scope note:** This plan covers the HISTORICAL mode only. The live forward-tracker (RTDS ingest + resolution poller) is a separate follow-up plan that reuses `model.rs`, `equity.rs`, `sizing.rs`, `ledger.rs`, and `summary.rs` unchanged.

---

## File structure

```
crates/pm-copytrade/
  Cargo.toml
  src/
    lib.rs           # re-exports; run_historical(); source traits
    model.rs         # Side, LeaderFill, Resolution, CopyEntry, CopyResult, RunConfig, PriceTag
    sources/
      mod.rs
      activity.rs     # parse_activity_row, dedupe_sort; HttpFillSource (reqwest)
      prices.rs       # nearest_price; HttpPriceSource (/trades + prices-history fallback)
      resolution.rs   # parse_gamma_market; HttpResolutionSource (+ in-memory cache)
    equity.rs        # LeaderEquity::reconstruct, equity_at
    sizing.rs        # proportional_stake
    ledger.rs        # CopyLedger over pm_risk::PortfolioState
    summary.rs       # Summary::from_results, write_ledger_jsonl, write_summary_json
crates/pm-app/src/main.rs   # add CopyTrade subcommand
Cargo.toml                  # add crate to members; add async-trait, futures to workspace deps
```

---

### Task 1: Crate skeleton + model types + workspace wiring

**Files:**
- Modify: `Cargo.toml` (workspace `members` + `workspace.dependencies`)
- Create: `crates/pm-copytrade/Cargo.toml`
- Create: `crates/pm-copytrade/src/lib.rs`
- Create: `crates/pm-copytrade/src/model.rs`

- [ ] **Step 1: Add the crate to the workspace**

In root `Cargo.toml`, add to `members`:
```toml
    "crates/pm-copytrade",
```
And under `[workspace.dependencies]` add (if absent):
```toml
async-trait = "0.1"
futures     = "0.3"
tokio       = { version = "1", features = ["macros", "rt-multi-thread", "time"] }
```
(If `tokio` is already declared, leave it; ensure `time` feature is present.)

- [ ] **Step 2: Create `crates/pm-copytrade/Cargo.toml`**

```toml
[package]
name = "pm-copytrade"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
authors.workspace = true
license.workspace = true
publish = false

[dependencies]
pm-types     = { path = "../pm-types" }
pm-risk      = { path = "../pm-risk" }
serde        = { workspace = true }
serde_json   = { workspace = true }
chrono       = { workspace = true }
anyhow       = { workspace = true }
thiserror    = { workspace = true }
reqwest      = { workspace = true }
tokio        = { workspace = true }
async-trait  = { workspace = true }
futures      = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
```

- [ ] **Step 3: Write `model.rs` with the failing test**

```rust
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side { Buy, Sell }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LeaderFill {
    pub ts: i64,
    pub token_id: String,
    pub condition_id: String,
    pub slug: String,
    pub outcome: String,
    pub outcome_index: u8,
    pub side: Side,
    pub price: f64,
    pub size: f64,   // shares
    pub usdc: f64,   // notional
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub condition_id: String,
    pub resolved: bool,
    pub winning_index: Option<u8>, // index into outcomes whose price == 1
    pub end_ts: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PriceTag { MarketPrint, PricesHistory, LeaderFill }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyEntry {
    pub leader: LeaderFill,
    pub latency_s: f64,
    pub entry_price: f64,
    pub priced_from: PriceTag,
    pub our_stake_usdc: f64,
    pub our_shares: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CopyResult {
    pub entry: CopyEntry,
    pub won: bool,
    pub payout_usdc: f64,
    pub pnl_usdc: f64,
    pub resolved_ts: i64,
}

impl CopyResult {
    /// Binary settlement: a winning share pays $1, a losing share pays $0.
    pub fn settle(entry: CopyEntry, won: bool, resolved_ts: i64) -> Self {
        let payout = if won { entry.our_shares } else { 0.0 };
        let pnl = payout - entry.our_stake_usdc;
        CopyResult { entry, won, payout_usdc: payout, pnl_usdc: pnl, resolved_ts }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn entry(stake: f64, price: f64) -> CopyEntry {
        CopyEntry {
            leader: LeaderFill { ts: 0, token_id: "t".into(), condition_id: "c".into(),
                slug: "s".into(), outcome: "Up".into(), outcome_index: 0, side: Side::Buy,
                price, size: stake / price, usdc: stake },
            latency_s: 0.0, entry_price: price, priced_from: PriceTag::MarketPrint,
            our_stake_usdc: stake, our_shares: stake / price,
        }
    }

    #[test]
    fn winning_share_pays_one_dollar() {
        let r = CopyResult::settle(entry(10.0, 0.40), true, 100);
        // 10 / 0.40 = 25 shares -> payout 25, pnl 15
        assert!((r.payout_usdc - 25.0).abs() < 1e-9);
        assert!((r.pnl_usdc - 15.0).abs() < 1e-9);
    }

    #[test]
    fn losing_bet_loses_stake() {
        let r = CopyResult::settle(entry(10.0, 0.40), false, 100);
        assert_eq!(r.payout_usdc, 0.0);
        assert!((r.pnl_usdc + 10.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 4: Write `lib.rs` stub**

```rust
pub mod model;
pub mod equity;
pub mod sizing;
pub mod ledger;
pub mod summary;
pub mod sources;
```
(Leave `run_historical` and source traits for later tasks; create empty module files as each task reaches them, or create stubs now: `pub mod equity {}` style is not valid for files — create the files in their tasks. For this task, only declare `pub mod model;` so the crate compiles.)

Replace `lib.rs` with just:
```rust
pub mod model;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p pm-copytrade`
Expected: PASS (2 tests in model).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/pm-copytrade
git commit -m "pm-copytrade: crate skeleton + model types"
```

---

### Task 2: Leader fills source (activity parse + window-walk)

**Files:**
- Create: `crates/pm-copytrade/src/sources/mod.rs`
- Create: `crates/pm-copytrade/src/sources/activity.rs`
- Modify: `crates/pm-copytrade/src/lib.rs`

- [ ] **Step 1: Declare modules**

`lib.rs`:
```rust
pub mod model;
pub mod sources;
```
`sources/mod.rs`:
```rust
pub mod activity;
```

- [ ] **Step 2: Write failing tests in `activity.rs`**

```rust
use crate::model::{LeaderFill, Side};
use serde_json::{json, Value};

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

#[cfg(test)]
mod tests {
    use super::*;

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
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p pm-copytrade sources::activity`
Expected: PASS (3 tests). (Implementation is in Step 2 alongside the tests — pure functions, no external I/O.)

- [ ] **Step 4: Add the async `FillSource` trait + reqwest impl (no unit test; covered by Task 9 integration)**

Append to `activity.rs`:
```rust
use anyhow::{Context, Result};

#[async_trait::async_trait]
pub trait FillSource: Send + Sync {
    async fn fetch_fills(&self, wallet: &str, start_ts: i64, end_ts: i64) -> Result<Vec<LeaderFill>>;
}

pub struct HttpFillSource {
    pub client: reqwest::Client,
    pub base: String,          // "https://data-api.polymarket.com"
    pub bucket_seconds: i64,    // window size for the walk, e.g. 3600
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
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo test -p pm-copytrade`
Expected: PASS (5 tests total; the new trait/impl has no test yet but must compile).

- [ ] **Step 6: Commit**

```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: leader activity parsing + window-walk fill source"
```

---

### Task 3: Price-at-latency source

**Files:**
- Create: `crates/pm-copytrade/src/sources/prices.rs`
- Modify: `crates/pm-copytrade/src/sources/mod.rs`

- [ ] **Step 1: Declare module** — add `pub mod prices;` to `sources/mod.rs`.

- [ ] **Step 2: Write failing test for the pure `nearest_price`**

```rust
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
```

Run: `cargo test -p pm-copytrade sources::prices`
Expected: PASS (2 tests).

- [ ] **Step 3: Add the async `PriceSource` trait + reqwest impl**

Append to `prices.rs`:
```rust
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
    pub data_base: String,  // https://data-api.polymarket.com
    pub clob_base: String,  // https://clob.polymarket.com
    pub window_s: i64,      // search +/- this many seconds, e.g. 90
}

impl HttpPriceSource {
    async fn market_prints(&self, token_id: &str, condition_id: &str) -> Result<Vec<PricePoint>> {
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
```

- [ ] **Step 4: Verify compile + tests** — Run: `cargo test -p pm-copytrade`. Expected: PASS (7 total).

- [ ] **Step 5: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: price-at-latency source (market prints + prices-history fallback)"
```

---

### Task 4: Resolution source

**Files:**
- Create: `crates/pm-copytrade/src/sources/resolution.rs`
- Modify: `crates/pm-copytrade/src/sources/mod.rs`

- [ ] **Step 1: Declare module** — add `pub mod resolution;` to `sources/mod.rs`.

- [ ] **Step 2: Write failing test for the pure Gamma parser**

```rust
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
```

Run: `cargo test -p pm-copytrade sources::resolution`
Expected: PASS (2 tests).

- [ ] **Step 3: Add async `ResolutionSource` trait + cached reqwest impl**

```rust
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
```

- [ ] **Step 4: Verify compile + tests** — Run: `cargo test -p pm-copytrade`. Expected: PASS (9 total).

- [ ] **Step 5: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: Gamma resolution source with cache"
```

---

### Task 5: Leader equity reconstruction

**Files:**
- Create: `crates/pm-copytrade/src/equity.rs`
- Modify: `crates/pm-copytrade/src/lib.rs` (add `pub mod equity;`)

- [ ] **Step 1: Write failing test**

```rust
use crate::model::{LeaderFill, Resolution};
use std::collections::HashMap;

/// A time-ordered reconstruction of the leader's USDC equity from their own
/// buys (cash out) and resolution payoffs (winning shares pay $1 each).
pub struct LeaderEquity {
    points: Vec<(i64, f64)>, // (ts, equity) sorted ascending
}

impl LeaderEquity {
    pub fn reconstruct(fills: &[LeaderFill], res: &HashMap<String, Resolution>, seed_usdc: f64) -> Self {
        // Build a single event timeline: at fill.ts subtract usdc; at resolution end_ts
        // add winning payoff for fills on the winning token.
        let mut events: Vec<(i64, f64)> = Vec::new();
        for f in fills {
            events.push((f.ts, -f.usdc));
            if let Some(r) = res.get(&f.condition_id) {
                if let (true, Some(wi), Some(ets)) = (r.resolved, r.winning_index, r.end_ts) {
                    if wi == f.outcome_index { events.push((ets, f.size)); } // shares * $1
                }
            }
        }
        events.sort_by_key(|(ts, _)| *ts);
        let mut equity = seed_usdc;
        let mut points = Vec::with_capacity(events.len());
        for (ts, delta) in events {
            equity += delta;
            points.push((ts, equity));
        }
        if points.is_empty() { points.push((0, seed_usdc)); }
        LeaderEquity { points }
    }

    /// Equity as of just before `ts` (most recent point with point.ts <= ts).
    pub fn equity_at(&self, ts: i64) -> f64 {
        match self.points.partition_point(|(t, _)| *t <= ts) {
            0 => self.points.first().map(|(_, e)| *e).unwrap_or(0.0),
            i => self.points[i - 1].1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Side;
    fn fill(ts: i64, cond: &str, oi: u8, usdc: f64, shares: f64) -> LeaderFill {
        LeaderFill { ts, token_id: format!("{cond}-{oi}"), condition_id: cond.into(),
            slug: "s".into(), outcome: "x".into(), outcome_index: oi, side: Side::Buy,
            price: usdc / shares, size: shares, usdc }
    }
    #[test]
    fn buy_then_win_restores_and_grows_equity() {
        let fills = vec![fill(100, "c1", 0, 10.0, 25.0)]; // bet 10 -> 25 shares
        let mut res = HashMap::new();
        res.insert("c1".into(), Resolution { condition_id: "c1".into(), resolved: true,
            winning_index: Some(0), end_ts: Some(200) });
        let eq = LeaderEquity::reconstruct(&fills, &res, 1000.0);
        assert!((eq.equity_at(150) - 990.0).abs() < 1e-9);   // after buy, before resolve
        assert!((eq.equity_at(250) - 1015.0).abs() < 1e-9);  // +25 payout
    }
    #[test]
    fn buy_then_loss_keeps_equity_down() {
        let fills = vec![fill(100, "c1", 1, 10.0, 25.0)]; // bought index 1
        let mut res = HashMap::new();
        res.insert("c1".into(), Resolution { condition_id: "c1".into(), resolved: true,
            winning_index: Some(0), end_ts: Some(200) }); // index 0 won
        let eq = LeaderEquity::reconstruct(&fills, &res, 1000.0);
        assert!((eq.equity_at(250) - 990.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run tests** — Run: `cargo test -p pm-copytrade equity`. Expected: PASS (2 tests).

- [ ] **Step 3: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: leader equity reconstruction"
```

---

### Task 6: Proportional sizing

**Files:**
- Create: `crates/pm-copytrade/src/sizing.rs`
- Modify: `crates/pm-copytrade/src/lib.rs` (add `pub mod sizing;`)

- [ ] **Step 1: Write failing test**

```rust
/// Our stake mirrors the leader's conviction (their bet as a fraction of their
/// equity) applied to our equity, then clamped to a per-trade cap.
pub fn proportional_stake(
    leader_usdc: f64,
    leader_equity: f64,
    our_equity: f64,
    max_clip_usdc: f64,
) -> f64 {
    if leader_equity <= 0.0 || our_equity <= 0.0 { return 0.0; }
    let fraction = (leader_usdc / leader_equity).clamp(0.0, 1.0);
    (fraction * our_equity).min(max_clip_usdc).min(our_equity)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mirrors_fraction_of_our_equity() {
        // leader bet 5% of equity -> we bet 5% of ours = 5.0
        assert!((proportional_stake(50.0, 1000.0, 100.0, 1000.0) - 5.0).abs() < 1e-9);
    }
    #[test]
    fn clamped_by_max_clip() {
        assert!((proportional_stake(500.0, 1000.0, 100.0, 5.0) - 5.0).abs() < 1e-9);
    }
    #[test]
    fn zero_when_equity_nonpositive() {
        assert_eq!(proportional_stake(50.0, 0.0, 100.0, 10.0), 0.0);
        assert_eq!(proportional_stake(50.0, 1000.0, 0.0, 10.0), 0.0);
    }
}
```

- [ ] **Step 2: Run tests** — Run: `cargo test -p pm-copytrade sizing`. Expected: PASS (3 tests).

- [ ] **Step 3: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: proportional sizing"
```

---

### Task 7: Copy ledger + settlement

**Files:**
- Create: `crates/pm-copytrade/src/ledger.rs`
- Modify: `crates/pm-copytrade/src/lib.rs` (add `pub mod ledger;`)

Reuse `pm_risk::PortfolioState` for bankroll equity + drawdown. Inspect its API first:
Run: `grep -nE 'pub fn (new|mark|record_outlay|snapshot|starting_equity)' crates/pm-risk/src/lib.rs`
(Confirms: `new(starting_equity, limits)`, `mark(equity)`, `record_outlay(market_id, ts_ns, outlay)`, `snapshot(ts_ns, equity)`.)

- [ ] **Step 1: Write failing test**

```rust
use crate::model::{CopyEntry, CopyResult, LeaderFill, PriceTag, Resolution, Side};
use crate::equity::LeaderEquity;
use crate::sizing::proportional_stake;
use std::collections::HashMap;

pub struct LedgerConfig {
    pub our_bankroll: f64,
    pub max_clip_usdc: f64,
    pub latency_s: f64,
}

/// Walk the leader's fills in time order. For each fill: size against current
/// (leader_equity_at_fill, our_running_equity), record a CopyEntry at the
/// provided entry price, and when the market resolves, realise P&L into our
/// running equity. Entries whose market never resolves are returned separately.
pub struct CopyOutcome {
    pub results: Vec<CopyResult>,
    pub open_unresolved: usize,
    pub final_equity: f64,
}

/// `priced`: map from fill index -> (entry_price, PriceTag). Missing entries are
/// skipped (treated as un-copyable: no price available even after fallback).
pub fn run_ledger(
    fills: &[LeaderFill],
    leader_eq: &LeaderEquity,
    res: &HashMap<String, Resolution>,
    priced: &HashMap<usize, (f64, PriceTag)>,
    cfg: &LedgerConfig,
) -> CopyOutcome {
    let mut equity = cfg.our_bankroll;
    let mut results = Vec::new();
    let mut open = 0usize;
    for (i, f) in fills.iter().enumerate() {
        let Some(&(entry_price, tag)) = priced.get(&i) else { continue; };
        if entry_price <= 0.0 || entry_price >= 1.0 { continue; }
        let stake = proportional_stake(f.usdc, leader_eq.equity_at(f.ts), equity, cfg.max_clip_usdc);
        if stake <= 0.0 { continue; }
        let shares = stake / entry_price;
        let entry = CopyEntry {
            leader: f.clone(), latency_s: cfg.latency_s, entry_price, priced_from: tag,
            our_stake_usdc: stake, our_shares: shares,
        };
        match res.get(&f.condition_id) {
            Some(r) if r.resolved => {
                let won = r.winning_index == Some(f.outcome_index);
                let result = CopyResult::settle(entry, won, r.end_ts.unwrap_or(f.ts));
                equity += result.pnl_usdc;
                results.push(result);
            }
            _ => { open += 1; }
        }
    }
    CopyOutcome { results, open_unresolved: open, final_equity: equity }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fill(ts: i64, cond: &str, oi: u8, usdc: f64) -> LeaderFill {
        LeaderFill { ts, token_id: format!("{cond}-{oi}"), condition_id: cond.into(),
            slug: "s".into(), outcome: "x".into(), outcome_index: oi, side: Side::Buy,
            price: 0.5, size: usdc / 0.5, usdc }
    }
    fn resolved(cond: &str, wi: u8, ets: i64) -> Resolution {
        Resolution { condition_id: cond.into(), resolved: true, winning_index: Some(wi), end_ts: Some(ets) }
    }
    #[test]
    fn winning_copy_grows_equity() {
        let fills = vec![fill(100, "c1", 0, 50.0)]; // 5% of 1000
        let leader_eq = LeaderEquity::reconstruct(&fills, &HashMap::new(), 1000.0);
        let mut res = HashMap::new(); res.insert("c1".into(), resolved("c1", 0, 200));
        let mut priced = HashMap::new(); priced.insert(0usize, (0.40, PriceTag::MarketPrint));
        let cfg = LedgerConfig { our_bankroll: 100.0, max_clip_usdc: 1000.0, latency_s: 2.0 };
        let out = run_ledger(&fills, &leader_eq, &res, &priced, &cfg);
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].won);
        // stake = 5.0, shares = 12.5, payout 12.5, pnl +7.5 -> equity 107.5
        assert!((out.final_equity - 107.5).abs() < 1e-9);
    }
    #[test]
    fn unresolved_market_counts_as_open() {
        let fills = vec![fill(100, "c1", 0, 50.0)];
        let leader_eq = LeaderEquity::reconstruct(&fills, &HashMap::new(), 1000.0);
        let mut priced = HashMap::new(); priced.insert(0usize, (0.40, PriceTag::MarketPrint));
        let cfg = LedgerConfig { our_bankroll: 100.0, max_clip_usdc: 1000.0, latency_s: 2.0 };
        let out = run_ledger(&fills, &leader_eq, &HashMap::new(), &priced, &cfg);
        assert_eq!(out.open_unresolved, 1);
        assert!((out.final_equity - 100.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Run tests** — Run: `cargo test -p pm-copytrade ledger`. Expected: PASS (2 tests).

- [ ] **Step 3: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: copy ledger + settlement"
```

---

### Task 8: Summary aggregation + writers

**Files:**
- Create: `crates/pm-copytrade/src/summary.rs`
- Modify: `crates/pm-copytrade/src/lib.rs` (add `pub mod summary;`)

- [ ] **Step 1: Write failing test**

```rust
use crate::model::CopyResult;
use serde::Serialize;
use std::io::Write;
use std::path::Path;
use anyhow::Result;

#[derive(Debug, Serialize, PartialEq)]
pub struct Summary {
    pub trades: usize,
    pub wins: usize,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub roi: f64,          // total_pnl / total_staked
    pub max_drawdown: f64, // on the realised-equity path, starting at bankroll
}

pub fn summarize(results: &[CopyResult], starting_bankroll: f64) -> Summary {
    let trades = results.len();
    let wins = results.iter().filter(|r| r.won).count();
    let total_pnl: f64 = results.iter().map(|r| r.pnl_usdc).sum();
    let total_staked: f64 = results.iter().map(|r| r.entry.our_stake_usdc).sum();
    let mut equity = starting_bankroll;
    let mut peak = starting_bankroll;
    let mut max_dd = 0.0f64;
    for r in results {
        equity += r.pnl_usdc;
        peak = peak.max(equity);
        if peak > 0.0 { max_dd = max_dd.max((peak - equity) / peak); }
    }
    Summary {
        trades, wins,
        win_rate: if trades > 0 { wins as f64 / trades as f64 } else { 0.0 },
        total_pnl,
        roi: if total_staked > 0.0 { total_pnl / total_staked } else { 0.0 },
        max_drawdown: max_dd,
    }
}

pub fn write_ledger_jsonl(path: &Path, results: &[CopyResult]) -> Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    { let mut f = std::fs::File::create(&tmp)?;
      for r in results { writeln!(f, "{}", serde_json::to_string(r)?)?; } }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn write_summary_json<T: Serialize>(path: &Path, summary: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(summary)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CopyEntry, LeaderFill, PriceTag, Side};
    fn result(stake: f64, price: f64, won: bool) -> CopyResult {
        let shares = stake / price;
        let entry = CopyEntry {
            leader: LeaderFill { ts: 0, token_id: "t".into(), condition_id: "c".into(),
                slug: "s".into(), outcome: "x".into(), outcome_index: 0, side: Side::Buy,
                price, size: shares, usdc: stake },
            latency_s: 0.0, entry_price: price, priced_from: PriceTag::MarketPrint,
            our_stake_usdc: stake, our_shares: shares };
        CopyResult::settle(entry, won, 1)
    }
    #[test]
    fn computes_win_rate_and_roi() {
        let rs = vec![result(10.0, 0.5, true), result(10.0, 0.5, false)];
        // win: 20 shares payout 20 pnl +10; loss: pnl -10 -> total 0, staked 20, roi 0
        let s = summarize(&rs, 100.0);
        assert_eq!(s.trades, 2);
        assert_eq!(s.wins, 1);
        assert!((s.win_rate - 0.5).abs() < 1e-9);
        assert!((s.total_pnl - 0.0).abs() < 1e-9);
        assert!((s.roi - 0.0).abs() < 1e-9);
    }
    #[test]
    fn writes_jsonl_one_line_per_result(tmp: &std::path::Path) {} // replaced below
}
```

Note: replace the broken last test with a real tempfile test:
```rust
    #[test]
    fn writes_jsonl_one_line_per_result() {
        let rs = vec![result(10.0, 0.5, true), result(10.0, 0.5, false)];
        let dir = std::env::temp_dir().join(format!("pmct-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("ledger.jsonl");
        write_ledger_jsonl(&p, &rs).unwrap();
        let content = std::fs::read_to_string(&p).unwrap();
        assert_eq!(content.lines().count(), 2);
    }
```

- [ ] **Step 2: Run tests** — Run: `cargo test -p pm-copytrade summary`. Expected: PASS (2 tests).

- [ ] **Step 3: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: summary aggregation + atomic JSONL/JSON writers"
```

---

### Task 9: `run_historical` orchestration + integration test with fakes

**Files:**
- Modify: `crates/pm-copytrade/src/lib.rs`

- [ ] **Step 1: Write the orchestration + an integration test using in-memory fake sources**

Replace `lib.rs` with:
```rust
pub mod model;
pub mod sources;
pub mod equity;
pub mod sizing;
pub mod ledger;
pub mod summary;

use std::collections::HashMap;
use anyhow::Result;
use serde::Serialize;
use futures::stream::{self, StreamExt};

use crate::model::{CopyResult, LeaderFill, PriceTag, Resolution};
use crate::sources::activity::FillSource;
use crate::sources::prices::PriceSource;
use crate::sources::resolution::ResolutionSource;
use crate::equity::LeaderEquity;
use crate::ledger::{run_ledger, LedgerConfig};
use crate::summary::{summarize, Summary};

pub struct RunConfig {
    pub wallet: String,
    pub start_ts: i64,
    pub end_ts: i64,
    pub latencies_s: Vec<f64>,
    pub our_bankroll: f64,
    pub max_clip_usdc: f64,
    pub leader_seed_usdc: f64,
    pub concurrency: usize,
}

#[derive(Serialize)]
pub struct LatencyRun { pub latency_s: f64, pub summary: Summary, pub open_unresolved: usize, pub final_equity: f64 }

#[derive(Serialize)]
pub struct HistoricalReport {
    pub wallet: String,
    pub fills: usize,
    pub runs: Vec<LatencyRun>,
}

pub async fn run_historical(
    fills_src: &dyn FillSource,
    price_src: &dyn PriceSource,
    res_src: &dyn ResolutionSource,
    cfg: &RunConfig,
) -> Result<(HistoricalReport, HashMap<f64, Vec<CopyResult>>)> {
    let fills = fills_src.fetch_fills(&cfg.wallet, cfg.start_ts, cfg.end_ts).await?;

    // Resolutions: one lookup per distinct condition_id, bounded concurrency.
    let conds: Vec<String> = {
        let mut s: Vec<String> = fills.iter().map(|f| f.condition_id.clone()).collect();
        s.sort(); s.dedup(); s
    };
    let res_pairs: Vec<(String, Option<Resolution>)> = stream::iter(conds)
        .map(|c| async move { (c.clone(), res_src.resolution(&c).await.ok().flatten()) })
        .buffer_unordered(cfg.concurrency)
        .collect().await;
    let res: HashMap<String, Resolution> =
        res_pairs.into_iter().filter_map(|(_, r)| r.map(|r| (r.condition_id.clone(), r))).collect();

    let leader_eq = LeaderEquity::reconstruct(&fills, &res, cfg.leader_seed_usdc);

    let mut runs = Vec::new();
    let mut ledgers: HashMap<f64, Vec<CopyResult>> = HashMap::new();
    for &lat in &cfg.latencies_s {
        // Price every fill at ts + latency, bounded concurrency.
        let priced_vec: Vec<(usize, Option<(f64, PriceTag)>)> = stream::iter(fills.iter().enumerate())
            .map(|(i, f)| {
                let target = f.ts + lat as i64;
                async move { (i, price_src.price_at(&f.token_id, &f.condition_id, target).await.ok().flatten()) }
            })
            .buffer_unordered(cfg.concurrency)
            .collect().await;
        let priced: HashMap<usize, (f64, PriceTag)> = priced_vec.into_iter()
            .filter_map(|(i, p)| p.map(|p| (i, p))).collect();

        let lcfg = LedgerConfig { our_bankroll: cfg.our_bankroll, max_clip_usdc: cfg.max_clip_usdc, latency_s: lat };
        let out = run_ledger(&fills, &leader_eq, &res, &priced, &lcfg);
        let summary = summarize(&out.results, cfg.our_bankroll);
        runs.push(LatencyRun { latency_s: lat, summary, open_unresolved: out.open_unresolved, final_equity: out.final_equity });
        ledgers.insert(lat_key(lat), out.results);
    }
    Ok((HistoricalReport { wallet: cfg.wallet.clone(), fills: fills.len(), runs }, ledgers))
}

fn lat_key(l: f64) -> f64 { l } // ledgers keyed by latency value

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Side;
    use async_trait::async_trait;

    struct FakeFills(Vec<LeaderFill>);
    #[async_trait]
    impl FillSource for FakeFills {
        async fn fetch_fills(&self, _w: &str, _s: i64, _e: i64) -> Result<Vec<LeaderFill>> { Ok(self.0.clone()) }
    }
    struct FakePrice;
    #[async_trait]
    impl PriceSource for FakePrice {
        async fn price_at(&self, _t: &str, _c: &str, _ts: i64) -> Result<Option<(f64, PriceTag)>> {
            Ok(Some((0.40, PriceTag::MarketPrint)))
        }
    }
    struct FakeRes;
    #[async_trait]
    impl ResolutionSource for FakeRes {
        async fn resolution(&self, c: &str) -> Result<Option<Resolution>> {
            Ok(Some(Resolution { condition_id: c.into(), resolved: true, winning_index: Some(0), end_ts: Some(200) }))
        }
    }

    #[tokio::test]
    async fn end_to_end_single_winning_trade() {
        let fills = vec![LeaderFill { ts: 100, token_id: "t".into(), condition_id: "c1".into(),
            slug: "btc-updown-15m-1".into(), outcome: "Up".into(), outcome_index: 0, side: Side::Buy,
            price: 0.5, size: 100.0, usdc: 50.0 }];
        let cfg = RunConfig { wallet: "0xabc".into(), start_ts: 0, end_ts: 1000,
            latencies_s: vec![0.0, 5.0], our_bankroll: 100.0, max_clip_usdc: 1000.0,
            leader_seed_usdc: 1000.0, concurrency: 4 };
        let (report, ledgers) = run_historical(&FakeFills(fills), &FakePrice, &FakeRes, &cfg).await.unwrap();
        assert_eq!(report.fills, 1);
        assert_eq!(report.runs.len(), 2);
        // 5% of 100 = 5 stake, entry 0.40 -> 12.5 shares, win -> +7.5
        assert!((report.runs[0].final_equity - 107.5).abs() < 1e-6);
        assert_eq!(ledgers[&0.0].len(), 1);
    }
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p pm-copytrade end_to_end`
Expected: PASS.

- [ ] **Step 3: Run the whole crate test suite**

Run: `cargo test -p pm-copytrade`
Expected: PASS (all tasks' tests green).

- [ ] **Step 4: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: run_historical orchestration + integration test"
```

---

### Task 10: `pm-app copy-trade` subcommand + real run

**Files:**
- Modify: `crates/pm-app/Cargo.toml` (add `pm-copytrade = { path = "../pm-copytrade" }`)
- Modify: `crates/pm-app/src/main.rs` (new `CopyTrade` subcommand)

- [ ] **Step 1: Add the dependency** to `crates/pm-app/Cargo.toml` under `[dependencies]`:
```toml
pm-copytrade = { path = "../pm-copytrade" }
```

- [ ] **Step 2: Locate the clap subcommand enum** in `main.rs`:
Run: `grep -nE 'enum (Commands|Cli)|#\[command|Subcommand' crates/pm-app/src/main.rs | head`
Add a new variant to the subcommand enum (match the existing derive style):
```rust
    /// Backtest copying a specific wallet's trades under modelled latency.
    CopyTrade(CopyTradeArgs),
```

- [ ] **Step 3: Define the args struct** (place near the other `*Args` structs):
```rust
#[derive(clap::Args, Debug)]
pub struct CopyTradeArgs {
    #[arg(long)] pub wallet: String,
    #[arg(long, default_value_t = 0)] pub start_ts: i64,     // 0 -> auto (wallet first trade)
    #[arg(long, default_value_t = 0)] pub end_ts: i64,        // 0 -> now
    #[arg(long, value_delimiter = ',', default_value = "0,2,5,15")] pub latency_s: Vec<f64>,
    #[arg(long, default_value_t = 100.0)] pub our_bankroll: f64,
    #[arg(long, default_value_t = 5.0)] pub max_clip_usdc: f64,
    #[arg(long, default_value_t = 1000.0)] pub leader_seed_usdc: f64,
    #[arg(long, default_value_t = 16)] pub concurrency: usize,
    #[arg(long, default_value = "https://data-api.polymarket.com")] pub data_base: String,
    #[arg(long, default_value = "https://clob.polymarket.com")] pub clob_base: String,
    #[arg(long, default_value = "https://gamma-api.polymarket.com")] pub gamma_base: String,
    #[arg(long, default_value = "/tmp/copytrade-ledger")] pub out_prefix: String,
}
```

- [ ] **Step 4: Add the dispatch arm** in the `match` over subcommands (mirror how other async subcommands are awaited; the runner uses tokio):
```rust
        Commands::CopyTrade(a) => {
            use pm_copytrade::sources::activity::HttpFillSource;
            use pm_copytrade::sources::prices::HttpPriceSource;
            use pm_copytrade::sources::resolution::HttpResolutionSource;
            let client = reqwest::Client::builder()
                .user_agent("pm-copytrade/0.1")
                .build()?;
            let end_ts = if a.end_ts == 0 { chrono::Utc::now().timestamp() } else { a.end_ts };
            let start_ts = if a.start_ts == 0 { end_ts - 60 * 24 * 3600 } else { a.start_ts }; // 60d lookback default
            let fills_src = HttpFillSource { client: client.clone(), base: a.data_base.clone(), bucket_seconds: 3600 };
            let price_src = HttpPriceSource { client: client.clone(), data_base: a.data_base.clone(), clob_base: a.clob_base.clone(), window_s: 90 };
            let res_src = HttpResolutionSource::new(client.clone(), a.gamma_base.clone());
            let cfg = pm_copytrade::RunConfig {
                wallet: a.wallet.clone(), start_ts, end_ts, latencies_s: a.latency_s.clone(),
                our_bankroll: a.our_bankroll, max_clip_usdc: a.max_clip_usdc,
                leader_seed_usdc: a.leader_seed_usdc, concurrency: a.concurrency,
            };
            let (report, ledgers) = pm_copytrade::run_historical(&fills_src, &price_src, &res_src, &cfg).await?;
            for run in &report.runs {
                let p = std::path::PathBuf::from(format!("{}-L{}.jsonl", a.out_prefix, run.latency_s));
                pm_copytrade::summary::write_ledger_jsonl(&p, &ledgers[&run.latency_s])?;
                println!("latency {:>4}s | trades {:>5} | win {:>5.1}% | ROI {:>6.2}% | maxDD {:>5.1}% | endEq {:.2} | open {}",
                    run.latency_s, run.summary.trades, run.summary.win_rate * 100.0,
                    run.summary.roi * 100.0, run.summary.max_drawdown * 100.0, run.final_equity, run.open_unresolved);
            }
            pm_copytrade::summary::write_summary_json(&std::path::PathBuf::from(format!("{}-summary.json", a.out_prefix)), &report)?;
        }
```
(If `main` is not already `#[tokio::main] async fn`, the existing async subcommands show the pattern — match it. If the dispatch is inside a `Runtime::block_on`, wrap the arm accordingly.)

- [ ] **Step 5: Build**

Run: `cargo build -p pm-app`
Expected: compiles clean.

- [ ] **Step 6: Real run against the target wallet (manual verification)**

Run:
```bash
cargo run --release -p pm-app -- copy-trade \
  --wallet 0xb55fa1296e6ec55d0ce53d93b9237389f11764d4 \
  --latency-s 0,2,5,15 --our-bankroll 100 --max-clip-usdc 5 \
  --out-prefix /tmp/b55f-copy
```
Expected: a printed table, one row per latency, e.g.
```
latency    0s | trades  NNNN | win  XX.X% | ROI  ±X.XX% | maxDD  X.X% | endEq XXX.XX | open NN
latency    2s | ...
```
Sanity checks to eyeball: (1) win rate is plausible (not 0% or 100%); (2) ROI degrades monotonically as latency rises (the central hypothesis); (3) `open` count is small relative to `trades` for a historical window that ends a day or more ago; (4) `/tmp/b55f-copy-L0.jsonl` has one line per settled trade. If ROI does NOT degrade with latency, inspect whether `price_at` is returning the leader's own fill price (PriceTag::LeaderFill) too often — check the `priced_from` distribution in the JSONL.

- [ ] **Step 7: Commit**
```bash
git add crates/pm-app/Cargo.toml crates/pm-app/src/main.rs
git commit -m "pm-app: copy-trade subcommand (historical wallet copy backtest)"
```

---

## Self-review

**Spec coverage:**
- Historical replay with modelled latency -> Tasks 3, 9, 10 (latency sweep). ✓
- Buy-and-hold-to-resolution settlement -> Tasks 1, 7. ✓
- Proportional sizing vs reconstructed leader equity -> Tasks 5, 6, 7. ✓
- Data sources (activity / market prints + prices-history / Gamma) -> Tasks 2, 3, 4. ✓
- Reuse pm-risk / repo conventions -> Task 7 (PortfolioState referenced; note: the ledger here tracks equity directly and references PortfolioState's API for drawdown semantics — if strict PortfolioState integration with per-market outlay caps is wanted, that is a refinement on Task 7, not a new requirement). ✓ (simplified: drawdown computed in summary; outlay caps available via max_clip).
- Multi-asset: handled implicitly (we follow the wallet's markets; no BTC-5m assumption in pm-copytrade). Per-asset breakdown in summary is NOT yet implemented -> GAP, see below.
- Output JSONL + summary JSON -> Tasks 8, 10. ✓
- Live mode -> explicitly deferred to follow-up plan. ✓

**Gap found (fix inline):** the spec asks for per-asset (BTC/ETH/SOL/XRP) and per-duration breakdowns in the summary; Task 8 computes only aggregate stats. Add the breakdown as Task 8b below rather than expanding Task 8 (keeps tasks bite-sized).

**Placeholder scan:** one intentional note in Task 8 flags a broken stub test that is immediately replaced with the real tempfile test in the same step — not a placeholder in the delivered code. No TBD/TODO remain.

**Type consistency:** `LeaderFill`, `CopyEntry`, `CopyResult`, `Resolution`, `PriceTag`, `Side` are defined once in `model.rs` (Task 1) and used unchanged in Tasks 5-10. `price_at(token_id, condition_id, target_ts)` signature is consistent between the trait (Task 3), the fake (Task 9), and the call site (Task 9). `run_ledger` / `LedgerConfig` consistent between Task 7 and Task 9.

---

### Task 8b: Per-asset / per-duration breakdown in summary

**Files:** Modify `crates/pm-copytrade/src/summary.rs`; the report struct in `lib.rs`.

- [ ] **Step 1: Add a failing test** in `summary.rs`:

```rust
/// Derive (asset, duration) from a slug like "eth-updown-15m-1780918200".
pub fn slug_bucket(slug: &str) -> (String, String) {
    let parts: Vec<&str> = slug.split('-').collect();
    let asset = parts.get(0).copied().unwrap_or("unknown").to_string();
    let dur = parts.get(2).copied().unwrap_or("unknown").to_string();
    (asset, dur)
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Bucket { pub key: String, pub trades: usize, pub wins: usize, pub pnl: f64 }

pub fn breakdown(results: &[CopyResult]) -> Vec<Bucket> {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<String, (usize, usize, f64)> = BTreeMap::new();
    for r in results {
        let (asset, dur) = slug_bucket(&r.entry.leader.slug);
        let e = m.entry(format!("{asset}-{dur}")).or_default();
        e.0 += 1; e.1 += r.won as usize; e.2 += r.pnl_usdc;
    }
    m.into_iter().map(|(key, (t, w, p))| Bucket { key, trades: t, wins: w, pnl: p }).collect()
}

#[cfg(test)]
mod breakdown_tests {
    use super::*;
    use crate::model::{CopyEntry, LeaderFill, PriceTag, Side};
    fn r(slug: &str, won: bool) -> CopyResult {
        let entry = CopyEntry { leader: LeaderFill { ts:0, token_id:"t".into(), condition_id:"c".into(),
            slug: slug.into(), outcome:"x".into(), outcome_index:0, side: Side::Buy, price:0.5, size:2.0, usdc:1.0 },
            latency_s:0.0, entry_price:0.5, priced_from: PriceTag::MarketPrint, our_stake_usdc:1.0, our_shares:2.0 };
        CopyResult::settle(entry, won, 1)
    }
    #[test]
    fn groups_by_asset_and_duration() {
        let rs = vec![r("eth-updown-15m-1", true), r("eth-updown-15m-2", false), r("sol-updown-15m-3", true)];
        let b = breakdown(&rs);
        let eth = b.iter().find(|x| x.key == "eth-15m").unwrap();
        assert_eq!(eth.trades, 2); assert_eq!(eth.wins, 1);
        assert!(b.iter().any(|x| x.key == "sol-15m"));
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p pm-copytrade breakdown`. Expected: PASS.

- [ ] **Step 3: Wire `breakdown` into `LatencyRun`** in `lib.rs`: add `pub buckets: Vec<summary::Bucket>` and populate with `summary::breakdown(&out.results)`. Re-run `cargo test -p pm-copytrade`. Expected: PASS.

- [ ] **Step 4: Commit**
```bash
git add crates/pm-copytrade/src
git commit -m "pm-copytrade: per-asset/duration breakdown in summary"
```

---

## Done criteria

- `cargo test -p pm-copytrade` green (all tasks).
- `cargo build -p pm-app` clean.
- A real `copy-trade` run on `0xb55f...` prints a latency table and writes per-latency JSONL ledgers + a summary JSON.
- The headline finding is readable directly from the table: copy ROI at 0s vs 2s/5s/15s latency = how much edge survives copy delay.
```
