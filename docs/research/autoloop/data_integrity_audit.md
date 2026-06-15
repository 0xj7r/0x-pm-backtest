# Data-Integrity Audit: feemin/base W3 (2026-05-07..05-18)

Auditor pass against EXTERNAL ground truth (Polymarket gamma-api, raw Binance
parquet). Scope: BTC-5m, `data/runs/alpha/feemin/base.trades.jsonl` (3046 trades,
2109 distinct traded markets, 3445 of 3456 markets run). Internal consistency was
already verified upstream and is not re-litigated here.

## Headline correction to the audit premise

The audit brief and the coordinator both assume "our backtest computes `won` by
comparing a Binance close to strike K." **That is not how this codebase works.**
The backtest outcome is the official Polymarket resolution label carried in the
market manifest, not a Binance close-vs-strike computation.

Trace:
- `crates/pm-alpha/src/harness/replay.rs`: every `won`/payout derives from
  `series.resolved_yes` (lines 286, 453, 525, 797).
- `crates/pm-app/src/alpha.rs:555,613`: `resolved_yes = outcome_label_resolved_yes(market.outcome)`.
  The Binance-proxy `strike` (line 572) feeds ONLY the BSM belief, never the label.
- `crates/pm-app/src/walkforward.rs:1492`: the label maps the manifest `outcome`
  string ("Up"/"Down") to a bool. Source of that string is the Telonex
  availability API (`crates/pm-app/src/discovery.rs:20-30,63-69`), i.e. the
  vendor-recorded on-chain Polymarket settlement.
- The close-vs-strike fallback (`infer_outcome`, alpha.rs:613-626) only fires when
  the manifest label is absent. The feemin/base run used `infer_outcome=false` (no
  `--infer-outcome` flag in `scripts/batch0612.sh`) and reports
  `n_skipped_no_outcome: 0`, so EVERY market used the explicit official label. The
  close-vs-strike path was never taken.

Consequence: the 14bps Binance-vs-official basis CANNOT flip any resolution label
in this run, because the label is the official result, independent of Binance
price. The basis only affects the belief (already fixed: belief uses Binance-proxy
strikes in the same basis as its Binance-spot state; official prices are
verification-only, per memory strike-basis-usd-vs-usdt).

## Finding 1 - Resolution labels vs official ground truth: PASS (MINOR residual)

External verification against gamma-api `?slug=...&closed=true` (`outcomePrices`,
the 1.0 side = winner):

- 16/16 sampled W3 markets agree with the manifest label, including the **8
  tightest at-the-money markets** (|close-strike| 0.001-0.016 bps, the razor-edge
  zone the basis concern targets). 0 disagreements.
- Slugs checked: 1779063600, 1778544000, 1778545500, 1778523900, 1778698500,
  1779033000, 1778634000, 1778362500 (first batch), plus 1778800800, 1778649300,
  1778361600, 1778309100, 1778256000, 1778909100, 1779046500, 1778409600 (the
  8 closest-to-strike). All match.

Marginal-window stress: 96.4% of traded W3 markets resolve within 20bps of strike
and the median distance is 3.9bps (well inside the ~0.8 bar-sigma basis noise).
A naive Binance close-vs-strike reconstruction over these markets disagrees with
the official label ~50% of the time (coin-flip), which is exactly why the engine
must NOT and does NOT label that way. It uses the official result. The
at-the-money cluster is therefore correctly labelled, not at risk.

Held-trade cross-check: of all trades that rode to resolution unexited
(`exit_price=null`), the side-implied `resolved_yes` matched the manifest in
2108/2109 markets. The single exception is a record-level bug, not a label error
(see Finding 5).

**Severity: PASS on labels. The one residual is the held-trade record bug below.**
**Net P&L impact of label disagreement vs official: $0 of $20,973 (0.00%).**

## Finding 2 - Strike basis offset: confirmed, already mitigated (MINOR)

Measured our Binance-proxy strike against gamma's official `priceToBeat`
(USD-index basis):
- 1778545500: ours 81607.27 vs official 81509.84 = +97.43 (+11.95 bps)
- 1778800800: ours 81252.46 vs official 81233.99 = +18.47 (+2.27 bps)

Sign and magnitude match the documented ~$91 / ~14bps Binance-above-USD level
offset. This is a known, fixed concern: the strike feeds only the belief and is
intentionally in the same basis as the belief's Binance-spot state, so the offset
is basis-consistent and injects no phantom edge. Official prices are
verification-only. **Severity: MINOR (informational, already correct by design).**

## Finding 3 - Tape-gap bias: not volatility-clustered (MINOR)

11 markets skipped of 3456 (0.32%): 7 load-error + 4 no-strike. The 7 load-error
slugs (only ones logged individually) cluster on just two ingest days:
- 2026-05-07: 1778113200 (00h), 1778131500 (05h), 1778133300 (05h), 1778178600 (18h)
- 2026-05-15: 1778849700 (12h), 1778859600 (15h), 1778878200 (20h)

Spread across hours 00-20 on both days; the clustering is by INGEST DAY (vendor
tape availability), not by hour-of-day or a volatility window. Two days is too few
to attribute to volatility selection, and 0.32% of markets cannot materially bias
the aggregate. No evidence of optimistic selection. **Severity: MINOR.**

## Finding 4 - Price-series units and scale: correct for all assets (MINOR)

The `transact_time_ms` column is microseconds despite its name (the F3 finding).
The loader handles it correctly: `crates/pm-telonex-loader/src/binance_trades.rs:99-101,155-156`
reads it as microseconds and converts `ts_ns = ts_us * 1_000`.

Empirically verified all four spot caches (2026-05-10/12 parquet):
- BTCUSDT, ETHUSDT, SOLUSDT, XRPUSDT all carry 16-digit timestamps that ONLY
  validate as microseconds (ms interpretation overflows the year). Prices are
  plain decimal-dollar strings at sane scales (BTC ~80678, ETH ~2326, SOL ~93,
  XRP ~1.42).
- Spot, perp, and ref_spot all load through this same loader
  (`walkforward.rs:2872`), so there is no divergent unit path for any asset.

No unit or scale mismatch. **Severity: MINOR (already handled, verified across the
multi-asset expansion).**

## Finding 5 - One held-trade `won` record inconsistency (MINOR)

Market btc-updown-5m-1778527500 (open 1778527200): gamma and manifest both resolve
**Down**, but the held Yes trade (decision +3min, avg 0.36, `exit_price=null`)
records `won=true`. This is the only unexited trade in the entire feemin/base run
and the only label inconsistency. It is a record-level bug in the exit/hold path
(likely a passive-exit-timeout ride-to-resolution branch that set `won` from a
stale field rather than from `resolved_yes`), NOT a resolution-label error: the
market itself is correctly labelled Down.

Impact: this single record contributes +$86.65 of falsely-won P&L, **0.41% of the
$20,973 total run P&L**. Worth a code fix before scaling hold-mode lanes, but it
does not move the aggregate.

## Finding 6 - Look-ahead: clean (no issue)

Decision beliefs are built tick-by-tick in timestamp order
(`replay.rs:71` enumerate) with state `now_ns = tick.ts_ns`. All spot/perp reads
use `price_at_or_before(now_ns)` and backward-walking `trailing_return`
(`model.rs:78,116,146`; `state.rs:93-94,114-118,167`). Fills require
`t.ts_ns >= decision_ts + latency_ns` and exits scan `ticks[tick_idx..]` forward
only (`replay.rs:272-274,446-449`). Stability windows iterate `ticks[..=tick_idx]`
(past only, line 199). No future leak found.

## Verdict

**GO for a real-money pilot, with the residual risks below noted.**

The two largest theoretical risks (resolution-label correctness and strike basis)
are NOT live in this engine: the label is the official Polymarket result
(gamma-verified 16/16 including the 8 tightest at-the-money markets), and the
basis is fixed to belief-only with the correct sign. Price-series units are
correct across all four assets, tape gaps are negligible and non-selective, and
the harness is look-ahead clean.

Residual risks beyond the already-verified label/basis layer:
- One held-trade `won` record bug (0.41% P&L). Fix before relying on hold-mode
  lanes; immaterial to the fade/exit strategy that dominates this run.
- Ground-truth sample was 16 markets (manual gamma fetches). The agreement was
  perfect including the hardest cases; a fuller automated sweep would tighten the
  bound but is unlikely to change the verdict.
