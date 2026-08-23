# Root-cause track: closing the live-vs-replay gap at the source

**Date:** 2026-07-04
**Trigger:** first soak realization rows (Jul 2: 0.32, Jul 3: -0.21) with
live-vs-replay side agreement ~48% while TWIN agreement was 100%. The
divergence is between live-feeds-as-a-class and the recorded archive, not
between live observers. Three probes ran today to localize it.

## Probe results

**B. Decision-latency decay (offline, chop days Jun 16-18):**

| decision cadence | 3-day NET |
|---|---|
| 500ms | +$4,220 |
| 1000ms (prod) | +$4,239 |
| 2000ms | +$3,087 (-27%) |
| 3000ms | +$569 (-87%) |

The replay number is STABLE at <=1s sampling (not sampling luck) but the edge
has a ~1-2 SECOND HALF-LIFE on chop days. Live's effective reaction chain
(receipt delay ~100ms + 1s timer phase avg ~500ms + submit ~150-250ms +
engine queueing) puts live roughly 0.5-1s deeper into that decay curve than
the replay's model (exchange-time alignment + 250ms fill latency). This is
currently the best single explanation of the realization gap, and it
coheres with ce25's behavior: median entry 152s into the window; the winner
does not play the first-second race at all, it harvests slower repricing.

**C. Archive fidelity (offline):** Telonex book snapshots are per-update
(median gap ~0ms, p99 130ms) and carry BOTH exchange and local-receipt
timestamps. Archive granularity is NOT the culprit. The relevant modeling gap
is CROSS-SOURCE CLOCK SKEW: replay aligns Binance and Polymarket events on
exchange time; live sees Binance ~100ms late relative to the PM book.

**A. Live capture (deployed today):** `live_collector` (the April build,
never deployed; ECS attempt was a placeholder) now runs on Dublin as
`pm-live-collector.service`, streaming raw market/spot frames with receipt
timestamps through the existing `pm-research-events-prod` Firehose into
`s3://pm-research-data-prod/processed/v=1/dt=.../`. Dublin's instance role
gained the minimal Firehose put policy. Zero-error startup verified.

## The program (in order)

1. **Measure the real reaction chain (1-2 days of collector data).** From
   captured frames + shadow decision logs: distribution of
   (event exchange-time) -> (Dublin receipt) -> (decision tick) ->
   (would_enter emit). Compare against the dt-decay curve to predict expected
   realization; if predicted ~0.3, the gap is fully explained by latency and
   the fixes are known (below). If not, diff feed CONTENT next (drop/coalesce
   rates vs archive for the same hours).
2. **Replay-on-captured-tape.** Run the harness on the collector's tape (the
   rust-replay/v=1 format was built for exactly this). If harness-on-live-
   tape reproduces live decisions, the engine is fully consistent and the
   archive-replay is simply an optimistic observer; backtest expectations
   then get restated against captured-tape replays.
3. **Latency fixes, cheapest first** (each through the deployment gate):
   a. Event-driven decisions: decide on book/spot events instead of the 1s
      timer (recovers ~500ms average phase; engine change, needs GATE B).
   b. Co-location: Dublin is eu-west-1; Polymarket CLOB and Binance both
      serve faster from other regions; measure receipt deltas from the
      collector before moving anything.
   c. Submit path: already ~150-250ms; limited headroom.
4. **Collector as the single feed (the architecture endgame):** engines
   consume the collector's stream instead of their own sockets; live ==
   recorded == replayable by construction. Only worth building after steps
   1-2 prove which layer pays.

## Strategic note

If step 1 confirms the latency explanation, there are two viable postures:
(i) invest in speed (fixes 3a-c) to capture the fast edge, or (ii) follow
the ce25 posture: concede the first-second race entirely (the stability gate
already points this way), trade the slower repricing edge, and let the
15m book (validated, slower by construction) carry more weight. These are
not exclusive; (ii) is deployable this month, (i) is engineering.

## Measurement result (2026-07-05, closes program step 1)

From the deciding engine's own per-minute telemetry (4,437 samples, July):

| leg | median | p90 |
|---|---|---|
| Binance event -> engine receipt | 100ms | 101ms (p99 103ms: rock-steady) |
| PM book event -> engine receipt | 9ms | 10ms |
| decision timer phase (1s cadence) | ~500ms mean | up to 1000ms |
| executor tail + submit (June calib) | ~200ms | ~250ms |
| venue match | ~75ms | ~100ms |
| **total reaction (entry)** | **~0.9s mean** | **~1.4s** |

Versus the replay's modeled 250ms, live runs ~0.65s (mean) to ~1.15s (p90)
further down the decay curve. Interpolating the dt-sweep (roughly -27% per
extra second in the 1-2s band), unmodeled delay alone predicts capturing
~70-85% of the fast component; measured realization (~0.3 on fast days) is
WORSE, because delay does not merely shrink the same trade: deciding on a
0.9s-older microstate near the strike flips the SIDE (the 48% live-vs-replay
agreement). Delay and instability compound; both point the same direction.

Conclusions:
1. **Network placement is fine.** 100ms Binance receipt (p99 103) and 9ms
   book receipt leave little to win from region moves, and moving would
   trade one leg against the other.
2. **The one real speed lever is the 1s decision timer** (mean 500ms of the
   chain): event-driven decide would roughly halve mean reaction. Engine
   change, GATE B required; queue only if we ever choose to chase the fast
   component.
3. **The primary posture stands: concede the race** (stability gate + 15m
   book), per ce25. The measured chain confirms we are structurally ~1s
   slow, and the profitable operators simply do not play that game.
4. Collector defect noted: the Binance spot tap degraded to a REST loop and
   there is no futures tap; fix before any feed-content diffing
   (polymarket-exec work, low priority while posture 3 holds).
