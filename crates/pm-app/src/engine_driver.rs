//! Leg-pairing and parquet→EngineEvent loading for the pm-engine backtest driver.
//!
//! Each Polymarket binary market has two legs (YES / NO), stored as separate
//! `MarketHandle` rows with `outcome` in {"Up","Yes"} or {"Down","No"}.
//! `pair_legs` merges both legs' `ReplayEvent` slices into one ts-ordered
//! `Vec<EngineEvent>`, forward-filling the counter-leg's ladder on every update.
//!
//! Tie-break rule: when a YES and NO event share an identical `ts_ns`, the YES
//! event is processed first (state updated, event emitted) then the NO event
//! (state updated, second event emitted). The output length therefore equals
//! `yes.len() + no.len()` in all cases.

// Public API consumed by the engine driver (Task 8); suppress dead_code until
// the caller is wired in.
#![allow(dead_code)]

use anyhow::{Context, Result, anyhow};
use pm_engine::event::EngineEvent;
use pm_telonex_loader::{Channel, TelonexStore, load_book_snapshot_async};
use pm_types::{MarketId, NoBook, ReplayEvent};
use std::sync::Arc;

use crate::discovery::MarketHandle;

// Task 8: wire PM trade tape to SimExchange.on_trade

/// Returns `true` when the outcome label maps to the YES leg.
fn is_yes_leg(outcome: &str) -> Option<bool> {
    if outcome.eq_ignore_ascii_case("up") || outcome.eq_ignore_ascii_case("yes") {
        Some(true)
    } else if outcome.eq_ignore_ascii_case("down") || outcome.eq_ignore_ascii_case("no") {
        Some(false)
    } else {
        None
    }
}

/// Build a `NoBook` from the bids/asks of a NO-leg `ReplayEvent`.
fn no_book_from_replay(ev: &ReplayEvent) -> NoBook {
    NoBook {
        bids: ev.bids,
        asks: ev.asks,
    }
}

/// Merge a YES-leg and a NO-leg `ReplayEvent` slice for one market into a
/// single timestamp-ordered `Vec<EngineEvent>`.
///
/// Each output event carries:
/// - `replay`: the latest YES `ReplayEvent` with `market_id` forced to `market`.
///   Before the first YES snapshot the event is NOT emitted (we have nothing to
///   carry for the strategy).
/// - `no_book`: the latest NO ladder seen at or before this timestamp.
///   Before the first NO snapshot `NoBook::default()` (all-zero) is used.
///
/// **Tie-break:** when `yes.ts_ns == no.ts_ns`, YES is applied first, then NO.
/// Each application emits one event, so a tie produces two consecutive events
/// at the same timestamp.
///
/// Both input slices are assumed to be sorted ascending by `ts_ns` (the loader
/// sorts them after loading).
pub fn pair_legs(
    market: MarketId,
    yes: &[ReplayEvent],
    no: &[ReplayEvent],
) -> Vec<EngineEvent> {
    let capacity = yes.len() + no.len();
    let mut out = Vec::with_capacity(capacity);

    let mut latest_yes: Option<ReplayEvent> = None;
    let mut latest_no: NoBook = NoBook::default();

    let mut yi = 0usize;
    let mut ni = 0usize;

    while yi < yes.len() || ni < no.len() {
        let take_yes = match (yes.get(yi), no.get(ni)) {
            (Some(y), Some(n)) => y.ts_ns <= n.ts_ns, // YES wins on tie
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };

        if take_yes {
            let mut ev = yes[yi];
            ev.market_id = market;
            latest_yes = Some(ev);
            yi += 1;
            out.push(EngineEvent::Market { replay: ev, no_book: latest_no });
        } else {
            let no_ev = &no[ni];
            latest_no = no_book_from_replay(no_ev);
            ni += 1;
            if let Some(yes_ev) = latest_yes {
                // Stamp the event at the NO update's ts so the stream stays
                // monotonically ordered. The YES book fields are carried forward
                // unchanged; only ts_ns and market_id are overwritten.
                let mut carried = yes_ev;
                carried.ts_ns = no_ev.ts_ns;
                carried.market_id = market;
                out.push(EngineEvent::Market { replay: carried, no_book: latest_no });
            }
            // If no YES event has arrived yet, we have nothing to emit for the
            // strategy; the NO ladder will be carried once YES arrives.
        }
    }

    out
}

/// Load both legs for one market and return a merged `Vec<EngineEvent>`.
///
/// `handles` must contain exactly two `MarketHandle`s for the same underlying
/// market (one YES leg and one NO leg). The YES/NO distinction is made via
/// `outcome_label_resolved_yes` (Up/Yes → YES; Down/No → NO).
///
/// Loading is async (parquet over S3/local object-store); call this from within
/// a tokio context. The resulting `Vec` can be wrapped in a `SliceFeed` for
/// synchronous engine consumption.
pub async fn load_market_paired(
    store: &TelonexStore,
    market: MarketId,
    yes_handle: &MarketHandle,
    no_handle: &MarketHandle,
) -> Result<Vec<EngineEvent>> {
    let store_arc: Arc<dyn object_store::ObjectStore> = store.store();

    let yes_path = store
        .resolve_asset_day("polymarket", Channel::BookSnapshot25, &yes_handle.date, &yes_handle.asset_id)
        .await
        .with_context(|| format!("resolve YES leg for {}", yes_handle.slug))?;

    let no_path = store
        .resolve_asset_day("polymarket", Channel::BookSnapshot25, &no_handle.date, &no_handle.asset_id)
        .await
        .with_context(|| format!("resolve NO leg for {}", no_handle.slug))?;

    let (yes_events, _) = load_book_snapshot_async(store_arc.clone(), yes_path, market)
        .await
        .with_context(|| format!("load YES leg for {}", yes_handle.slug))?;

    // Use a temporary market id for the NO leg so it doesn't collide; pair_legs
    // ignores NO market_ids (only uses bids/asks for NoBook).
    let no_tmp_id = MarketId(market.0 ^ 0xFFFF_FFFF);
    let (no_events, _) = load_book_snapshot_async(store_arc, no_path, no_tmp_id)
        .await
        .with_context(|| format!("load NO leg for {}", no_handle.slug))?;

    Ok(pair_legs(market, &yes_events, &no_events))
}

/// Classify a slice of two `MarketHandle`s into (yes_handle, no_handle).
///
/// Returns an error if the outcome labels are not both resolvable or if both
/// handles map to the same side.
pub fn split_yes_no(
    handles: &[MarketHandle],
) -> Result<(&MarketHandle, &MarketHandle)> {
    let mut yes: Option<&MarketHandle> = None;
    let mut no: Option<&MarketHandle> = None;
    for h in handles {
        match is_yes_leg(&h.outcome) {
            Some(true) => yes = Some(h),
            Some(false) => no = Some(h),
            None => return Err(anyhow!("unrecognized outcome label {:?} for {}", h.outcome, h.slug)),
        }
    }
    match (yes, no) {
        (Some(y), Some(n)) => Ok((y, n)),
        (None, _) => Err(anyhow!("no YES leg found in handles")),
        (_, None) => Err(anyhow!("no NO leg found in handles")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pm_types::{BookLevel, MarketId, NoBook, ReplayEvent, ReplayFlags, tape::TAPE_DEPTH};

    fn yes_event(ts: i64, ask0_price: f32) -> ReplayEvent {
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        asks[0] = BookLevel { price: ask0_price, size: 500.0 };
        ReplayEvent {
            ts_ns: ts,
            market_id: MarketId(99),
            yes_mid: ask0_price - 0.01,
            yes_bid: ask0_price - 0.02,
            yes_ask: ask0_price,
            volume: 0.0,
            bids: Default::default(),
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    fn no_event(ts: i64, ask0_price: f32) -> ReplayEvent {
        let mut asks = [BookLevel::default(); TAPE_DEPTH];
        asks[0] = BookLevel { price: ask0_price, size: 400.0 };
        ReplayEvent {
            ts_ns: ts,
            market_id: MarketId(88),
            yes_mid: ask0_price - 0.01,
            yes_bid: ask0_price - 0.02,
            yes_ask: ask0_price,
            volume: 0.0,
            bids: Default::default(),
            asks,
            spot_price: 0.0,
            flags: ReplayFlags::BOOK_UPDATE,
        }
    }

    #[test]
    fn pair_legs_forward_fills_counter_leg_by_ts() {
        // YES leg: ts=1000 ask0=0.60, ts=3000 ask0=0.62
        // NO  leg: ts=2000 ask0=0.42
        let yes = vec![yes_event(1000, 0.60), yes_event(3000, 0.62)];
        let no = vec![no_event(2000, 0.42)];

        let evs = pair_legs(MarketId(0), &yes, &no);

        // All three raw events should produce output (2 YES + 1 NO that has a
        // prior YES to carry forward).
        assert_eq!(evs.len(), 3, "merged length should be yes.len() + no.len()");

        // Events must be in ts order.
        let ts: Vec<i64> = evs.iter().map(|e| e.ts()).collect();
        assert_eq!(ts, vec![1000, 2000, 3000]);

        // ts=1000: first YES event, no NO snapshot yet → empty NoBook.
        let EngineEvent::Market { replay: r0, no_book: nb0 } = evs[0];
        assert_eq!(r0.ts_ns, 1000);
        assert!((r0.yes_ask - 0.60).abs() < 1e-6);
        assert_eq!(nb0, NoBook::default(), "ts=1000 must have empty no_book");

        // ts=2000: NO update carries the latest YES (ask=0.60) forward.
        // The event's ts_ns is stamped at the NO event time (2000) so the stream
        // stays monotonically ordered; the YES book fields are unchanged.
        let EngineEvent::Market { replay: r1, no_book: nb1 } = evs[1];
        assert_eq!(r1.ts_ns, 2000, "NO-triggered event stamped at NO ts");
        assert!((r1.yes_ask - 0.60).abs() < 1e-6, "YES ask carried forward");
        assert!((nb1.asks[0].price - 0.42).abs() < 1e-6, "no_book should carry NO ask0=0.42");

        // ts=3000: second YES update, no_book forward-filled from the ts=2000 NO.
        let EngineEvent::Market { replay: r2, no_book: nb2 } = evs[2];
        assert_eq!(r2.ts_ns, 3000);
        assert!((r2.yes_ask - 0.62).abs() < 1e-6);
        assert!(
            (nb2.asks[0].price - 0.42).abs() < 1e-6,
            "ts=3000 event must forward-fill no_book.asks[0].price=0.42, got {}",
            nb2.asks[0].price
        );
    }

    #[test]
    fn pair_legs_all_market_ids_set_to_target() {
        let yes = vec![yes_event(1000, 0.60)];
        let no = vec![no_event(2000, 0.42)];
        let evs = pair_legs(MarketId(7), &yes, &no);
        for ev in &evs {
            assert_eq!(ev.market_id(), MarketId(7));
        }
    }

    #[test]
    fn pair_legs_empty_no_leg() {
        let yes = vec![yes_event(1000, 0.60), yes_event(2000, 0.62)];
        let evs = pair_legs(MarketId(0), &yes, &[]);
        assert_eq!(evs.len(), 2);
        for ev in &evs {
            let EngineEvent::Market { no_book, .. } = ev;
            assert_eq!(*no_book, NoBook::default());
        }
    }

    #[test]
    fn pair_legs_empty_yes_leg() {
        // With no YES events, NO updates have nothing to carry forward; no output.
        let no = vec![no_event(1000, 0.42)];
        let evs = pair_legs(MarketId(0), &[], &no);
        assert_eq!(evs.len(), 0, "no YES data → nothing to emit");
    }

    #[test]
    fn pair_legs_tie_break_yes_first() {
        // Both legs have an event at ts=1000; YES should appear first.
        let yes = vec![yes_event(1000, 0.60)];
        let no = vec![no_event(1000, 0.42)];
        let evs = pair_legs(MarketId(0), &yes, &no);
        assert_eq!(evs.len(), 2);
        // First event: YES update, no_book still empty.
        let EngineEvent::Market { replay: r0, no_book: nb0 } = evs[0];
        assert!((r0.yes_ask - 0.60).abs() < 1e-6);
        assert_eq!(nb0, NoBook::default(), "tie-break: YES first, no_book empty");
        // Second event: NO update, carries YES forward with new no_book.
        let EngineEvent::Market { no_book: nb1, .. } = evs[1];
        assert!((nb1.asks[0].price - 0.42).abs() < 1e-6);
    }

    #[test]
    fn split_yes_no_classifies_correctly() {
        let h_yes = MarketHandle {
            asset_id: "a1".into(),
            slug: "btc-updown-yes".into(),
            close_ts: 0,
            outcome: "Up".into(),
            date: "2026-05-01".into(),
        };
        let h_no = MarketHandle {
            asset_id: "a2".into(),
            slug: "btc-updown-no".into(),
            close_ts: 0,
            outcome: "Down".into(),
            date: "2026-05-01".into(),
        };
        let handles = [h_yes.clone(), h_no.clone()];
        let (y, n) = split_yes_no(&handles).unwrap();
        assert_eq!(y.outcome, "Up");
        assert_eq!(n.outcome, "Down");
    }
}
