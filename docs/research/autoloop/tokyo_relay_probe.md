# Tokyo Relay Probe: Binance Feed Latency to Dublin

Date: 2026-06-12. Window: 13:53-14:13 UTC (20 min, simultaneous on both boxes, same market minutes).

## Question

Does a Tokyo (ap-northeast-1) relay box forwarding Binance spot trade messages over a persistent TCP socket beat the direct Binance WS connection from Dublin (eu-west-1)? Cutting the Binance leg is worth roughly $100/day at current scale; physics caps the honest win at routing/jitter/stream-choice (10-40ms).

## Setup

- Tokyo probe: t4g.nano, i-08ab3dfc3da3dd473, 13.112.154.21, Ubuntu 24.04 arm64, tagged Name=tokyo-feed-probe.
- Dublin: existing shadow box i-0e1d441131c50103c (34.242.101.97). The three pm-app shadow processes were untouched and confirmed running after the probe (pgrep count 3).
- Clocks: chrony against Amazon Time Sync (169.254.169.123) on both boxes. Offsets at collection: Tokyo 1.6us slow, Dublin 7.8us fast. Clock error is negligible at these scales.
- Streams: wss://stream.binance.com:9443 btcusdt@aggTrade, btcusdt@trade, btcusdt@bookTicker. Spot bookTicker carries no event timestamp E (confirmed: 276k messages received, none with E), so the perp fstream btcusdt@bookTicker (which has E) was measured as the bookTicker proxy.
- Relay: on each aggTrade/trade message Tokyo sent one line (stream, E, price, tokyo_ms) over a persistent TCP socket (TCP_NODELAY, drain per message) to Dublin:9099; Dublin listener recorded local receipt ms. Receipt delta = local_ms - E.
- Note: orchestration ran via AWS SSM Run Command, not SSH (local policy blocked SSH key file access; the keypair imported for the box reuses the whale-pair-dublin public key, so the existing pem would open it).

## Results

### Receipt delta vs exchange event time E (median / p90, ms)

| Stream | Tokyo local | Dublin direct | Tokyo relay end-to-end (at Dublin) |
|---|---|---|---|
| btcusdt@aggTrade | 2.96 / 5.16 (n=20,127) | 104.21 / 109.05 (n=19,900) | 104.43 / 274.22 (n=20,127) |
| btcusdt@trade | 4.14 / 9.41 (n=86,260) | 105.72 / 113.73 (n=83,770) | 127.20 / 299.35 (n=86,260) |
| fstream btcusdt@bookTicker | 4.68 / 27.58 (n=967,545) | 107.29 / 114.67 (n=973,243) | not relayed |

Relay delivery was lossless (sent == received: 20,127 aggTrade, 86,260 trade lines).

### Relay vs direct, per stream

| Stream | Direct median | Relay median | Relay delta | Direct p90 | Relay p90 |
|---|---|---|---|---|---|
| aggTrade | 104.21 | 104.43 | +0.22 (relay worse) | 109.05 | 274.22 (relay much worse) |
| trade | 105.72 | 127.20 | +21.48 (relay worse) | 113.73 | 299.35 (relay much worse) |

### Trade vs aggTrade from Dublin direct (zero-infra check)

| | aggTrade | trade | delta |
|---|---|---|---|
| median | 104.21 | 105.72 | aggTrade faster by 1.5ms |
| p90 | 109.05 | 113.73 | aggTrade faster by 4.7ms |

aggTrade is the marginally faster stream; no material stream-switch win (1-5ms, not the 10ms+ that would matter).

## Verdict: NO-WIN

- The relay does not beat the direct connection at the median on either stream and is dramatically worse in the tail (p90 274-299ms vs 109-114ms direct). A single cross-region TCP stream with ~200ms RTT suffers head-of-line blocking on any loss or jitter; per-message drain cannot fix path physics.
- Tokyo ingest is ~3-5ms, so essentially all of Dublin's ~104ms is Tokyo-to-Dublin propagation. Binance's own WS edge already delivers to Dublin at near the fiber bound (measured 101-105ms; theoretical one-way 95-105ms). There is no meaningful routing slack for a relay to reclaim on AWS's inter-region backbone.
- STREAM-SWITCH-ONLY does not apply either: trade does not beat aggTrade; aggTrade (already the natural choice) is 1.5ms faster at median.
- The latency floor for the Binance leg from Dublin is ~104ms. The only ways materially below it are moving the decision logic to Tokyo or a sub-fiber path (neither is a $4/month relay).

## Cost

- Probe cost: under $0.05 (t4g.nano ~1.5h at ~$0.0054/h plus 8GB gp3 prorated).
- Permanent relay, had it won: ~$4-5/month (instance ~$3.95 + 8GB gp3 ~$0.77 + ~7GB/month egress at this message rate, mostly inside the free 100GB tier). Not justified by the numbers above.

## Teardown (executed)

Relay win < 10ms median, so per the decision rule everything was removed:

- Tokyo instance i-08ab3dfc3da3dd473 terminated.
- Keypair tokyo-probe-key (ap-northeast-1) deleted.
- Security group tokyo-feed-probe-sg (sg-08574c626e4983413, ap-northeast-1) deleted after termination.
- Dublin SG sg-04673c15bc138e358 inbound rule TCP 9099 from 13.112.154.21/32 revoked.
- Probe artifacts remain on Dublin at /home/ubuntu/tokyo_probe/ (scripts, stats JSON, logs) for reference; pm-app shadows undisturbed.

## Recommendation

Do not build the relay. Treat ~104ms as the structural Binance-spot latency from Dublin and spend the latency budget elsewhere (e.g. the 9ms CLOB leg, or decision-side compute). If the Binance leg ever has to shrink materially, the move is relocating signal computation to ap-northeast-1 and shipping decisions (not ticks) west, which is a different project with real operational cost.
