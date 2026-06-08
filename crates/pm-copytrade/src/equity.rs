use crate::model::{LeaderFill, Resolution};
use std::collections::HashMap;

pub struct LeaderEquity {
    seed: f64,
    points: Vec<(i64, f64)>,
}

impl LeaderEquity {
    pub fn reconstruct(fills: &[LeaderFill], res: &HashMap<String, Resolution>, seed_usdc: f64) -> Self {
        let mut events: Vec<(i64, f64)> = Vec::new();
        for f in fills {
            events.push((f.ts, -f.usdc));
            if let Some(r) = res.get(&f.condition_id) {
                if let (true, Some(wi), Some(ets)) = (r.resolved, r.winning_index, r.end_ts) {
                    if wi == f.outcome_index { events.push((ets, f.size)); }
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
        LeaderEquity { seed: seed_usdc, points }
    }

    /// Equity just before `ts`: last recorded point strictly before ts,
    /// or seed if ts is at or before the first event.
    pub fn equity_at(&self, ts: i64) -> f64 {
        match self.points.partition_point(|(t, _)| *t < ts) {
            0 => self.seed,
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
        let fills = vec![fill(100, "c1", 0, 10.0, 25.0)];
        let mut res = HashMap::new();
        res.insert("c1".into(), Resolution { condition_id: "c1".into(), resolved: true,
            winning_index: Some(0), end_ts: Some(200) });
        let eq = LeaderEquity::reconstruct(&fills, &res, 1000.0);
        assert!((eq.equity_at(150) - 990.0).abs() < 1e-9);
        assert!((eq.equity_at(250) - 1015.0).abs() < 1e-9);
    }
    #[test]
    fn buy_then_loss_keeps_equity_down() {
        let fills = vec![fill(100, "c1", 1, 10.0, 25.0)];
        let mut res = HashMap::new();
        res.insert("c1".into(), Resolution { condition_id: "c1".into(), resolved: true,
            winning_index: Some(0), end_ts: Some(200) });
        let eq = LeaderEquity::reconstruct(&fills, &res, 1000.0);
        assert!((eq.equity_at(250) - 990.0).abs() < 1e-9);
    }
}
