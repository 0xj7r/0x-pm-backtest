# The deployed config was backtest-negative: min_entry_ask is the culprit (2026-07-09)

Triggered by the soak week showing consistent live losses. Before concluding a
realization failure, checked whether the config LIVE runs is even
backtest-profitable. It is not, and one gate is responsible.

## The finding (June 13-30, truthful 1250ms latency, fee-net, $50 clips)

| config | n | NET | hit |
|---|---|---|---|
| bare (core frozen only) | 2587 | **+$1,957** | 62% |
| + min_entry_ask 0.45 (alone) | 2177 | **-$149** | 68% |
| + open_fav gate (alone) | 2582 | +$1,591 | 62% |
| + skip_spot_misalign 30 (alone) | 2297 | +$1,653 | 64% |
| DEPLOYED (all three) | 1990 | **-$459** | 68% |
| RECOMMENDED (open_fav + misalign, NO min_entry_ask) | 2290 | **+$1,297** | 64% |

`min_entry_ask 0.45` alone turns +$1,957 into -$149 (a -$2,106 swing). The
other two gates cost little (-$366, -$304). Dropping min_entry_ask recovers
the config to +$1,297 while keeping the other two gates' drawdown protection.

## Why min_entry_ask destroys the edge

It blocks entries where the ask is below $0.45, i.e. buying the CHEAP UNDERDOG
- the side the market prices as unlikely (0.30-0.45) when the model says it is
actually more likely. Those are the highest-payoff fades: buy at 0.30, resolve
to $1 = +233%. They are the profit engine, and the gate discards them.

## The governance failure

min_entry_ask was added as part of a "mayjune drawdown failure mode fix" and
was NEVER in the TUNE validation (all TUNE/latency runs were bare config or
bare + v1 stability gate; none included the base gates). So the config we
deployed and soaked was never validated, and it loses in its own backtest.
This is the THIRD config-consistency failure found 2026-07-08/09 (after M-1
stop_before_close and the daily-replay base-gates mismatch): each made us
measure or deploy something other than what we validated.

## The unresolved crux (the realization question, concentrated)

min_entry_ask blocks the cheap-underdog fades. It was added because the theory
was those trades LOSE LIVE (realization gap) even though they win in backtest.
We have ZERO live data on them: both live streams (shadow-final, fast_live)
gate them out. So "should we drop min_entry_ask" cannot be answered from
current data:
- cheap fades realize live -> drop the gate, recover ~+$1,297 config. Big.
- cheap fades do not realize -> the gate was right; true capturable edge is
  thinner; closer to the sober scenario.

## The experiment

Soak a parallel stream WITHOUT min_entry_ask (min_entry_ask=0) so the
cheap-underdog entries run live. When July replay data publishes (~Jul 11),
compare their live outcomes to the matched-config replay. That single
measurement decides whether the recovered edge is real. Until then, do not
deploy either config; the edge is real in backtest but its realization on the
exact trades in question is unmeasured.

## Caveat

Verified on June only (May/Feb book data not cached locally today). The
min_entry_ask effect (blocking the cheap-underdog payoff tail) is structural
and should generalize, but confirm on May + the TUNE months when data allows.
