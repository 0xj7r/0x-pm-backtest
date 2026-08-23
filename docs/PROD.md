# PROD: BTC-5m Exogenous Fade (canonical reference)

Last updated: 2026-07-03. This is the single source of truth for what runs in
production. If another doc disagrees with this one, this one wins.

Why live results can diverge from backtests, and every counter-measure:
docs/WHY-LIVE-DIVERGED.md. Rollout state and plan:
docs/archive/2026-07/IMPLEMENTATION-AND-ROLLOUT-2026-07.md. Pending gate (judged ~Jul 9-10):
docs/archive/2026-07/stability-gate-preregistration-2026-07.md.

**Standing policy (from the decision-stability finding):** backtest P&L is an
upper bound realized only by decisions that are stable across observers. Any
config or strategy whose profits concentrate in observer-sensitive decisions
(early-window, extreme-conviction, near-threshold) must be treated as
unvalidated regardless of backtest quality.

## 1. The strategy

Exogenous BSM fade on Polymarket BTC-5m up/down markets. A Black-Scholes-style
belief is computed from CEX spot + perp state (perp weight 0.75, realized vol
over 3600s). When the Polymarket book disagrees with the belief by more than the
edge threshold, buy the cheap side and hold to redemption. The edge is staleness
capture: CEX state updates faster than the PM book reprices (~150-250ms), so we
monetize episodic dislocations, not superior calibration.

## 2. THE candidate config (single-candidate policy, 2026-07-10)

POLICY: there is exactly ONE production candidate config at any time. Every
5m measurement stream (twins, fast engine, consensus, executor) and every
replay baseline runs THE candidate and nothing else. Research variants live
in backtests only; a live A/B experiment requires an explicit, documented,
time-boxed exception with its own stream name, and at most one may exist at
a time. Rationale: docs/deep-review-2026-07-10.md - running a zoo of configs
made validation evidence and live measurement disjoint, and an unvalidated
config reached the deploy path unnoticed.

THE candidate = the BARE FROZEN config. It is the only config with full-depth
evidence: positive in all five validated months at truthful latency
(Feb +$11.9k / Mar +$18.5k / Apr +$13.1k / May +$17.3k / Jun +$2.0k @1250ms;
substantially more at the fast engine's 750ms class).

| Flag | Value |
|---|---|
| `edge_threshold` | 0.12 |
| `exit_after_s` | 0 (hold to redemption) |
| `perp_price_weight` | 0.75 |
| `vol_estimator` / `vol_lookback_s` | realized / 3600 |
| `rearm_edge` | 0.08 |
| `max_clips` | 2 |
| `min_entry_sigma_bps` | 3.0 |
| `clip_cooldown_ms` | 5000 |
| `stop_before_close_s` | 90 (pinned explicitly; see M-1) |
| `skip_saturday` | true |
| `min_marginal_edge` | 0.04 |
| Base gates (`min_entry_ask`, `open_fav_*`, `skip_spot_misalign_s`) | **NONE** |
| Regime gates (`skip_calm`, `skip_expanded_mixed`, ...) | **NONE** (removed 2026-07-01, overfit) |

Gate history (why the candidate is bare):
- Regime gates: selected on Jun 14-19 live tape; blocked 99% of entries OOS
  Jun 20-30. Removed 2026-07-01.
- Base gates (min_entry_ask 0.45 + open_fav + misalign 30): adopted un-
  validated during the June drawdown firefight (governance archaeology in
  docs/deep-review-2026-07-10.md section 7). The package is backtest-NEGATIVE
  (June @1250ms: -$459 vs bare +$1,957; min_entry_ask alone -$2,106 - it
  blocks the cheap-underdog payoff tail). Removed 2026-07-10. Any gate
  returns only through the front door: multi-month truthful-latency evidence
  + the deployment gate.
- v1 stability gate (min_secs_from_open 15 + max_p_side 0.85): validated as
  a COST across all five months (-6% to -34%); its live BENEFIT hypothesis
  is pre-registered and judged on the candidate-config soak. Not in the
  candidate unless it passes.

Flags SSOT: `scripts/ops/shadow_candidate_flags.sh` (enforced against the
Rust canon by the config-parity test). Reference command:

```bash
pm-app shadow \
  --edge-threshold 0.12 \
  --perp-price-weight 0.75 \
  --exit-after-s 0 \
  --rearm-edge 0.08 \
  --max-clips 2 \
  --min-entry-sigma-bps 3.0 \
  --vol-estimator realized \
  --vol-lookback-s 3600 \
  --skip-saturday \
  --stop-before-close-s 90 \
  --out-dir ~/data/pm-alpha/shadow-final
```

## 3. Live execution path

| Component | Role |
|---|---|
| shadow-final (`pm-app shadow`) | Computes decisions, writes `would_enter` JSONL to `~/data/pm-alpha/shadow-final/` |
| `shadow_exec_tail` (polymarket-agent repo) | Tails the JSONL and submits orders; live == shadow by construction |
| Paper mode (`--paper`) | Parity soaks before arming real money |
| Kill switch | `touch ~/fade.kill` on the box; executor stands down |
| Parity monitor | matched / orphan / missed_ref counts; **any orphan or side mismatch = kill** |

`fade_live` (a separate reimplementation) is retired: see
`docs/postmortem-2026-06-16-fade-live-divergence.md`.

## 4. Accounting policy

Canonical dollar accounting is the pm-app alpha harness with
`--fee-curve-rate 0.07 --latency-ms 250`
(see `docs/fill-model-calibration-2026-07.md` for the live-fill calibration).
At-touch Python scorers over shadow JSONL are research-only: no fees, no
latency, numbers not comparable to harness or live P&L.

## 5. Governance

| Rule | Detail |
|---|---|
| Sizing | Clip = 1% of current bankroll via `PM_SHADOW_CLIP_FRAC`; ceiling = 1.2% of bankroll ($10 at $850). Flat clips above ~1.2% of bankroll are ruin-grade: $50 flat at $850 goes to ZERO in the May+June replay (docs/drawdown-sizing-2026-07.md) |
| Automation | May only reduce size, never increase; the night_scale streak-scaler pattern is banned |
| Kill criteria | Parity breach or feed breach only, NEVER P&L drawdown (circuit breakers research-rejected; drawdown protection = sizing) |
| Discretion | No discretionary/manual trades on the strategy wallet |
| Deployment gate | No prod config change without all-window backtest evidence + committed code + 48h paper parity soak |
| Re-arm gates | (a) 7 PASS days on the soak scorecard (paper_soak_report.py), then (b) 14 micro-live days at 1% clips with realization ratio >= 0.85 vs same-day harness replay before any size increase. Break-even is at 0.82 realization; below that the edge nets negative at any sizing |

## 6. Validation windows

| Window | Dates | Use |
|---|---|---|
| TUNE | 2026-02-12 to 2026-04-30 | Selection only |
| VERIFY | 2026-05-07 to 2026-05-18 | Confirm frozen config |
| HOLDOUT | 2026-05-19 to 2026-05-28 | One-shot |
| June 2026 | BURNED | Selection leaked into it (regime-gate episode); measure only |
| July 2026 | SEALED | The new holdout; never select on July |

## 7. Deploy (Dublin)

| Item | Value |
|---|---|
| Box | `i-0e1d441131c50103c`, eu-west-1, currently stopped |
| IP | Changes each start; `export SHADOW_SSH_HOST=<new-ip>` |
| Deploy | Git-backed (clone/pull + `cargo build --release -p pm-app`); rsync is deprecated |
| Warmup | vol3600 buffer needs ~1h after restart before entries resume |
| Account | ~$850 as of 2026-07-01 |

History: `docs/postmortem-2026-06-16-fade-live-divergence.md` (why live must
share the shadow decision path) and the June deploy-cycle forensics in project
memory (`june-deploy-cycle-postmortem.md`) covering the regime-gate episode.
