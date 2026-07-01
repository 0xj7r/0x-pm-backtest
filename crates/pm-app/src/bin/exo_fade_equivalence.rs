//! GATE B parity test: exogenous-fade INPUT-CONSTRUCTION equivalence.
//!
//! `pm_alpha::decide_entry` is the single decision authority (the SSOT). Because
//! BOTH the backtest (`harness::replay::execute`) and the live mirror
//! (`pm_app::shadow::ShadowCore::decide`) feed it `DecisionInputs`, proving the
//! two paths make identical decisions reduces to proving they BUILD identical
//! `DecisionInputs` on the same market data: same belief, same book touches,
//! same sigma, same basis-momentum. This test constructs those inputs two ways
//! and asserts the resulting `decide_entry` decisions are value-identical.
//!
//!   (a) BACKTEST construction — mirrors `belief_pass`: p_up via `model.evaluate`
//!       on an `ExoState{spot,perp}`, yes_ask/no_buy/mid from a `BookTick`,
//!       sigma_bar_bps from `ev.raw`, basis_mom_60s_bps from the perp basis.
//!   (b) LIVE construction — mirrors `shadow::decide`: p_up via `model.evaluate`
//!       on an `ExoState` built from a rolling `SpotHistory` buffer + a
//!       `PerpState{trades,oi:[],funding:[]}`, yes_ask/no_buy/mid read from a
//!       `Ladder` (top-of-book BTreeMap reads, the exact shadow semantics).
//!       `fade_live.rs` in the agent repo mirrors shadow byte-for-byte, so
//!       shadow == backtest transitively covers the live agent.
//!
//! Both paths use the SAME `AlphaModel` (realized / 3600s / perp 0.75) and the
//! SAME frozen `DecideConfig` (edge 0.12, sigma floor 3.0, skip-Sat, min_marginal
//! 0.04, rearm 0.08, hold) and a SHARED evolving `EntryState`. Each decision is
//! quantized to {action, side, notional_milli, limit_milli, hold} and compared
//! exactly. PASS iff zero mismatches; any real construction discrepancy is a
//! genuine parity bug and is reported, never papered over.
//!
//! Run: cargo run -p pm-app --bin exo_fade_equivalence

use std::collections::BTreeMap;
use std::collections::VecDeque;

use pm_alpha::decide::{
    DecideConfig, DecisionInputs, EntryAction, EntryDecision, EntryState, EntryStateDelta,
    decide_entry,
};
use pm_alpha::harness::{BookTick, EntryMode, Side};
use pm_alpha::{AlphaModel, AlphaModelConfig, ExoState, MarketMeta, PerpState, Token, VolEstimator};
use pm_types::tape::{BookLevel, TAPE_DEPTH};
use pm_types::{SpotHistory, SpotTick};

const NS_PER_S: i64 = 1_000_000_000;
const WINDOW_S: i64 = 300; // BTC-5m
const WARMUP_S: i64 = 3_700; // > vol3600 lookback so warm-up passes
const STOP_BEFORE_CLOSE_S: i64 = 90; // shadow's STOP_BEFORE_CLOSE_S constant
const SHADOW_NOTIONAL_USDC: f64 = 50.0;

/// The frozen live/shadow `DecideConfig` (hold, edge 0.12, sigma floor 3.0,
/// skip-Sat, min_marginal 0.04, rearm 0.08). Identical object for both paths.
fn frozen_cfg() -> DecideConfig {
    DecideConfig {
        edge_threshold: 0.12,
        min_marginal_edge: 0.04,
        min_entry_sigma_bps: 3.0,
        max_entry_sigma_bps: 0.0,
        skip_saturday: true,
        rearm_edge: 0.08,
        clip_cooldown_ms: 5000,
        exit_after_s: 0,
        enter_within_close_s: 0,
        stop_before_close_s: STOP_BEFORE_CLOSE_S as u32,
        notional_usdc: SHADOW_NOTIONAL_USDC,
        kelly_sizing: false,
        min_p_side: 0.0,
        max_p_side: 1.0,
        min_entry_ask: 0.0,
        max_entry_ask: 1.0,
        min_secs_from_open: 0,
        vol_sizing_ref_bps: 0.0,
        vol_sizing_lo: 0.5,
        vol_sizing_hi: 2.0,
        basis_mom_agree: 1.0,
        basis_mom_disagree: 1.0,
        entry_mode: EntryMode::Fade,
        align_min_mid: 0.55,
        skip_calm: false,
        only_calm: false,
        skip_expanded_mixed: false,
        skip_expanded_high_flip: false,
        skip_open_fav_gap: false,
        open_fav_p_min: 0.90,
        open_fav_ask_max: 0.60,
        open_fav_secs: 5,
        pause_after_consec_losses: 0,
        max_rearm_entry_ask: 0.0,
        skip_spot_misalign_s: 0,
        skip_spot_against_all: false,
    }
}

/// The validated belief model: realized vol over 3600s, perp price weight 0.75.
fn alpha_model() -> AlphaModel {
    AlphaModel {
        cfg: AlphaModelConfig {
            vol_lookback_s: 3600,
            vol_sample_dt_s: 1,
            momentum_lookback_s: 0,
            momentum_weight: 1.0,
            perp_price_weight: 0.75,
            vol_estimator: VolEstimator::Realized,
            ..AlphaModelConfig::default()
        },
        calibrator: None,
        dir_model: None,
    }
}

/// One synthetic deterministic BTC-5m scenario.
struct Scenario {
    name: String,
    open_ns: i64,
    /// Net spot move from window-open to the decision instant (USD). The strike
    /// is the spot-at-open, so this signed move (vs the per-bar vol band) sets
    /// how far the belief sits from 50/50 and which side it favours.
    move_usd: f64,
    /// Spot oscillation amplitude (USD) — sets realized vol / sigma.
    wobble: f64,
    /// YES book ask at the decision instant (0.70..0.94); NO = 1 - yes_bid.
    yes_ask: f32,
    /// YES bid = yes_ask - spread.
    spread: f32,
}

/// Deterministic 3700s FLAT-drift warmup (wobble only, so the vol estimate is
/// stable) + an in-window linear ramp of `move_usd` to the decision instant, 1
/// print/sec, plus a matching perp tape offset by a constant basis so the
/// perp-price-weight 0.75 blend is exercised. Strike = spot-at-open, so the
/// belief reflects `move_usd` against the realized-vol band (mid-range, not
/// saturated).
fn build_tapes(s: &Scenario) -> (Vec<SpotTick>, Vec<SpotTick>) {
    let mut spot = Vec::new();
    let mut perp = Vec::new();
    let start = s.open_ns - WARMUP_S * NS_PER_S;
    let close = s.open_ns + WINDOW_S * NS_PER_S;
    let decision_ns = s.open_ns + 150 * NS_PER_S;
    let base = 64_000.0_f64;
    const PERP_BASIS: f64 = 8.0; // constant perp-minus-spot offset (USD)
    let mut t = start;
    let mut step = 0i64;
    while t <= close {
        let osc = ((step as f64) * 0.7).sin() * s.wobble;
        // Linear ramp of `move_usd` over [open, decision]; flat before/after.
        let ramp = if t <= s.open_ns {
            0.0
        } else if t >= decision_ns {
            s.move_usd
        } else {
            s.move_usd * (t - s.open_ns) as f64 / (decision_ns - s.open_ns) as f64
        };
        let price = base + ramp + osc;
        spot.push(SpotTick {
            ts_ns: t,
            price,
            quantity: 3.0,
            is_buyer_maker: step % 3 == 0,
        });
        perp.push(SpotTick {
            ts_ns: t,
            price: price + PERP_BASIS,
            quantity: 3.0,
            is_buyer_maker: false,
        });
        t += NS_PER_S;
        step += 1;
    }
    (spot, perp)
}

/// The book at the decision instant: top-of-book YES + a couple deeper levels.
/// `decide_entry` only reads the touch (yes_ask / no_buy / mid), but populating
/// depth keeps the two builders honest about which level they read.
fn book_levels(s: &Scenario) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
    let yes_ask = s.yes_ask as f64;
    let yes_bid = (s.yes_ask - s.spread) as f64;
    let bids = vec![
        (yes_bid, 600.0),
        ((yes_bid - 0.01).max(0.01), 400.0),
        ((yes_bid - 0.02).max(0.01), 300.0),
    ];
    let asks = vec![
        (yes_ask, 600.0),
        ((yes_ask + 0.01).min(0.99), 400.0),
        ((yes_ask + 0.02).min(0.99), 300.0),
    ];
    (bids, asks)
}

// ---------- BACKTEST construction (mirrors replay::belief_pass) ----------

fn build_book_tick(s: &Scenario, ts_ns: i64) -> BookTick {
    let (bids_v, asks_v) = book_levels(s);
    let mut bids = [BookLevel::default(); TAPE_DEPTH];
    let mut asks = [BookLevel::default(); TAPE_DEPTH];
    for (i, (p, sz)) in bids_v.iter().enumerate().take(TAPE_DEPTH) {
        bids[i] = BookLevel { price: *p as f32, size: *sz as f32 };
    }
    for (i, (p, sz)) in asks_v.iter().enumerate().take(TAPE_DEPTH) {
        asks[i] = BookLevel { price: *p as f32, size: *sz as f32 };
    }
    BookTick {
        ts_ns,
        yes_bid: bids[0].price,
        yes_ask: asks[0].price,
        bids,
        asks,
        no_bid: 0.0,
        no_ask: 0.0,
        no_bids: [BookLevel::default(); TAPE_DEPTH],
        no_asks: [BookLevel::default(); TAPE_DEPTH],
    }
}

/// Build `DecisionInputs` exactly as `belief_pass` does at a decision tick.
fn backtest_inputs(
    s: &Scenario,
    spot: &SpotHistory,
    perp: &PerpState,
    model: &AlphaModel,
    meta: MarketMeta,
    ts_ns: i64,
) -> Option<DecisionInputs> {
    let tick = build_book_tick(s, ts_ns);
    let state = ExoState {
        spot,
        perp: Some(perp),
        ref_spot: None,
        market: meta,
        now_ns: ts_ns,
    };
    let ev = model.evaluate(&state, false)?;
    // belief_pass gates: mid must exist and yes_ask > yes_bid; no_buy must exist.
    let mid = tick.mid()?;
    if !(tick.yes_ask > tick.yes_bid) {
        return None;
    }
    let no_buy = tick.no_buy_price()?;
    let basis_mom_60s_bps = {
        let bn = perp.basis_frac(spot, ts_ns);
        let bp = perp.basis_frac(spot, ts_ns - 60_000_000_000);
        match (bn, bp) {
            (Some(bn), Some(bp)) => (bn - bp) * 1e4,
            _ => 0.0,
        }
    };
    Some(DecisionInputs {
        p_exo: ev.p,
        dir_p_up: None,
        dir_model_active: false,
        yes_ask: tick.yes_ask as f64,
        no_buy,
        mid,
        sigma_bar_bps: ev.raw.sigma_bar_bps,
        basis_mom_60s_bps,
        regime_at_decision: None,
        clip_index: 0,
        spot_ret_10s_bps: pm_alpha::harness::spot_ret_bps(spot, ts_ns, 10),
        spot_ret_30s_bps: pm_alpha::harness::spot_ret_bps(spot, ts_ns, 30),
        spot_ret_60s_bps: pm_alpha::harness::spot_ret_bps(spot, ts_ns, 60),
        spot_ret_120s_bps: pm_alpha::harness::spot_ret_bps(spot, ts_ns, 120),
        spot_ret_300s_bps: pm_alpha::harness::spot_ret_bps(spot, ts_ns, 300),
        spot_ret_600s_bps: pm_alpha::harness::spot_ret_bps(spot, ts_ns, 600),
        spot_ret_900s_bps: pm_alpha::harness::spot_ret_bps(spot, ts_ns, 900),
    })
}

// ---------- LIVE construction (mirrors shadow::ShadowCore::decide) ----------

/// Shadow's `Ladder`: integer-keyed top-of-book BTreeMap. Replicated verbatim
/// (best_ask = lowest ask, best_bid = highest bid, mid = their average) so the
/// touch reads match shadow byte-for-byte without depending on its private mod.
#[derive(Default)]
struct Ladder {
    bids: BTreeMap<i64, f64>,
    asks: BTreeMap<i64, f64>,
}

fn price_key(price: f64) -> i64 {
    (price * 100_000.0).round() as i64
}
fn key_price(key: i64) -> f64 {
    key as f64 / 100_000.0
}

impl Ladder {
    fn from_levels(bids: &[(f64, f64)], asks: &[(f64, f64)]) -> Self {
        let mut l = Ladder::default();
        for (p, sz) in bids {
            if *p > 0.0 && *p < 1.0 && *sz > 0.0 {
                l.bids.insert(price_key(*p), *sz);
            }
        }
        for (p, sz) in asks {
            if *p > 0.0 && *p < 1.0 && *sz > 0.0 {
                l.asks.insert(price_key(*p), *sz);
            }
        }
        l
    }
    fn best_ask(&self) -> Option<(f64, f64)> {
        self.asks.iter().next().map(|(k, s)| (key_price(*k), *s))
    }
    fn best_bid(&self) -> Option<(f64, f64)> {
        self.bids.iter().next_back().map(|(k, s)| (key_price(*k), *s))
    }
    fn mid(&self) -> Option<f64> {
        Some((self.best_bid()?.0 + self.best_ask()?.0) / 2.0)
    }
}

/// The live spot buffer (a `VecDeque<SpotTick>`, drained into a `SpotHistory`
/// on each decision) + the live perp buffer (`PerpState{trades,oi:[],funding}`).
struct LiveFeeds {
    spot: VecDeque<SpotTick>,
    perp: VecDeque<SpotTick>,
}

impl LiveFeeds {
    fn new(spot: &[SpotTick], perp: &[SpotTick]) -> Self {
        Self {
            spot: spot.iter().copied().collect(),
            perp: perp.iter().copied().collect(),
        }
    }
    fn spot_history(&self) -> SpotHistory {
        SpotHistory::new(self.spot.iter().copied().collect())
    }
    fn perp_state(&self) -> PerpState {
        PerpState {
            trades: SpotHistory::new(self.perp.iter().copied().collect()),
            oi: Vec::new(),
            funding: Vec::new(),
        }
    }
}

/// Build `DecisionInputs` exactly as `shadow::decide` does: belief from an
/// ExoState over the live buffers, touches from the Up/Down `Ladder`s. Shadow
/// reads the Up token book for yes and the Down token book for no_buy; here the
/// Down ladder is the synthetic complement (1 - yes), matching the synthetic-NO
/// fallback the backtest also uses (no real NO ladder in either path).
fn live_inputs(
    s: &Scenario,
    feeds: &LiveFeeds,
    model: &AlphaModel,
    meta: MarketMeta,
    ts_ns: i64,
) -> Option<DecisionInputs> {
    let spot = feeds.spot_history();
    let perp = feeds.perp_state();
    let (bids_v, asks_v) = book_levels(s);
    let up_ladder = Ladder::from_levels(&bids_v, &asks_v);
    // Synthetic Down ladder: NO ask = 1 - YES bid, NO bid = 1 - YES ask. This is
    // the same synthetic complement the backtest's `no_buy_price` uses when no
    // real NO ladder is loaded — keeping both paths on identical NO pricing.
    let down_bids: Vec<(f64, f64)> =
        asks_v.iter().map(|(p, sz)| ((1.0 - p).clamp(0.0, 1.0), *sz)).collect();
    let down_asks: Vec<(f64, f64)> =
        bids_v.iter().map(|(p, sz)| ((1.0 - p).clamp(0.0, 1.0), *sz)).collect();
    let down_ladder = Ladder::from_levels(&down_bids, &down_asks);

    let state = ExoState {
        spot: &spot,
        perp: Some(&perp),
        ref_spot: None,
        market: meta,
        now_ns: ts_ns,
    };
    let ev = model.evaluate(&state, false)?;
    let (up_ask, _) = up_ladder.best_ask()?;
    let (down_ask, _) = down_ladder.best_ask()?;
    let mid = up_ladder.mid()?;
    // Shadow does not thread basis-mom (the frozen agree/disagree are 1.0 so it
    // never affects the decision), but to prove FULL input parity we compute it
    // here from the same primitives the backtest uses.
    let basis_mom_60s_bps = {
        let bn = perp.basis_frac(&spot, ts_ns);
        let bp = perp.basis_frac(&spot, ts_ns - 60_000_000_000);
        match (bn, bp) {
            (Some(bn), Some(bp)) => (bn - bp) * 1e4,
            _ => 0.0,
        }
    };
    Some(DecisionInputs {
        p_exo: ev.p,
        dir_p_up: None,
        dir_model_active: false,
        yes_ask: up_ask,
        no_buy: down_ask,
        mid,
        sigma_bar_bps: ev.raw.sigma_bar_bps,
        basis_mom_60s_bps,
        regime_at_decision: None,
        clip_index: 0,
        spot_ret_10s_bps: pm_alpha::harness::spot_ret_bps(&spot, ts_ns, 10),
        spot_ret_30s_bps: pm_alpha::harness::spot_ret_bps(&spot, ts_ns, 30),
        spot_ret_60s_bps: pm_alpha::harness::spot_ret_bps(&spot, ts_ns, 60),
        spot_ret_120s_bps: pm_alpha::harness::spot_ret_bps(&spot, ts_ns, 120),
        spot_ret_300s_bps: pm_alpha::harness::spot_ret_bps(&spot, ts_ns, 300),
        spot_ret_600s_bps: pm_alpha::harness::spot_ret_bps(&spot, ts_ns, 600),
        spot_ret_900s_bps: pm_alpha::harness::spot_ret_bps(&spot, ts_ns, 900),
    })
}

/// Quantized decision for an exact value compare.
#[derive(Debug, Clone, PartialEq, Eq)]
struct QDecision {
    action: u8,
    side: u8,
    notional_milli: i64,
    limit_milli: i64,
    hold: bool,
}

fn quantize(action: EntryAction, side: Side, d: &EntryDecision) -> QDecision {
    QDecision {
        action: match action {
            EntryAction::Enter => 0,
            EntryAction::Skip => 1,
            EntryAction::Rearm => 2,
        },
        side: match side {
            Side::Yes => 0,
            Side::No => 1,
        },
        notional_milli: (d.target_notional * 1000.0).round() as i64,
        limit_milli: (d.marketable_limit_price * 10000.0).round() as i64,
        hold: d.hold_to_redemption,
    }
}

/// 2026-06-13 is a Saturday (per the frozen-config tests); 06-12 a Friday.
fn open_for(sat_straddle: bool) -> i64 {
    use chrono::{TimeZone, Utc};
    // Place the decision instant ~midway in the window; for the Saturday case
    // open just before midnight Fri->Sat so the decision tick lands on Sat.
    if sat_straddle {
        // 2026-06-13 00:00:30 UTC open -> decision ~150s in is Saturday.
        Utc.with_ymd_and_hms(2026, 6, 13, 0, 0, 30)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap()
    } else {
        Utc.with_ymd_and_hms(2026, 6, 12, 12, 0, 0)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap()
    }
}

fn scenarios() -> Vec<Scenario> {
    let mut out = Vec::new();
    // 20 base scenarios: alternating Up/Down moves, yes_ask swept 0.70..0.94, the
    // move and the wobble (=> sigma) both varied so the side-pick, the edge gate,
    // and the NO=1-yes mapping are all exercised at non-saturated beliefs.
    for i in 0..20u32 {
        let up = i % 2 == 0;
        let yes_ask = 0.70 + (i % 5) as f32 * 0.06; // 0.70,0.76,0.82,0.88,0.94
        // Signed move scaled so belief lands mid-range against the vol band.
        let move_usd = if up { 1.0 } else { -1.0 } * (6.0 + (i % 4) as f64 * 4.0);
        let wobble = 12.0 + (i % 4) as f64 * 6.0; // 12..30 USD -> sigma above floor
        out.push(Scenario {
            name: format!("base_{i}_{}", if up { "up" } else { "down" }),
            open_ns: open_for(false),
            move_usd,
            wobble,
            yes_ask,
            spread: 0.02,
        });
    }
    // Saturday-straddling case: exercise skip_saturday (both paths share the SAME
    // frozen skip-Sat config, so both must skip identically).
    out.push(Scenario {
        name: "saturday_straddle".to_string(),
        open_ns: open_for(true),
        move_usd: 10.0,
        wobble: 20.0,
        yes_ask: 0.78,
        spread: 0.02,
    });
    // Low-sigma case: near-flat tape -> sigma < 3 bps -> the floor fires.
    out.push(Scenario {
        name: "low_sigma_floor".to_string(),
        open_ns: open_for(false),
        move_usd: 0.02,
        wobble: 0.05,
        yes_ask: 0.74,
        spread: 0.02,
    });
    out
}

/// Running totals for one comparison pass.
#[derive(Default)]
struct Tally {
    compared: usize,
    mismatches: usize,
    first_diffs: Vec<String>,
    action_counts: BTreeMap<u8, usize>,
    sides_seen: BTreeMap<u8, usize>,
    p_min: f64,
    p_max: f64,
    below_floor: usize,
}

/// Build inputs both ways for one scenario and decision instant, run
/// `decide_entry` on each with the same `state`+`cfg`, and fold the comparison
/// into `t`. Returns the (identical) delta so the caller can evolve the state.
fn compare_one(
    s: &Scenario,
    model: &AlphaModel,
    cfg: &DecideConfig,
    state: &EntryState,
    t: &mut Tally,
) -> EntryStateDelta {
    let open_ns = s.open_ns;
    let close_ns = open_ns + WINDOW_S * NS_PER_S;
    // Decision instant: 150s into the window (inside the entry window, well
    // before the 90s stop-before-close deadline).
    let ts_ns = open_ns + 150 * NS_PER_S;
    let meta = MarketMeta {
        token: Token::Btc,
        window_secs: WINDOW_S as u32,
        open_ts_ns: open_ns,
        close_ts_ns: close_ns,
        strike: 64_000.0, // = spot-at-open (the proxy both paths use); see build_tapes
    };
    let (spot_v, perp_v) = build_tapes(s);
    let spot_hist = SpotHistory::new(spot_v.clone());
    let perp_state = PerpState {
        trades: SpotHistory::new(perp_v.clone()),
        oi: Vec::new(),
        funding: Vec::new(),
    };
    let feeds = LiveFeeds::new(&spot_v, &perp_v);

    let bt = backtest_inputs(s, &spot_hist, &perp_state, model, meta, ts_ns);
    let lv = live_inputs(s, &feeds, model, meta, ts_ns);

    let (bt, lv) = match (bt, lv) {
        (Some(a), Some(b)) => (a, b),
        (a, b) => {
            // One path produced inputs where the other stood down: a divergence
            // (only a true both-None is benign).
            if a.is_some() != b.is_some() {
                t.mismatches += 1;
                if t.first_diffs.len() < 8 {
                    t.first_diffs.push(format!(
                        "scenario={} INPUT-BUILD divergence: backtest={} live={}",
                        s.name,
                        if a.is_some() { "Some" } else { "None" },
                        if b.is_some() { "Some" } else { "None" },
                    ));
                }
            }
            return EntryStateDelta::default();
        }
    };

    // The constructed inputs must be identical (decide_entry is pure, so
    // identical inputs => identical output). Belief-derived fields (p_exo, sigma,
    // basis_mom) come from the SAME `model.evaluate` over the SAME spot/perp
    // tapes and are bit-identical. Book touches differ only by representation:
    // the backtest stores book prices as f32 (`BookTick`) while the live path
    // quantizes them through the cent-grid integer key (`price_key`, x1e5 round).
    // Both faithfully encode the same book; the residual is ~1e-8, far below the
    // cent price grid and the decision's marketable-limit (1e-4) resolution. So
    // belief fields are compared exactly; book touches at the book's own (1e-6).
    const PX_TOL: f64 = 1e-6;
    let inputs_match = bt.p_exo == lv.p_exo
        && bt.sigma_bar_bps == lv.sigma_bar_bps
        && bt.basis_mom_60s_bps == lv.basis_mom_60s_bps
        && (bt.yes_ask - lv.yes_ask).abs() < PX_TOL
        && (bt.no_buy - lv.no_buy).abs() < PX_TOL
        && (bt.mid - lv.mid).abs() < PX_TOL;

    let (bt_dec, bt_delta) = decide_entry(&bt, ts_ns, open_ns, close_ns, state, None, cfg);
    let (lv_dec, lv_delta) = decide_entry(&lv, ts_ns, open_ns, close_ns, state, None, cfg);
    let q_bt = quantize(bt_dec.action, bt_dec.side, &bt_dec);
    let q_lv = quantize(lv_dec.action, lv_dec.side, &lv_dec);

    t.compared += 1;
    *t.action_counts.entry(q_bt.action).or_default() += 1;
    *t.sides_seen.entry(q_bt.side).or_default() += 1;
    t.p_min = t.p_min.min(bt.p_exo);
    t.p_max = t.p_max.max(bt.p_exo);
    if bt.sigma_bar_bps < cfg.min_entry_sigma_bps {
        t.below_floor += 1;
    }

    // The deltas must match too (loop-carried rearm/cooldown parity).
    let delta_match = bt_delta.set_armed == lv_delta.set_armed
        && bt_delta.set_next_entry_ns == lv_delta.set_next_entry_ns
        && bt_delta.inc_clips == lv_delta.inc_clips;

    if !inputs_match || q_bt != q_lv || !delta_match {
        t.mismatches += 1;
        if t.first_diffs.len() < 8 {
            let q_eq = q_bt == q_lv;
            t.first_diffs.push(format!(
                "scenario={} [inputs_match={inputs_match} q_eq={q_eq} delta_match={delta_match}]\n    dp={:.3e} dyes={:.3e} dno={:.3e} dmid={:.3e} dsig={:.3e} dbmom={:.3e}\n    DECISION bt: {:?}\n    DECISION lv: {:?}",
                s.name,
                bt.p_exo - lv.p_exo,
                bt.yes_ask - lv.yes_ask,
                bt.no_buy - lv.no_buy,
                bt.mid - lv.mid,
                bt.sigma_bar_bps - lv.sigma_bar_bps,
                bt.basis_mom_60s_bps - lv.basis_mom_60s_bps,
                q_bt, q_lv,
            ));
        }
    }
    bt_delta
}

fn main() {
    let cfg = frozen_cfg();
    let model = alpha_model();
    let scenarios = scenarios();

    let mut t = Tally { p_min: f64::INFINITY, p_max: f64::NEG_INFINITY, ..Tally::default() };

    // PASS 1 — fresh per-scenario state (each synthetic market starts armed, as
    // both `replay::execute` and `shadow::decide` reset per market). This gives
    // each scenario its intended gate outcome: side-pick, edge gate, sigma floor,
    // Saturday-skip.
    for s in &scenarios {
        let fresh = EntryState { armed: true, next_entry_ns: i64::MIN };
        compare_one(s, &model, &cfg, &fresh, &mut t);
    }

    // PASS 2 — a SINGLE shared EntryState threaded through the whole sequence and
    // evolved by the (identical) delta, exercising the rearm/cooldown loop-carried
    // state parity across consecutive decisions.
    let mut state = EntryState { armed: true, next_entry_ns: i64::MIN };
    for s in &scenarios {
        let delta = compare_one(s, &model, &cfg, &state, &mut t);
        if let Some(a) = delta.set_armed {
            state.armed = a;
        }
        if let Some(n) = delta.set_next_entry_ns {
            state.next_entry_ns = n;
        }
    }

    let total = scenarios.len();
    let compared = t.compared;
    let mismatches = t.mismatches;
    let first_diffs = t.first_diffs;
    let action_counts = t.action_counts;
    let sides_seen = t.sides_seen;
    let (p_min, p_max, below_floor) = (t.p_min, t.p_max, t.below_floor);

    println!();
    println!("============ EXO-FADE INPUT-CONSTRUCTION EQUIVALENCE ============");
    println!("scenarios built            : {total}");
    println!("scenarios compared         : {compared}");
    println!("decision mismatches        : {mismatches}");
    println!("belief p_exo range         : [{p_min:.4}, {p_max:.4}]");
    println!("sigma-floor (<{:.1}) fired   : {below_floor}", cfg.min_entry_sigma_bps);
    println!(
        "side-pick coverage         : Yes={} No={}",
        sides_seen.get(&0).copied().unwrap_or(0),
        sides_seen.get(&1).copied().unwrap_or(0),
    );
    println!("decisions by action        :");
    for (a, n) in &action_counts {
        let name = match a {
            0 => "Enter",
            1 => "Skip",
            _ => "Rearm",
        };
        println!("    {name:8} {n}");
    }

    if mismatches == 0 {
        println!();
        println!(
            "VERDICT: backtest-construction == live-construction across all {compared} compared"
        );
        println!("scenarios. decide_entry receives byte-identical DecisionInputs on both paths,");
        println!("so the fade makes value-identical decisions. GATE B: PASS.");
    } else {
        println!();
        println!("DIVERGENCES (first {}):", first_diffs.len());
        for d in &first_diffs {
            println!("  {d}");
        }
        println!();
        println!("VERDICT: backtest-construction != live-construction. {mismatches} divergent. GATE B: FAIL.");
        std::process::exit(1);
    }
}
