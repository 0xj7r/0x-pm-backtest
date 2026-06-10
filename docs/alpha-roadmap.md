# pm-alpha Execution Roadmap (2026-06-10)

Status base: BTC-5m fade validated Feb-June (hardened June holdout +$11,912; capture-stress floor +$4.5k/wk at 10% depth). BTC-15m promising. ETH/SOL/XRP rejected as-is (feed-leadership finding). Champion config: vol3600, thr 0.16, exit_after_s 30, 150ms, real-NO, canonical labels.

Each phase is a self-contained subagent-sized task with its own verification. Protocol invariants for every phase: tune/test splits only, June + any new holdout untouched by selection, config-count disclosure, determinism checks on harness changes, honest rejection is a valid outcome.

## Phase A — close out the fade (mostly running)
A1. [running] Hardened Feb-Apr rerun (canonical labels, real NO, fixed strike, tick cache). Output: exact Feb-Apr number + localized tick caches. Then: re-mine regime cells + daily series; update alpha-hunt-002 docs.
A2. [running] Official-strike backfill (polymarket.com crypto-price API, 45k btc-5m windows, resumable). Then: add `--strikes <jsonl>` flag to pm-app alpha (override proxy); re-run June once; expect ~no change (proxy validated) — retires the proxy caveat.
A3. BTC-15m scale-up: tune May 1-18 / test May 19-28 / June check via canonical manifest (books on S3 for 15m; local cache has May 21-28 only). Verdict gates adding the second deployment cell.
A4. NO-staleness sensitivity: June run was not yet re-validated after the 30s cutoff + cache bump — run once, record delta (expected ~0).

## Phase B — directional model (the big build)
B1. Rust perp plumbing: PerpState { trades: SpotHistory, oi: Vec<(ts_ns, f64)>, funding: Vec<(ts_ns, f64)> } in pm-alpha::state; loaders in pm-telonex-loader/pm-app: futures_agg_trades reuses the binance trades loader (channel param), metrics/funding need small readers (schemas: metrics create_time "YYYY-MM-DD HH:MM:SS" strings, 5-min cadence; funding calc_time ms epoch, last_funding_rate f64). ExoState gains `perp: Option<&PerpState>` (update all constructors/tests).
B2. Directional feature vector (exogenous, computed at decision time): funding level/percentile, OI delta 5m/30m, perp-spot basis, perp flow imbalance + large-print bursts, liquidation proxy (perp print burst + OI drop), trend ladder (port spot_score_stack 10s-4h from pm-model lib.rs:2713 with alignment/consistency), vol expansion-vs-contraction, BTC-lead features (for alts).
B3. Naive-alignment baseline: mine Feb-Apr hardened trade dumps + a dedicated aligned-mode run (EntryMode::Aligned exists) on Feb-Apr tune cohort. Every signal must show lift over this.
B4. Supervised combiner: boosted-stump machinery (calibrator.rs pattern) on B2 features, label = continuation (side-of-move wins window), trained on Feb-Mar trending cohorts, tested Apr, evaluated as P(continuation) belief driving Aligned entries + tail hedge (--tail-max-price/--tail-frac exist). Greedy per-family adds.
B5. Regime classifier v2: v1 calls 80% of tape expanded_mixed (113 clean_directional markets in 78 days — useless for routing). Recalibrate thresholds on vol/efficiency distributions (quantile-based), validate labels against per-regime P&L separation, THEN build the router: fade capital in expanded cells, directional in trending cells, stand down elsewhere.

## Phase C — alt markets revisit (after B)
C1. SOL/XRP with perp-feed belief input (their books beat Binance spot; perps may lead their books). Fetch SOL/XRP perp data (same fetcher).
C2. The inverse hypothesis on alts (follow their book early move = alignment) as a B4 special case.
C3. DOGE/HYPE cells (canonical manifests exist; need spot + book coverage check).

## Phase D — execution & deployment (gates live)
D1. Pair-cost window study: book_snapshot_full for targeted windows; measure sub-$1 window duration, depth, two-leg latency feasibility. Output: pair-arb viability verdict + maker queue groundwork.
D2. Chainlink crypto_prices ingestion (needs telonex API key — ask user or find in s3 ingest script); oracle-lag quantification per token; exact-oracle strikes from Apr 2.
D3. Shadow mode: agent computes pm-alpha belief + logs would-be entries/exits live (zero capital). Measures: signal frequency vs backtest, quote existence at decision time, live WS vs tape timestamp deltas.
D4. Micro-pilot: $10-25 clips, 2-concurrency, hard daily stop, kill-switch. Measures realized capture fraction vs sim. Pre-agreed scale-up bar (e.g. realized >= 50% of sim per-trade edge).
D5. Engine integration: agent consumes pm-alpha (SSOT endgame), portfolio guards (daily loss cap, exposure caps, loss-streak cooldown — port from br2 harness), $1K sizing ($25 clips, 3-concurrent cap).

## Phase E — infrastructure (parallel, opportunistic)
E1. EC2 co-location runs for any new full-history sweep (existing launcher; write tick caches there, sync back).
E2. Oct-Dec 2025 history extension (canonical manifests cover it; books on S3) once tick-cache + EC2 path is smooth.
E3. close_ts->open_ts rename (audit latent trap), duration-from-slug hardening, tick-cache key including down-map hash.

## Deployment naming convention (2026-06-10)
New stack units are `pm-alpha-*`: `pm-alpha-shadow.service` (log-only), future `pm-alpha-live.service` (pilot). Logs under `~/data/pm-alpha/<mode>/`. Legacy `polymarket-exec@*` instances stay MASKED (strongest off state; unit files kept for the execution-adapter reference); `mm-live-logger` and `poly-daily-summary` stopped/disabled 2026-06-10. Kill-switch file restored at ~/.config/polymarket-exec/live.kill as a belt-and-braces layer. The pm-alpha units read nothing from ~/.config/polymarket-exec/.

## Sizing (user direction, 2026-06-10 evening)
Flat-$ clips mis-size the cheap-entry cohort (live shadow: five 0.04-0.17 entries all lost full premium at $50 flat). Next harness knob after the deadline verdict: fractional-Kelly sizing on a belief-reliability-discounted edge (shrink p toward price where the belief is least calibrated: tau small, extreme moneyness), capped at the flat clip. Tune {deadline} x {flat, kelly-frac} jointly on May 7-18, verify once on May 19-28, judge on daily Sharpe + worst day. Kelly subtlety recorded: naive Kelly OVER-allocates to cheap asymmetric entries; the discount is the point.
