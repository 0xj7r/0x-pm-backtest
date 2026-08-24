//! Construction-parity report: the backtest and live paths must BUILD identical
//! `DecisionInputs`.
//!
//! Thin runnable front-end over `pm_alpha::equivalence`, the single source of
//! truth for the backtest-vs-live `DecisionInputs` construction harness (the CI
//! test `pm-alpha/src/decide_construction_parity.rs` wraps the same module, so
//! this bin and cargo test can never drift apart). Runs the full harness and
//! prints the PASS/FAIL report; exits 1 on any divergence.
//!
//! The bin exists alongside the test because the shell gate stack
//! (`scripts/pipeline/harness_data_audit.sh`) wants one command with an exit
//! code and a legible verdict block, which reads better as an audit artifact
//! than `cargo test` output for a single test.
//!
//! Run: cargo run -p pm-app --bin decide_construction_parity

fn main() {
    let r = pm_alpha::equivalence::run();
    let t = &r.tally;

    println!();
    println!("============ DECIDE INPUT-CONSTRUCTION PARITY ============");
    println!("scenarios built            : {}", r.scenarios_built);
    println!("scenarios compared         : {}", t.compared);
    println!("decision mismatches        : {}", t.mismatches);
    println!("belief p_exo range         : [{:.4}, {:.4}]", t.p_min, t.p_max);
    println!("sigma-floor (<{:.1}) fired   : {}", r.cfg.min_entry_sigma_bps, t.below_floor);
    println!(
        "side-pick coverage         : Yes={} No={}",
        t.yes_picks(),
        t.no_picks(),
    );
    println!("decisions by action        :");
    for (a, n) in &t.action_counts {
        let name = match a {
            0 => "Enter",
            1 => "Skip",
            _ => "Rearm",
        };
        println!("    {name:8} {n}");
    }

    if t.mismatches == 0 {
        println!();
        println!(
            "VERDICT: backtest-construction == live-construction across all {} compared",
            t.compared
        );
        println!("scenarios. decide_entry receives byte-identical DecisionInputs on both paths,");
        println!("so it makes value-identical decisions. PARITY: PASS.");
    } else {
        println!();
        println!("DIVERGENCES (first {}):", t.first_diffs.len());
        for d in &t.first_diffs {
            println!("  {d}");
        }
        println!();
        println!(
            "VERDICT: backtest-construction != live-construction. {} divergent. PARITY: FAIL.",
            t.mismatches
        );
        std::process::exit(1);
    }
}
