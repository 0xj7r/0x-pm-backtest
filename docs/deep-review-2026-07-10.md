# Deep review 2026-07-10: what is going wrong, root causes, remaining weaknesses, fixes

Requested after three config-consistency failures surfaced in 48 hours. This
is the full forensic: what happened, the failure classes, why the process
allowed them, what is still weak, and the fix plan. Audit-agent findings are
folded in at the end (section 7).

## 1. Executive summary

The strategy's edge is real and repeatedly validated (bare config, truthful
latency: Feb +$11.9k, Mar +$18.5k, Apr +$13.1k, May +$17.3k, Jun +$2.0-2.5k).
Nothing this week contradicts that. What IS broken is our configuration and
measurement governance: the system has at least seven places a config value
can live, they are hand-maintained, nothing enforces equality, and validation
evidence does not travel with the config it validated. The result: we soaked
and nearly deployed a config that loses in its own backtest, measured
"realization" against baselines that differed from live in both latency AND
config, and reported alarming numbers that were partly artifacts. No money
was lost (paper-only discipline held), but two weeks of measurement are
compromised and the go/no-go timeline resets.

The one number that has never been cleanly measured - live realization of a
validated config - is STILL unmeasured. That is the core unresolved risk, not
any single bug.

## 2. Timeline of the failure cascade

- 2026-05/06: base gates (min_entry_ask 0.45, open_fav, spot_misalign) enter
  the live config as a "May/June drawdown failure mode" fix. Never validated
  at truthful latency as a package (TUNE/latency sweeps all ran bare config).
- 2026-06-11: commit 01de6b5f1 sets shadow CLI clap default
  stop_before_close_s (lane-mode work), diverging from the frozen constant 90.
- 2026-06-16: commit 750989e98 adds sync_decide_cfg, which copies ShadowConfig
  (i.e. CLI values) over DecideConfig every tick. The CLI default silently
  starts overriding the validated 90 for any launcher that omits the flag.
  The field's own doc-comment ("fade mode keeps its validated 90s constant
  regardless of this flag") becomes false; no test catches it because the
  config-parity test exercises the Rust constructor, not the CLI path.
- 2026-06-16/18: June live deployment loses ~$1,500 (separate reimplementation
  + flat sizing + the general realization gap). Postmortem correctly blames
  process; rebuild begins with heavy paper-first discipline.
- 2026-07-02..07: soak week runs shadow-final with stop_before_close_s=10
  (drifted) and the unvalidated gated config. All realization numbers also
  compared against a 250ms fantasy-latency, bare-config replay. Three
  layers of mismatch in the one comparison that gates real money.
- 2026-07-08: GLM config audit finds M-1 (stop=10 vs 90). Fixed, deployed,
  soak reset. Same audit refutes my wrong skip_spot_misalign claim (my SSH
  spot-check had read a stale Dublin checkout - deployment drift, a fourth
  instance of the same class).
- 2026-07-09: soak still red on the corrected config. Before accepting a
  realization-failure verdict, we finally ran the LIVE config in backtest:
  it loses (-$459 June) while the validated bare config makes +$1,957.
  Decomposition: min_entry_ask 0.45 alone is a -$2,106 swing (it blocks the
  cheap-underdog fades - the payoff tail of the whole strategy). The other
  two gates are cheap. A recommended config (drop min_entry_ask, keep the
  others) recovers +$1,297.
- 2026-07-09: shadow-recommended stream deployed so the cheap-underdog trades
  finally accrue live evidence. July replay data (publication lag ~Jul 11)
  will enable the first genuinely matched live-vs-replay comparison.

## 3. Failure taxonomy - the classes, not the instances

C1. CONFIG SPRAWL WITHOUT PARITY ENFORCEMENT. A "config" exists in >= 7
    places: DecideConfig defaults, frozen_/gated_shadow_final_args (Rust),
    clap CLI defaults (a SECOND set of Rust defaults!), shell flag arrays,
    systemd unit env/args, PROD.md tables, and every analysis script's
    hand-copied command line. Instances: M-1 stop=10; align_min_mid /
    enter_within_close_s CLI-vs-frozen divergences (inert today); the
    replay-vs-live config mismatches; fast_live's compiled-in overrides.

C2. VALIDATION EVIDENCE DOES NOT TRAVEL WITH THE CONFIG. We validated one
    config (bare) and deployed another (gated), and nothing in the pipeline
    linked "this exact flag set" to "this backtest evidence". The deployed
    config had NO backtest run at truthful latency until 2026-07-09,
    months after it went live-shaped. The 15m stream has the same gap today.

C3. MEASUREMENT BASELINE MISMATCH. Realization = live / replay is only
    meaningful if replay matches live in (a) latency and (b) config. We had
    (a) wrong (250ms fantasy) then fixed it, and (b) wrong the whole time
    (bare replay vs gated live) - still wrong in the lat1250 block today.
    Every catastrophic-looking realization number this month mixed the
    latency tax, the config delta, and true execution slippage into one
    number nobody could act on.

C4. DEPLOYMENT STATE DRIFT. What runs on the box is not what is in the repo:
    Dublin's checkout was 10 commits stale (my "120" misread); the agent
    workspace path-deps a SECOND checkout that froze at a June commit once
    before. No automated "running config == repo canonical" check exists.

C5. OPERATOR MEASUREMENT ERRORS (mine). I presented comparisons that were
    invalid: live vs historical-average of other months; live-gated vs
    replay-bare; "realization 0.32" against fantasy latency; a $1.90/clip
    recovery story that the venue's 5-share floor makes impossible; a
    uniform-shuffle Monte Carlo that understated clustered tail risk ~20x.
    Every one was caught by adversarial review (user pushback or GLM audit),
    NOT by my own process. The lesson is structural: safety-relevant numbers
    need an adversarial pass BEFORE presentation, not after.

## 4. Root causes

R1. No single source of truth with enforcement. GATE B (exo_fade_equivalence)
    guarantees the decision FUNCTION is identical across paths, which is why
    decision-parity never broke. But nothing guarantees the PARAMETERS are.
    We built parity for logic and left config to hand-maintenance.

R2. Config changes bypassed the deployment gate under firefight pressure.
    The base gates were added during the May/June drawdown response, tuned
    on live pain, evaluated (if at all) under measurement that we now know
    was broken (250ms latency, at-touch scoring). PROD.md's gate ("backtest
    evidence + 48h paper parity for any config change") existed on paper;
    the emergency path went around it, and nothing mechanical stops that.

R3. The CLI layer is a shadow config. clap defaults are a full second set of
    values that silently apply whenever a launcher omits a flag. Combined
    with sync_decide_cfg's blanket overwrite, every omitted flag is a latent
    M-1. This is a design flaw, not a one-off.

R4. Measurement pipelines were built ad hoc, each hand-copying a command
    line. There is no "matched replay" primitive that derives the replay
    invocation FROM the live stream's own logged config.

R5. Velocity outran verification. Fifteen-plus significant changes shipped
    in nine days (fast engine, event decide, consensus, dwell, knobs, ops).
    Each was individually tested (GATE B + suites green every time), but
    integration-level consistency (does the whole measured system cohere?)
    had no gate. The config audit that found M-1 took one agent-hour and
    could have run any day earlier.

## 5. What survives - the asset inventory

- The EDGE: bare config positive in all 5 validated months at truthful
  latency; chop-is-not-the-enemy result stands (Feb, 5th-choppiest month
  since 2019: +$11.9k). TUNE now fully complete incl Apr 750ms (+$21.4k).
- DECISION STABILITY: twins 100% agreement across ~1,500 markets. The
  decision SSOT + GATE B works - zero logic-parity incidents all month.
- EXECUTION INTEGRITY: paper executor 0 orphans / 0 mismatches over 1,000+
  fills; kill-switch proven.
- THE FAST ENGINE: halves the latency bleed on identical trades (measured
  live, 221-trade head-to-head); event-driven decide deployed.
- SIZING GOVERNANCE: fractional + floor analysis GLM-hardened; ruin
  structurally bounded; drawdown monitor + $550 mechanical stop.
- THE PROCESS ITSELF, partially: every bug was caught in paper at $0 cost.
  June cost $1,500 to learn less than this week cost $0 to learn. But
  catches came from ad-hoc adversarial review; they must become mechanical.

## 6. Remaining weaknesses confirmed today (pre-audit)

W1. fast_live - the intended LIVE DRIVER - still hardcodes min_entry_ask
    0.45 (backtest-negative) COMPILED INTO RUST (fast_live.rs:65). Not
    flag-driven: config changes require recompile+redeploy, the exact
    anti-pattern that caused June's fade_live divergence. CRITICAL.
W2. The "latency-matched" lat1250 replay block runs BARE config while the
    stream it is compared to is GATED: realization_lat1250.jsonl is still an
    apples-to-oranges number. My earlier fix corrected latency, not config.
W3. The 15m stream runs the gated flags (incl min_entry_ask 0.45) via the
    shared launcher; its June validation (+$4,586) was bare config. Same
    C2 class as the 5m mistake.
W4. The recommended config (open_fav+misalign, no min_ask) has June-only
    evidence; needs Feb/Mar/Apr/May runs before it can be a deploy candidate.
W5. The drawdown-sim daily P&L series (data/research/daily_pnl_series.csv)
    is BARE-config; the sizing/floor conclusions technically describe a
    different strategy than any deploy candidate. Directionally fine,
    needs regeneration on the final config.
W6. The v1 gate pre-registration criteria predate the baseline fixes and do
    not specify config-matched replay; they need a re-freeze before the
    (reset) soak is judged against them.

## 7. Audit-agent findings (folded in on completion)

See section appended below.

## 8. Fix plan - structural, prioritized

F1 (CRITICAL, before any further soak conclusions): CONFIG PARITY GATE.
   One canonical config definition in Rust (gated_/recommended_ args); a
   test that (a) asserts every clap default equals the frozen constant or
   is explicitly whitelisted with a reason, and (b) renders the canonical
   CLI invocation and diffs it against the checked-in shell flag arrays.
   Shell flag files become GENERATED (or CI-checked) artifacts. This kills
   class C1 the way GATE B killed decision drift.

F2 (CRITICAL): CONFIG FINGERPRINT + MATCHED REPLAY. Every stream logs a
   config fingerprint (the resolved DecideConfig, hashed) at startup and in
   each summary event. The daily replay derives its invocation from the
   stream's logged config (not a hand-copied command), and the realization
   report REFUSES to compare mismatched fingerprints. Kills C3.

F3 (CRITICAL): make fast_live's overrides flag-driven (same flags module as
   the launchers), remove the compiled-in values (W1). One config surface.

F4 (HIGH): DEPLOY VERIFICATION: a post-deploy script (and cron) that
   compares each unit's running cmdline + binary git-describe + repo HEAD
   across both Dublin checkouts and alerts on drift. Kills C4.

F5 (HIGH): validate the recommended config across Feb/Mar/Apr/May at 1250
   and 750ms (W4), and regenerate the drawdown series + sizing numbers on
   it (W5). Only then is it a deploy candidate.

F6 (HIGH): fix the lat1250 replay block to run matched (gated AND
   recommended variants, one per stream) (W2); re-freeze the v1 verdict
   criteria against matched baselines (W6); align or consciously fork the
   15m stream config (W3).

F7 (GOVERNANCE): the emergency path must not bypass validation: any change
   to a flags file requires a linked backtest artifact (a runs/ json path in
   the commit message) - enforceable with a pre-commit/CI check on the
   flags files. Plus: adversarial review (GLM or equivalent) is now a
   STANDING gate for any safety-relevant number before it is presented or
   acted on, not an occasional extra.

F8 (MEASUREMENT DEBT): backfill the matched replay for the soak-to-date
   days once July data publishes (~Jul 11), so the cheap-underdog
   realization question (the crux) gets answered on the first available
   data: shadow-recommended live vs matched replay.

## 9. The honest bottom line

Nothing found this week says the edge is gone; everything found says our
map of what we were measuring was wrong in correlated ways. The system's
logic layer (decide_entry + GATE B) never drifted once - because it has an
enforcement gate. Every layer WITHOUT an enforcement gate (config, baseline,
deployment state) drifted within weeks. The fix is not more care; it is
extending the same mechanical-parity discipline from logic to config,
measurement, and deployment. Until F1-F3 are done and the matched
cheap-underdog comparison lands, we do not know whether live realization
supports deployment - and that was true two weeks ago too; we just did not
know that we did not know it.
