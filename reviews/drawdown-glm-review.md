# Adversarial review: drawdown / 5-share-floor analysis (2026-07-08)

Independent review of `scripts/research/drawdown_sim.py`, `docs/drawdown-handling-plan-2026-07.md`,
and `docs/realization-baseline-correction-2026-07.md`. The brief asks for verdicts, not code
changes. Verdicts used: **sound**, **flawed**, **overstated**.

## What I could and could not reproduce

The per-trade dumps the sim reads (`data/runs/tune_validation/*`, etc.) are gitignored and are
**not in this checkout**. Running `python3 scripts/research/drawdown_sim.py` exits 1 cleanly with
`no trade data found; run the TUNE/latency sweeps first` (no traceback, so it "runs clean", but it
produces no numbers). Consequence: **none of the headline figures in the plan (0/3000 ruin, the
+/- % drawdowns, the "$600: 2% touch zone" table, the +$670/day mean) are independently
reproducible from the committed repository.** That is itself a governance gap: a real-money
safety case resting on numbers that cannot be re-run from the artifact as committed.

Everything I claim below is verified one of two ways: (a) pure arithmetic from the code, or
(b) by importing the sim's own functions (`clip_for`, `run`) and exercising them directly. The
commands and outputs are in the appendix.

---

## 1. Simulation methodology

### 1a. Linear clip scaling `pnl(clip) = pnl_at_50 * clip/50` : SOUND (with one untested caveat)

The code is exactly linear: `drawdown_sim.py:92` does `E += haircut * x * clip / TELEMETRY_CLIP`
with `TELEMETRY_CLIP=50`. For $2 to $10 clips against a deep book, ignoring market impact is
defensible, and because clip only scales size (entry decisions are unchanged), scaling the daily
P&L sum linearly is reasonable.

The untested caveat: the claim assumes fill quality is size-invariant up to the $10 ceiling. The
sim never trades a clip larger than $10, so this is fine for the backtest itself, but it means the
realization haircut (which is measured on small clips) is being extrapolated to the $10 capped
clips that dominate once equity grows (see 4a). No evidence is offered that capture ratio is
constant across clip size.

### 1b. Monte Carlo shuffle (without replacement) : FLAWED, understates tail risk

The docstring admits the issue at `drawdown_sim.py:22`: resampling without replacement
"preserves the realized win/loss mix but destroys autocorrelation." That is not a footnote, it
is a disqualifier for a drawdown estimate. Permutation tests assume exchangeability; crypto
prediction-market returns exhibit regime persistence (volatility/drawdown clustering), and the
plan's own June experience was a multi-day bleed.

I demonstrated the distortion directly. With the sample's 28% red marginal over 60 days, the
longest red run under the exchangeable (shuffle) model is p50=3, **p95=4**, p99=6; under a
regime-sticky Markov model with identical marginals it is p50=5, **p95=9**, p99=11 (see appendix
B). The plan's stated "worst red streak 4 days" (`drawdown-handling-plan-2026-07.md:7`) is
literally the exchangeable p95. That is circular: the model is calibrated to the one sample it
sees, and by construction cannot emit a clustering regime worse than that sample. The p5/p95
troughs the plan reports are therefore calibrated to an IID world and **systematically understate
the clustered-regime tail that actually caused the June blowup**.

The "adversarial worst-8-first" path (`drawdown_sim.py:125-129`) is the plan's nod to this, but
it is then dismissed as "astronomically unlikely" (`drawdown-handling-plan-2026-07.md:26-27`).
That dismissal re-uses the same exchangeable assumption; under clustering, a long opening red run
is nothing like astronomically unlikely, so the adversary is being defined out of existence by
the very flaw it is meant to cover.

### 1c. Resampling 75 fixed days : OVERSTATED (survivorship / small-sample / wrong-baseline)

Every one of the 3000 shuffles is, by construction, 72% green with mean +$670/day, because the
shuffle reuses one realized 75-day path. The MC cannot generate a losing-regime sample that is
not already in those 75 days. So "p95 trough" is a statement about the ordering of these specific
favorable days, not about the forward distribution of days.

Worse, the daily P&L feeding the sim is **replay** P&L, not live P&L. The realization doc and the
team's own June postmortem show live going from $2,357 to $850 in the same window the replay
calls +$670/day. The only bridge from replay to live is the realization haircut, and that
haircut is estimated from essentially one normal day (Jul 2, ratio 0.75 against the
latency-matched replay; Jul 3 had live *beating* replay, so it does not even point the same way).
A $850 real-money safety case resting on replay P&L discounted by an n=2 haircut is not a safety
case, it is a replay of the same live/replay gap that already destroyed the account once.

---

## 2. The 5-share floor math

### 2a. `floor = 5 * price` and the bite thresholds : SOUND (verified exactly)

The arithmetic checks out. `floor/frac = 5*price/0.0075` gives the stated thresholds precisely
(appendix A): price 0.45 bites at $300, 0.59 at $393, 0.85 at $567. And `clip_for` does reintroduce
flat-minimum betting below the bite equity: at price 0.59 the clip is pinned at $2.95 for every
equity from $393 down to $10 (`drawdown_sim.py:74-77`, verified in appendix A). The "linear ruin
at a low level" mechanism the plan describes is real in the code.

### 2b. Using the MEDIAN price for the floor : FLAWED (the floor should bind on the worst/highest price actually paid)

The floor binds on the trade you actually place. The strategy enters up to ~0.85, and an 0.85
entry is un-sizable below $567, not below $393. Using the median (0.59) price understates where
the floor first bites. The difference is not academic; it flips the verdict inside the exact band
the plan uses for its viability argument (appendix A):

- At equity $400, want = 0.0075*400 = $3.00. Versus median price floor $2.95 the sim says "size"
  ($3.00 > $2.95); versus the max price floor $4.25 it says "cannot size" ($3.00 < $4.25).
- Concretely in `run()`: from a $400 start, a single -$5000 (at $50 clip) day ruins the account
  at price 0.85 but **not** at price 0.59. So the plan's "$400: 36% touch the zone"
  (`drawdown-handling-plan-2026-07.md:74`) is calibrated to the lenient median floor and would
  look materially worse at the binding price.

Note the internal inconsistency: the **governance** number ($550 hard stop) is correctly anchored
to the *max*-price band ("where pricier entries first hit the floor", `:78-84`, i.e. $567), but
the **sim** that supposedly validates $850 safety uses the median floor ($393). The artifact and
the rule are not on the same floor model.

### 2c. Ruin condition `E <= 5 * price` : FLAWED (mis-defined; inflates the safety claim)

Ruin is defined as "cannot place even one minimum order" (`drawdown_sim.py:93`), i.e. equity <= $2.95
(median) or <= $4.25 (max). That is a **99.6% drawdown**. I verified `run()` ruins exactly at that
line (appendix A: at price 0.59, start $2.96 survives, $2.94 ruins; an account at $100 equity
returns `ruin=False` because a single $2.95 order is still placeable).

This makes "**0/3000 ruin at $850**" almost vacuous: it asserts only that the path did not reach
literal ~$3. The number the plan should headline, and does compute but buries ("X% touch the
<$400 zone", `:74`), is the probability of entering the floor band where sizing breaks. "0 ruin"
is rhetorically inflated by a ruin line set one minimum-order above zero.

---

## 3. Governance conclusions

### 3a. "$850 is safe / 0-3000 ruin" : OVERSTATED

Three independent biases all point the same way, and the safety headline inherits all three:

1. **Optimistic P&L.** The floor-section safety numbers are explicitly "no haircut"
   (`drawdown-handling-plan-2026-07.md:72`), i.e. `haircut=1.0` raw replay, while `main()`'s
   default and the doc's own conservative live estimate is `haircut=0.6` (`drawdown_sim.py:105`).
   The zone-touch stats depend on drift (equity has to drift down to reach $400); cutting drift
   by 40% can only increase the breach probability. The safety case uses the favorable P&L.
2. **Mis-defined ruin line** (2c above). 0 ruin at a $2.95 line is not a safety guarantee.
3. **Median floor** (2b). Calibrated to the lenient price.

Strip those three and the "$850 SAFE" verdict is not supported by what is shown.

### 3b. The $550 hard stop : defensible anchor; "never below $600" is thin

The $550 line is the one genuinely solid, mechanical conclusion: it approximates the max-price
floor bite ($567), so it marks where the sizing math actually breaks. Keep it. But two caveats.
First, the sim that establishes the $850-to-$550 buffer uses the median floor, so the buffer is
not computed against the same floor model that defines the stop. Second, "never start below $600"
(`:84`) is a ~$50, i.e. ~9%, buffer over $550. With intraday concurrency and the ceiling flat-band
(4a/4c), effective capital at risk is larger than the single-trade clip implies, so 9% is thin for
a viability line.

### 3c. Adversarial "survives, trough $203" : OVERSTATED framing

From $850 the worst-8-first path bottoms at $203 (`:76`), a **76% drawdown**, presented as
reassurance because it cleared the $2.95 ruin line. This is the same low-ruin-bar problem as 2c:
a 76% loss on real money is a near-total loss, not "survival". And the "astronomically unlikely"
dismissal (`:26`) is the exchangeability flaw from 1b again.

---

## 4. Missing or underweighted

### 4a. The ceiling reintroduces FLAT sizing above ~$1333; the "structural fix" only holds in a band

This is the hole the analysis entirely misses. The clip is flat-pinned at the floor below ~$393
AND flat-pinned at the ceiling (`CEIL_DEFAULT=10`, `drawdown_sim.py:44`) above ~$1333. I verified
(appendix A): clip = $2.95 at E=$200, rises through the fractional range, hits $9.99 at E=$1333,
then stays $10.00 at E=$2000, $5000, $10000. **Fractional sizing, the property that makes "ruin
impossible because clips shrink with equity", only operates between ~$393 and ~$1333.**

Above ~$1333 the clip is a constant $10, so a losing streak drains linearly, which is precisely
the June "flat $50 = linear path to ruin" mechanism (`:21`, `:33-34`), just slower. The plan's
stated *goal* is growth ("endpoint $10,836", `:24`), yet the moment the account grows past $1333
it loses the exact protection the plan sells. The claim that the curve "asymptotes toward zero
and never reaches it" (`:33`) is false outside the $393 to $1333 band.

### 4b. Intraday drawdown is invisible (daily granularity)

The sim aggregates per-trade P&L into one daily number and sizes once per day. A single market,
or several concurrent markets, can move hard against the book mid-day; peak-to-trough intraday
equity and any floor/throttle trigger inside the day are not modeled. The proposed drawdown
throttle (plan item 3, `:43-45`) keys off daily equity, so it structurally cannot fire on an
intraday excursion that resolves before the day closes.

### 4c. Position concurrency

"0.75% fractional" is per-trade. With N concurrent positions the book has up to N*0.75% at risk
simultaneously; the daily sum reports the net but not the concurrent gross exposure or correlated
simultaneous losers. The "$850 safe" framing prices single-trade risk, not portfolio risk.

### 4d. Realization haircut applied symmetrically : UNJUSTIFIED

`drawdown_sim.py:15` multiplies every day's P&L by the haircut, wins and losses alike. A symmetric
multiplier *shrinks* losses (a -$100 replay day becomes -$60), but live losses are typically
larger than replay, not smaller: worse entry and exit prices, real adverse selection on marginal
entries. The calibration sample does not even support symmetry: Jul 3 had live beating replay
(-$156 vs -$282), the opposite sign from Jul 2. Applying a symmetric haircut is the optimistic
choice on exactly the side (downside) that matters for a drawdown study.

Separately, it is not stated whether the replay `pnl` field is net of Polymarket taker fees. The
realization doc refers to taker rebates "accruing uncredited", which suggests the replay may be
gross of rebates; if so, the daily series is upward-biased on top of the haircut issue.

### 4e. Regimes longer/correlated than the sample

The worst case in the data is a 4-day red streak / -$2,364 cumulative (`:7`). A two-to-three-week
adverse regime (the kind the team actually lived through in June at the live level) is outside the
support of any 75-day-without-replacement shuffle. The MC cannot price it, and nothing else in the
plan does either.

---

## 5. Bottom line and biggest risk

**Verdict: not safe enough to risk ~$850 on the current analysis. There is a hole.**

The analysis certifies safety inside the $393 to $1333 fractional band, but the account is
protected by fractional sizing *only* inside that band: below ~$400 it degrades to flat-floor
linear drain (the June mechanism), and above ~$1333 it degrades to flat-ceil linear drain (4a).
The Monte Carlo that produces "0/3000 ruin" assumes exchangeable days and therefore cannot see
the clustered-regime tails that actually caused the June blowup (1b), and the headline combines a
near-zero ruin line ($2.95, 2c), an optimistic no-haircut P&L (3a.1), and a median-price floor
(2b), each biased toward safety, none reproducible from the committed repo.

**The single biggest risk the analysis underweights: the "structural fix" (fractional sizing makes
ruin impossible) is only valid in a bounded equity band, and the strategy's own growth target
pushes the account out of that band into the flat-ceil regime where linear-drain ruin, the exact
June failure mode, returns.** The plan sells "ruin is already impossible" while engineering the
account toward the equity level where that stops being true.

If I had to keep one thing, it is the $550 max-price-floor stop (3b). To make the rest credible,
recompute the supporting sim with: (a) the max-price (or per-trade) floor, not the median;
(b) `haircut=0.6` applied to the floor-section stats, not just the top table; (c) a clustered /
Markov day-ordering model (or at minimum a longer, streak-aware adversary) instead of uniform
without-replacement shuffling; (d) headline metric = "% of paths breaching the floor band", not
"% ruined" at a $2.95 line; and (e) a ceiling-aware growth policy, because the current one grows
the account straight out of its own protection.

---

## Appendix: commands and outputs relied on

**A. Floor / clip / ruin / ceiling mechanics** (imports the sim's own functions; data-independent):

```
python3 - <<'PY'
from scripts.research.drawdown_sim import clip_for, run
# bite thresholds
for p in (0.45,0.59,0.85):
    print(p, 5*p, int(5*p/0.0075))
# clip flat-pinned at floor below bite, at ceil above ~1333
for E in (200,393,567,850,1333,2000,5000,10000):
    print(E, round(clip_for(E,E,0.0075,10,0.59,False,0.8,0.5,'forced'),2))
# ruin triggers exactly at E<=5*price
for start in (2.96,2.94):
    print(start, run([-5000.0],start,0.0075,10,0.59,1.0)[2])
# price choice flips the $400 verdict
print(0.0075*400 < 5*0.59, 0.0075*400 < 5*0.85)   # False, True
PY
```
Key reproduced numbers: bites at $300/$393/$567; clip pinned $2.95 for E in [$10,$393], $10.00 for
E >= $1333; ruin at E<=$2.95 (0.59) and E<=$4.25 (0.85); at $400 start a -$5000@50 day ruins at
price 0.85 but not at 0.59.

**B. MC clustering distortion** (longest red-run tail, 60 days, 28% red marginal):

```
exchangeable(shuffle) p50=3 p95=4 p99=6  max=9
clustered(Markov)     p50=5 p95=9 p99=11 max=17
```
The plan's "worst red streak 4 days" equals the exchangeable p95: the shuffle model cannot emit a
regime worse than the sample, so it understates clustered-regime tails.

**C. The sim itself:**

```
$ python3 scripts/research/drawdown_sim.py
no trade data found; run the TUNE/latency sweeps first     # exit 1, no traceback
```
Runs clean (no crash) but produces no output because the input trade dumps are gitignored and
absent from this checkout. All MC/ruin percentages in the plan are therefore not independently
reproducible from the committed repo; the data-dependent claims in sections 1c, 3, 4 rest on the
plan's reported figures, not on numbers I could regenerate.
