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
pub struct LatencyRun {
    pub latency_s: f64,
    pub summary: Summary,
    pub open_unresolved: usize,
    pub final_equity: f64,
    pub buckets: Vec<summary::Bucket>,
}

#[derive(Serialize)]
pub struct HistoricalReport {
    pub wallet: String,
    pub fills: usize,
    pub runs: Vec<LatencyRun>,
}

// Map key for the ledgers HashMap: latency_s rounded to nearest millisecond.
// Task 10 must look up results with ledgers[&lat_key(run.latency_s)].
fn lat_key(l: f64) -> i64 {
    (l * 1000.0).round() as i64
}

pub async fn run_historical(
    fills_src: &dyn FillSource,
    price_src: &dyn PriceSource,
    res_src: &dyn ResolutionSource,
    cfg: &RunConfig,
) -> Result<(HistoricalReport, HashMap<i64, Vec<CopyResult>>)> {
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
    let mut ledgers: HashMap<i64, Vec<CopyResult>> = HashMap::new();
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
        let buckets = summary::breakdown(&out.results);
        runs.push(LatencyRun { latency_s: lat, summary, open_unresolved: out.open_unresolved, final_equity: out.final_equity, buckets });
        ledgers.insert(lat_key(lat), out.results);
    }
    Ok((HistoricalReport { wallet: cfg.wallet.clone(), fills: fills.len(), runs }, ledgers))
}

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
        assert_eq!(ledgers[&lat_key(0.0)].len(), 1);
    }
}
