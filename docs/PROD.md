# PROD: BTC-5m Exogenous Fade (canonical reference)

Last updated: 2026-07-01. This is the single source of truth for what runs in
production. If another doc disagrees with this one, this one wins.

## 1. The strategy

Exogenous BSM fade on Polymarket BTC-5m up/down markets. A Black-Scholes-style
belief is computed from CEX spot + perp state (perp weight 0.75, realized vol
over 3600s). When the Polymarket book disagrees with the belief by more than the
edge threshold, buy the cheap side and hold to redemption. The edge is staleness
capture: CEX state updates faster than the PM book reprices (~150-250ms), so we
monetize episodic dislocations, not superior calibration.

## 2. Frozen prod config

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
| `stop_before_close_s` | 90 |
| `skip_saturday` | true |
| `min_entry_ask` | 0.45 |
| `skip_open_fav_gap` | true (p_min 0.88, ask_max 0.62, secs 300) |
| `skip_spot_misalign_s` | 30 |
| `min_marginal_edge` | 0.04 |
| Regime gates (`skip_calm`, `skip_expanded_mixed`, ...) | **NONE** (removed 2026-07-01, overfit) |

Flags SSOT: `scripts/ops/shadow_final_gated_flags.sh`. Restart script:
`scripts/ops/shadow_final_restart_gated.sh`. Assembled command line:

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
  --out-dir ~/data/pm-alpha/shadow-final \
  --skip-spot-misalign-s 30 \
  --min-entry-ask 0.45 \
  --skip-open-fav-gap \
  --open-fav-p-min 0.88 \
  --open-fav-ask-max 0.62 \
  --open-fav-secs 300
```

Regime gates history: `skip_calm` + `skip_expanded_mixed` were selected on Jun
14-19 live tape only; out-of-sample Jun 20-30 they blocked 99% of entries (10
entries in 10 days, all clean_directional) while the ungated stream was green
every June day. Removed from prod 2026-07-01.

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
| Sizing | Clip = 1% of current bankroll via `PM_SHADOW_CLIP_FRAC`; env clip is a hard ceiling |
| Automation | May only reduce size, never increase; the night_scale streak-scaler pattern is banned |
| Kill criteria | Parity breach or feed breach only, NEVER P&L drawdown (circuit breakers research-rejected; drawdown protection = sizing) |
| Discretion | No discretionary/manual trades on the strategy wallet |
| Deployment gate | No prod config change without all-window backtest evidence + committed code + 48h paper parity soak |

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
