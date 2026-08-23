# Why the live implementation diverged: the complete causal explanation

**Date:** 2026-07-03
**Purpose:** one place that explains, end to end, why live results diverged so
badly from backtests in June. Three separate things went wrong and were
initially conflated; this doc separates them, explains the mechanism of each,
and maps each to its fix. The companion docs carry the raw evidence; this one
carries the WHY.

---

## The three divergences (they are different problems)

| # | what | June cost | status |
|---|---|---|---|
| D1 | fade_live was a REIMPLEMENTATION whose inputs drifted | ~-$1,050 | binary deleted; shared engine since Jun 16 |
| D2 | Decision instability: the correct engine is chaos-sensitive near the strike on fast tape | realization 0.34 vs 0.82 break-even on chop days | measured, gated (pre-registered), consensus built |
| D3 | Process failures around the engine (sizing automation, ledger lies, discretionary trades) | ~-$850+ | governance-as-code, all closed |

D1 and D3 are ordinary engineering/process failures. D2 is the deep one, and
the reason "our live implementation diverged" even AFTER the engine was
provably identical byte-for-byte. Understanding D2 requires walking through
how the model actually computes a trade.

---

## D2 explained: the physics of the divergence

### Step 1: what the belief computes

At every decision tick the model computes (crates/pm-alpha/src/model.rs):

```
eff_spot = 0.25 * spot + 0.75 * (perp - median_basis)
d        = ln(eff_spot / strike) / sigma_bar
P(up)    = Phi(d)                      # normal CDF
edge     = P(up) - ask                 # enter when >= 0.12
```

`sigma_bar` is the expected move for the REMAINING window, a few bps. The
critical property: **Phi is steepest exactly at the strike.** When eff_spot
is within ~1 sigma of the strike, a 5 bps input error moves P(up) by ~20
points. Away from the strike the same error moves it by almost nothing.

### Step 2: what "input error" means live

The engine consumes three live streams: Binance spot trades, Binance futures
trades, and the Polymarket book, each over its own WebSocket. Two processes
(or live vs the recorded archive) never see identical streams:

- ticks arrive with different network jitter and in different batch
  boundaries;
- the perp buffer's `median_basis` is computed over samples taken every 10s
  from each process's OWN arrival history, so the basis correction differs by
  a few bps between any two observers;
- during a fast burst, one observer's "now" price is 1-3 ticks behind
  another's, which at burst speed is 5-15 bps.

None of this matters when eff_spot is far from the strike. All of it matters
in the Phi-steep zone.

### Step 3: why the entry TRIGGER amplifies rather than averages the noise

The engine does not average over the window; it fires the moment
`edge >= 0.12` first becomes true. That is a first-passage event: whichever
observer's noisy eff_spot path crosses the threshold first, fires first, at
ITS reading. Three consequences, all measured:

1. **Selection for overshoot.** The moments that clear the threshold are
   disproportionately the moments an observer's input excursion is largest.
   A claimed edge of 0.32 six seconds after the open is far more likely to be
   a reading artifact than a real 32-point mispricing (nobody leaves that on
   the table for long).
2. **Observer-dependent sides.** Near the strike, observer A's excursion can
   cross UP's threshold while observer B's crosses DOWN's. Measured directly:
   two IDENTICAL engines on the SAME box picked opposite sides on 42% of
   early entries (docs/archive/2026-07/decision-stability-2026-07.md). This is not a bug in
   either process; it is what a threshold rule does to noisy inputs.
3. **Adverse selection on phantoms.** When the excursion was an artifact, the
   engine bought the side the phantom move favored; the market had not
   actually moved, so resolution reverts: measured 39.6% hit on those live
   entries vs 69.9% for the recorded observer, whose excursions happened to
   align with how the tape settled. Same code, different draw.

### Step 4: why the whipsaw days concentrated the damage

Whipsaw = spot repeatedly crossing the strike = maximum time spent in the
Phi-steep zone, and window opens are the worst (strike is SET at the open, so
the first seconds are by construction at-the-strike). June 16-18 therefore
produced many early, extreme-conviction, observer-dependent entries. Those
entries also pay the maximum fee (the 0.07*p*(1-p) curve peaks at p=0.5) and
double-clip re-arm could fire a second wrong-side clip. At $50 clips (the
night-scaler's doing, D3) on an $850-class account, three such days were
nearly terminal.

### Step 5: why every backtest was blind to this

The backtest replays ONE recorded stream. It is internally consistent: its
eff_spot excursions and its resolution outcomes come from the same tape, so
threshold-crossing trades look systematically better than any OTHER observer
of the same market would experience. This is not look-ahead bias or a data
bug; it is a single-draw fallacy. It is invisible to parameter sweeps,
holdout windows, walk-forwards, and even to the byte-identical decision
parity test (GATE B), because all of those validate the FUNCTION, and the
function is fine. The problem is the function's sensitivity to which
observer supplies its arguments.

**The general lesson, now in PROD.md policy:** backtest P&L is an upper bound
realized only by decisions that are STABLE across observers. Any strategy
whose profits concentrate in observer-sensitive decisions will replicate this
failure regardless of how clean its backtest is.

---

## Why D1 happened (the reimplementation bug, for completeness)

fade_live shared only the decision FUNCTION with the validated engine and
rebuilt the input pipeline (spot buffer, perp buffer, basis, strike capture)
from scratch. Its cold-start perp bootstrap used 1-minute klines against a
tick-level spot buffer, so its median basis (and hence eff_spot, 75%
perp-weighted) was systematically wrong: a permanent, large version of the D2
microstate error. Result: 61% opposite-side vs the validated twin, plus
over-entry. The fix was architectural and is in place: live execution is a
thin JSONL-tailing consumer of the one shared engine; there is no second
input pipeline left to drift. The equivalence harness that guards this is
single-sourced and runs in CI (crates/pm-alpha/src/equivalence.rs).

---

## The fix map (each mechanism -> its specific counter)

| mechanism | counter | status |
|---|---|---|
| Second input pipeline can drift (D1) | one engine, executor consumes its JSONL; CI equivalence gate | LIVE |
| First-seconds = at-the-strike by construction | `min_secs_from_open=15` | pre-registered, judged ~Jul 9-10 |
| Extreme claimed conviction = likely artifact | `max_p_side=0.85` | pre-registered, same verdict |
| Observer-dependent decisions in general | consensus execution: trade only when two independent observers agree | built + tested, deploys with the gate |
| Flash beliefs (side just flipped) | dwell gate (`belief_dwell_s >= 30`) | telemetry live; fallback candidate |
| Residual unstable trades that slip through | 1% fractional sizing (ruin-proof), hold-to-redemption (one fee leg) | LIVE |
| Process failures (D3) | reduce-only automation, on-chain reconciliation, no discretionary trades, deployment gate | LIVE |
| Independent validation | ce25 ($288k wallet): ZERO entries <5s, 79% after 60s across 360k fills | evidence on file |

---

## Promotion runbook (exact steps, when the gate verdict passes)

1. Confirm verdict: 4/4 criteria in
   docs/archive/2026-07/stability-gate-preregistration-2026-07.md against the soak week's
   `twin_agreement.jsonl` + `realization.jsonl`.
2. Config change (one commit, main):
   add `--min-secs-from-open 15` and `--max-p-side 0.85` to
   `scripts/ops/shadow_final_gated_flags.sh` (CLI wiring shipped b9d98e52);
   update the frozen-config table in docs/PROD.md.
3. Dublin: `git pull && cargo build --release -p pm-app`,
   `systemctl --user restart pm-shadow-final` (twin B and executor follow via
   ExecStartPost). 1h vol warmup.
4. 48h paper parity re-soak on the gated config (scorecard PASS x2).
5. Deploy consensus: start `consensus_tail.py` (systemd unit to be added at
   that step), point executor env `PM_SHADOW_JSONL_PATH` at
   `~/data/pm-alpha/shadow-consensus`, repoint the parity monitor at the
   consensus stream, restart executor. 48h combined paper soak.
6. Micro-live (needs explicit user go): remove `~/fade.kill`;
   `PM_SHADOW_CLIP_FRAC=0.01`, ceiling $10; 14 days; measure realization
   ratio vs daily replay; target >= 0.85; no changes mid-window.

Documentation index for this topic: postmortem (D1),
live-divergence-analysis + decision-stability (D2 evidence),
june-deploy-cycle-postmortem memory (D3),
stability-gate-preregistration (the frozen test),
IMPLEMENTATION-AND-ROLLOUT (master plan).
