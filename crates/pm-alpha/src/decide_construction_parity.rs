//! Construction-parity gate: the backtest and live paths must BUILD identical
//! `DecisionInputs`, so `decide_entry` makes value-identical decisions on both.
//!
//! Thin CI front-end over [`crate::equivalence`], the single source of truth for
//! the backtest-vs-live `DecisionInputs` construction harness (the runnable
//! report bin `pm-app/src/bin/decide_construction_parity.rs` wraps the same
//! module, so the two can never drift apart). This test runs the full harness
//! and asserts real coverage of every gate it claims to exercise, then zero
//! decision/construction mismatches.
//!
//! This was `exo_fade_equivalence`, historically "GATE B". It outlived the
//! strategy: it imports nothing from `pm-strategy` and nothing from the deleted
//! fade, only `decide_entry` and the frozen fade `DecideConfig`, both of which
//! are retained framework capital. The frozen config is now a FIXTURE here (a
//! fixed, realistic parameterization to compare the two paths under), not a
//! deployment target. See the promotion gate in `docs/CONSTRAINTS.md`.

#![cfg(test)]

use crate::equivalence;

#[test]
fn decide_construction_parity() {
    let r = equivalence::run();
    let t = &r.tally;
    assert!(r.scenarios_built >= 20, "need >= 20 scenarios, have {}", r.scenarios_built);

    eprintln!("============ DECIDE INPUT-CONSTRUCTION PARITY ============");
    eprintln!("scenarios built            : {}", r.scenarios_built);
    eprintln!("scenarios compared         : {}", t.compared);
    eprintln!("decision mismatches        : {}", t.mismatches);
    eprintln!("belief p_exo range         : [{:.4}, {:.4}]", t.p_min, t.p_max);
    eprintln!("sigma-floor (<{:.1}) fired   : {}", r.cfg.min_entry_sigma_bps, t.below_floor);
    eprintln!("saturday-skips fired       : {}", t.saturday_skips);
    eprintln!("side-pick coverage         : Yes={} No={}", t.yes_picks(), t.no_picks());
    eprintln!("actions: Enter={} Skip={} Rearm={}", t.enters(), t.skips(), t.rearms());
    if t.mismatches == 0 {
        eprintln!("VERDICT: backtest-construction == live-construction. PARITY: PASS.");
    } else {
        eprintln!("DIVERGENCES (first {}):", t.first_diffs.len());
        for d in &t.first_diffs {
            eprintln!("  {d}");
        }
        eprintln!("VERDICT: backtest-construction != live-construction. PARITY: FAIL.");
    }

    // The load-bearing assertions: real coverage of the gates the test claims to
    // exercise, then zero decision/construction mismatches.
    assert_eq!(t.compared, 2 * r.scenarios_built, "every scenario must compare in both passes");
    assert!(t.enters() > 0, "no Enter decisions: edge/side-pick path not exercised");
    assert!(
        t.yes_picks() > 0 && t.no_picks() > 0,
        "both Yes and No side-picks must be exercised (NO=1-yes mapping)"
    );
    assert!(t.below_floor > 0, "sigma floor never exercised");
    assert!(t.saturday_skips > 0, "skip_saturday never exercised");
    assert_eq!(
        t.mismatches, 0,
        "PARITY FAIL: backtest and live construction paths produced divergent decide_entry \
         decisions. First divergences:\n{}",
        t.first_diffs.join("\n")
    );
}
