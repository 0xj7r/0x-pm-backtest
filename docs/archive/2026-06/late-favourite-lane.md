# Late-favourite expiry lane (entry window study)

Feature: `enter_within_close_s` (CLI `--enter-within-close-s`, default 0 = off).
Entries permitted only in `[close - enter_within_close_s, close - stop_before_close_s]`.
Parity: default 0 reproduces the exit-30 champion golden trades byte-identical
(2,090 trades, $20,338.82 on the common market set vs `data/runs/alpha/exitsweep/ex30.json`).

All cells: May 7-18 tune window ONLY, canonical manifests, `--aligned-mode
--exit-after-s 0` (hold to expiry), $50 clips, latency 150ms, vol 3600s.
Hit minus breakeven (avg entry price) is the verdict metric: the lane risks
~entry to win ~(1-entry), so hit must exceed avg px.

## BTC-5m matrix

| Cell | thr/ew/stop/mid | trades (t/d) | P&L | per-trade | hit | avg px | hit-BE | Sharpe(d) | worst day | neg days |
|---|---|---|---|---|---|---|---|---|---|---|
| A1 | -1.0/60/5/.85 | 2,478 (207) | $638 | $0.26 | 94.8% | 0.944 | +0.4pp | 0.36 | -$304 | 4/12 |
| A2 | -1.0/60/10/.85 | 2,308 (192) | $735 | $0.32 | 94.9% | 0.943 | +0.5pp | 0.47 | -$244 | 3/12 |
| A3 | -1.0/120/5/.85 | 2,843 (237) | $1,687 | $0.59 | 93.4% | 0.923 | +1.1pp | 0.62 | -$406 | 1/12 |
| A4 | -1.0/120/10/.85 | 2,701 (225) | $1,744 | $0.65 | 93.3% | 0.922 | +1.2pp | 0.70 | -$339 | 2/12 |
| B1 | 0.02/60/5/.85 | 1,432 (119) | $371 | $0.26 | 92.4% | 0.919 | +0.5pp | 0.21 | -$236 | 4/12 |
| B2 | 0.05/60/5/.85 | 860 (72) | $273 | $0.32 | 90.7% | 0.901 | +0.6pp | 0.15 | -$308 | 5/12 |
| B3 | 0.02/120/5/.85 | 2,110 (176) | $1,501 | $0.71 | 92.5% | 0.912 | +1.3pp | 0.73 | -$212 | 3/12 |
| B4 | 0.05/120/5/.85 | 1,334 (111) | $1,242 | $0.93 | 91.2% | 0.896 | +1.6pp | 0.52 | -$328 | 3/12 |
| C | B3 + perp@0.5 | 2,138 (178) | $1,945 | $0.91 | 92.8% | 0.911 | +1.6pp | 0.87 | -$216 | 2/12 |
| E090 | B3 @ mid 0.90 | 1,870 (156) | $1,243 | $0.66 | 94.9% | 0.937 | +1.2pp | 0.90 | -$96 | 2/12 |
| E093 | B3 @ mid 0.93 | 1,640 (137) | $977 | $0.60 | 96.4% | 0.953 | +1.1pp | 0.68 | -$172 | 3/12 |
| D ctrl | -1.0/600/120/.85 | 1,613 (134) | $620 | $0.38 | 88.5% | 0.879 | +0.7pp | 0.27 | -$185 | 5/12 |

Timing story partially confirmed: the late lane is clearly better per trade and
per day, but the early band (control D) is mildly positive at our 150ms taker
fills, not the whale loss pocket. 120s windows strictly dominate 60s.

Tail accounting: every loss is the full -$50 clip (mean loss exactly -$50.00,
binary hold-to-expiry); one loss erases ~11-13 wins at px 0.92, ~20 at 0.953.
Max losses in a day: 19 (B3/C), 12 (E090); worst 20-trade run -$240; max
consecutive losses 2 in-window. May 15 (crossed-mid tail day) is the lane's
drawdown day in every cell.

Verdict (BTC): satellite lane only. C and E090 advance-worthy on margin
(+1.2 to +1.6pp, 9-10/12 green days) but economics thin (best ~$162/day per
$50 clip, Sharpe(d) <= 0.90 vs champion 2.61). Parked pending live infra;
no test-window look spent.

## Cross-asset transfer (SOL-5m, XRP-5m)

Coverage caveat: tick caches cover only ~67% (SOL: 2,313/3,456 run) and ~64%
(XRP: 2,204/3,456 run) of the window; perp tapes loaded fully (SOLUSDT 3.4M,
XRPUSDT 2.7M prints, 12/12 days).

| Cell | trades (t/d) | P&L | per-trade | hit | avg px | hit-BE | Sharpe(d) | worst day | neg days |
|---|---|---|---|---|---|---|---|---|---|
| SOL C | 391 (33) | -$1 | -$0.00 | 93.6% | 0.935 | +0.1pp | -0.00 | -$200 | 7/12 |
| SOL E090 | 294 (25) | -$139 | -$0.47 | 94.6% | 0.954 | -0.9pp | -0.19 | -$146 | 7/12 |
| SOL A4 | 1,546 (129) | -$204 | -$0.13 | 95.7% | 0.959 | -0.2pp | -0.13 | -$285 | 8/12 |
| XRP C | 773 (64) | -$294 | -$0.38 | 93.3% | 0.940 | -0.7pp | -0.23 | -$207 | 6/12 |
| XRP E090 | 645 (54) | $31 | $0.05 | 95.7% | 0.956 | +0.1pp | 0.03 | -$144 | 5/12 |
| XRP A4 | 1,673 (139) | -$904 | -$0.54 | 94.1% | 0.952 | -1.0pp | -0.41 | -$405 | 7/12 |

Verdict (cross-asset): the edge does NOT transfer. All six cells sit at
-1.0pp to +0.1pp vs breakeven (BTC: +1.2 to +1.6pp), 5-8/12 negative days.
Alt books price late favourites tighter (avg px 0.935-0.959 vs BTC 0.91),
leaving no margin, and the belief-gated cells barely trade (SOL C: 33/day vs
BTC 178/day). The lane is BTC-specific book behaviour, not generic venue
microstructure, subject to the 64-67% coverage caveat. Robustness check
(model-free A4) is negative on both assets, so this is not a belief artifact.

## Maker-entry study (REJECTED)

Feature: `--maker-entry-offset` (default -1 = off = parity). When >= 0 an
entry signal rests a bid at (side ask - offset) instead of taking; the order
fills only if the side ask later trades at-or-below the level before the
stop-before-close deadline (conservative crossed-through primitive), pays
zero fee, holds to resolution; one resting order per market. Parity verified:
defaults reproduce cell C byte-for-byte (2,138 trades / $1,945.19 / 92.8%)
and the fee-true sigma>=4 champion ($1,513.56 net).

Cells (May 7-18 tune window, lane C config + `--fee-curve-rate 0.07`, $50
clips; taker pays the one entry leg, maker pays zero). Offline sigma>=4bps
gate from the trades dump (`scripts/maker_entry_analyze.py`).

| Cell | fill rate | net P&L (/d) | hit | avg px | hit-BE | Sharpe(d) | neg days | sigma>=4 net (/d) |
|---|---|---|---|---|---|---|---|---|
| taker ctrl | - | +$1,281 (+$107) | 92.8% | 0.911 | +1.6pp | 0.57 | 3/12 | +$1,514 (+$126) |
| maker 0.01 | 64.4% | -$304 (-$25) | 89.0% | 0.894 | -0.4pp | -0.16 | 7/12 | +$257 (+$21) |
| maker 0.02 | 55.8% | -$562 (-$47) | 87.3% | 0.881 | -0.8pp | -0.30 | 10/12 | +$86 (+$7) |
| maker 0.03 | 49.2% | -$868 (-$72) | 85.6% | 0.870 | -1.4pp | -0.46 | 10/12 | -$117 (-$10) |

Adverse selection is the whole story, and it is fatal. Splitting the taker
control by would-the-bid-have-filled (offset 0.01): the no-fill 36% of
signals hit 99.6% (+$3,038, 0/12 negative days, Sharpe(d) 4.1); the
would-fill 64% hit 88.97% and LOSE -$1,757 even at taker prices. The
favourite's ask trading down through the bid level in the final 120s is the
flip starting, not benign de-risking: the counterparty hypothesis is
rejected. The maker improvement per filled trade is real (+1.33c price +
$0.33 fee saved, +$1,452 vs taker on the same signals) but it claws back
barely half of the selection damage, and the fill mechanism structurally
excludes exactly the no-wobble winners that carry all of the lane's profit.
Deeper offsets make every number worse (more selection, fewer fills).

Verdict: do NOT advance to OOS. Maker entry inverts this lane's edge; the
lane's margin lives in the favourites nobody sells down, which a resting bid
can never buy. Taker entry stays the champion execution.
