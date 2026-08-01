# Capture Pipeline Spec — corrective-supervision dataset for the learning loop

Status: SPEC (2026-06-05). Builds on `LEARNING_LOOP_ARCHITECTURE.md`. Assumes the
**hybrid** capture choice (§3 there): Kirra emits a small non-blocking verdict record; a
Linux collector joins it with bus telemetry into the full triple.

> **Recorded status.** Companion spec to `LEARNING_LOOP_ARCHITECTURE.md`. **The §3
> capture-location decision is CONFIRMED — hybrid (3)** (owner 2026-07-04); this spec is the
> hybrid-(3) elaboration. Its own §6 decisions (sink, correlation key, model-version
> attribution, dataset format, pass-sampling) are **all RESOLVED** by `COLLECTOR_DESIGN.md`
> D1–D6 (owner 2026-06-06) — see §6 below. **As-built note:** both emit seams exist on `main`
> today — fast-loop command gateway (Phase 1, #191) and the slow-loop
> `crates/kirra-ros2-adapter/src/node.rs` (Phase 1.5, #192): it binds `objects`,
> `effective_perception_cap`, then `verdict = validate_trajectory_slow_capped(...)`; the
> capture emit runs LATER in the tick — after `update_trajectory(...)` and **after the WCET
> measurement** — so the bounded `try_send` never counts against the slow-loop budget. The §0
> verdict-path anchor is the reviewed-amended talisman blob `851f3f44…` (H1/M1, then #1242 and
> #1243; it superseded `6a61b74f…`, `ed00f4da…` and the historical `997fb7ae…` — see §0).

## 0. The non-negotiable constraint
The verdict path is a FROZEN, reviewed talisman (`validate_vehicle_command`,
`enforce_degraded_decel_to_stop`, `DenyCode`, `effective_max_speed_mps`, …), living
in `kirra_core::kinematics_contract` (relocated verbatim in de-monolith Stage 3;
`src/gateway/kinematics_contract.rs` is a re-export shim).

> **Reviewed amendment (stop-gate review H1/M1).** The contract was DELIBERATELY
> amended once under the stop-gate review: `EnforceAction::ClampBoth` (H1 — a
> command breaching the longitudinal ceiling AND the lateral envelope now clamps
> BOTH axes instead of dropping the velocity correction) and direction-aware
> accel/brake selection (M1 — reverse acceleration is bounded by the accel limit,
> not the brake limit). The talisman re-pins to the amended logic blob
> `crates/kirra-core/src/kinematics_contract.rs = 851f3f44bcc23e7020397b01b830c17d510a0917`
> (superseding `6a61b74f…` (#1243), `ed00f4da…` (#1242), and before them the
> historical `997fb7ae…`, which predated the Stage-3 relocation and matched no
> current file).
>
> **#1243 re-pin — INTENTIONAL BEHAVIOUR CHANGE, not formatting drift.**
> `6a61b74f…` → `851f3f44…`.
>
> WHAT CHANGED. Priority 3/4 (the implied accel/brake rate bound) carried a
> `!ceiling_bound` guard, so a command over the speed ceiling was returned at the
> ceiling magnitude with its implied ACCELERATION never checked. Measured before
> the fix: an executed 5.0 → 35.0 m/s over 0.1 s implies 300 m/s² against a 2.5
> limit — 120×. The guard is removed; the returned linear is now the TIGHTER of
> {ceiling, rate bound}. Same early-exit defect class as #1242, one protected
> property over.
>
> WHAT IT COST — a safety-case amendment, recorded because it is not free. K3
> read "an over-ceiling command clamps to EXACTLY the ceiling, direction
> preserved". BOTH halves are now false in general:
>
> * magnitude — the rate bound can be tighter, so 50.0 from 5.0 returns 5.25;
> * direction — the bound steps `v` from CURRENT toward the request, so a +50.0
>   request from −5.0 returns −4.55. The executed sign follows the vehicle's
>   travel, not the request. Physically correct (no wheeled vehicle reverses
>   direction inside one tick) and a real change to what the proof asserted.
>
> K3 is restated, with both statements recorded in the harness: the executed
> magnitude never EXCEEDS the ceiling, and it is exactly the ceiling with the
> request's sign precisely when the ceiling is the only binding constraint. That
> conditional half is what still stops a future priority returning something
> arbitrarily below the ceiling and calling it enforcement.
>
> RESIDUAL — NOT CLOSED, and deliberately visible. When `|current| > ceiling` the
> ceiling itself forces a rate breach: a 39.9 m/s request from 40.0 m/s is a
> lawful −1.0 m/s², and clamping it to a 35.0 ceiling implies −50 m/s². No
> ordering of the two priorities avoids it — the vehicle is already outside the
> envelope and cannot be inside it one tick later within the brake limit.
> INVARIANT 8 ("clamp to the absolute hard boundary first, then apply
> rate-of-change limits; envelope cap always wins over rate priority") decides
> it, and K6 (P-CAP) independently requires the return to respect the ceiling, so
> the ceiling wins and the breach stands, bounded to that region. This is why
> K8's domain is `|current| ≤ ceiling`: a recorded conflict between two enforced
> bounds, not a convenience that makes a proof pass. It is pinned by an
> EXPECTED-BUT-UNDESIRED fixture
> (`crates/kirra-core/tests/over_ceiling_accel_bound.rs`) that fails the day the
> resolution changes.
>
> BLAST RADIUS — the apply-site inventory was NOT sufficient, and that is a
> method finding worth carrying forward. `CLAMP_APPLICATION_INVENTORY.md`
> enumerates consumers of `EnforceAction`; all four were mechanical and needed
> no change, exactly as it predicted. But `parko-kirra`'s DIVERSE governor is
> not a consumer — it is a second, independent implementation of the contract,
> cross-checked against the primary by `GovernorComparator`. It had mirrored
> this defect deliberately, its own comment noting that "the primary
> early-returns on it before computing acceleration", so the comparator went on
> agreeing while both governors returned a command implying 300 m/s². Diversity
> buys independent derivation, not independent requirements: two
> implementations of the same wrong rule agree perfectly. Both were fixed, the
> diverse one keeping its interval formulation. A kernel-semantics change must
> sweep re-implementations as well as apply sites.
>
> Three further consumers pinned the OLD value in their expectations and were
> updated with the reasoning inline, not just the number: the fabric industrial
> profile, the `prop_clamp_linear_preserves_direction` proptest (which #1243
> forecast by name as the casualty), and the actuator response-schema
> integration test.
>
> EVIDENCE. `crates/kirra-core/tests/over_ceiling_accel_bound.rs` (reproducer +
> non-vacuity control + the residual; the reproducer and the reversal case were
> RED before the fix, the control and the residual green throughout, which is the
> signature of a branch-specific defect rather than a broken oracle); Kani K8 —
> the acceleration bound over every executable return — plus the restated K3 and
> its two-part mirror; K1–K7 re-run.
>
> ASSURANCE CHANGE — this re-pin is not only a behaviour change. #1243 pushes
> SG1's true property beyond the per-PR Kani budget, so K3 moves to the weekly
> deep lane and a concrete mirror becomes its standing per-PR gate. That is an
> explicit REDUCTION IN PER-PR SYMBOLIC COVERAGE for a cited safety property,
> and it requires approval on those terms, not merely as proof maintenance.
> Measured: the pre-#1243 form took 22 s and is now false; the conditional and
> disjunctive true forms each produced no verdict at 15 minutes. The property
> was deliberately not weakened to recover the runtime.
>
> **CORRECTION (2026-07-31, same day, before the weekly lane ever ran with K3
> in it) — the paragraph above overstates what "moves to the weekly deep lane"
> currently buys, and the overstatement is load-bearing enough to fix rather
> than footnote.** The `kani-deep-weekly` lane has NEVER COMPLETED A RUN. Its
> three scheduled runs (12, 19, 26 July) were each killed at the GitHub-hosted
> SIX-HOUR per-job ceiling and reported `cancelled`; the workflow's
> `timeout-minutes: 480` cannot raise that ceiling and has no effect. All
> harnesses run serially in one job, and R2 alone consumed 5 h 59 m in a single
> kissat solve, so anything sequenced behind it receives no solver time at all.
> K7's demotion under #1242 therefore delivered ZERO effective coverage, not
> reduced coverage, and K3 and K8 inherit exactly that position.
>
> The honest statement of this re-pin's assurance effect is therefore STRONGER
> than the paragraph above: SG1's symbolic proof is not relocated to a slower
> tier, it is **suspended**, and the concrete two-part mirror is not a backstop
> beneath a working symbolic tier — for now it is the ONLY tier. That the
> property still has per-PR coverage at all is due to the standing rule that
> every deep harness keeps a BLOCKING concrete mirror; without it this would be
> a coverage hole rather than a coverage reduction.
>
> Tracked as #1260. This correction stands until the lane is fixed AND has
> completed a successful run — a merged workflow change is not sufficient
> evidence, since what failed here was believing a budget that was never
> delivered. Do not restore the "multi-hour budget" wording before then.
>
> **RESOLVED FOR K3 (2026-08-01) — SG1's symbolic proof is RESTORED, and the
> reason it was ever lost is worse than this note first recorded.** The lane was
> repaired (#1262: per-harness matrix, and a timeout that fails red instead of
> reporting grey `cancelled`), and its first completing run
> [30667373086](https://github.com/kirra-systems/kirra-runtime-sdk/actions/runs/30667373086)
> discharged K3 in **67 m 13 s** — `0 of 310 failed`, `VERIFICATION:- SUCCESSFUL`,
> `Verification Time: 4022.678s`. That is the evidence this correction demanded:
> the lane completed, and it proved the property.
>
> **The inference recorded above was wrong, and it is worth being exact about
> how.** The measurement — no verdict at 15 minutes — was accurate. What was
> inferred from it, that the property is beyond a practical budget, was not: K3
> needed 67 minutes and was never given them, because the lane it was demoted
> INTO had been dead since 12 July. A 15-minute observation cannot distinguish
> "intractable" from "needs 67 minutes", and the old shared-lane design could
> never have revealed the difference, since R2 or K7 consumed the whole job
> before K3 was reached. The assurance loss for SG1 was therefore larger than
> recorded AND avoidable. **The standing lesson: a timeout is a lower bound on
> cost, never evidence of intractability.**
>
> SG1 now has a genuine symbolic proof again, weekly, at ~67 min. K3 stays in
> `deep-proofs`; the two-part concrete mirror stays BLOCKING per-PR, so SG1 now
> holds both tiers rather than one. What is NOT restored is a per-PR symbolic
> proof — 67 min exceeds the 45-min per-PR budget — so the reduction this re-pin
> records is real, just far smaller than the suspension above described.
>
> **K8 — PROFILED AND DEMOTED (#1260).** It met the bar this note set: not the
> 300-minute timeout, but a phase diagnosis. The full harness completes three
> solves (101 s) then enters a fourth propositional conversion that never
> reaches a solver — 43 of 45 minutes. Isolated to its ONE real assertion, with
> 373 of 374 properties discarded, the CNF moves 0.54% (848,226 → 843,634), so
> the property set is not the cost; conversion then completes in 1.0 s, the
> solver IS reached, and returns nothing at 40 minutes. That is K7's signature
> exactly — conversion-pathological in multi-property mode AND solver-hard
> underneath — so more budget is not a credible remedy. Its 2,880-point
> physical-dt grid is unchanged and still blocking, and it asserts the
> ACCELERATION-space form the proof abandoned, so no coverage was lost.
>
> **R2 — STILL RESTRICTED, and deliberately NOT demoted with K8.** It is the one
> harness that never stalls in conversion: it reaches the solver and stays
> there. Its expensive solve is the SECOND — the UNSAT direction, after a cheap
> 36 s SAT — the ordinary shape of a hard but not impossible instance.
>
> **A size claim recorded here earlier was wrong and is corrected.** R2 was
> described as having a formula 4× smaller. Its PRE-SOLVER metrics are indeed
> smaller (2,227 program-expression steps and 74 VCCs, against ~8,900 and ~400),
> but its **CNF is LARGER** — 1,221,711 clauses over 251,960 variables, against
> K7's 738,811 and K8's 848,226. The two measures point opposite ways, and only
> the CNF is what the solver works on. The phase distinction (reaches the solver
> vs stalls before it) is what separates R2 from K7/K8; size does not, and the
> smaller-formula argument must not be repeated.
>
> **The open question is now measured, and the answer is mixed rather than
> clean.** kissat's own telemetry — captured through a pass-through wrapper on
> `--external-sat-solver`, since CBMC's `--verbosity` does not reach an external
> solver — over a full 43-minute solve:
>
> * 0 → 376 s: 26,000 conflicts (70/s), "remaining" 97% → 29%
> * 376 → 2,604 s: 94,659 conflicts (42/s), "remaining" 29% → 26%
> * total 120,888 conflicts, no verdict
>
> **The search never stalls** — conflicts accumulate throughout, restarts and
> reductions advance, and backbone probing, vivification and substitution stay
> active. **But instance reduction decelerates sharply**: almost all of it lands
> in the first six minutes, and the next thirty-seven buy three percentage
> points. R2 therefore sits BETWEEN "progressing" and "plateaued", not in either.
>
> That is enough to keep it a budget/solver question — the search is alive and
> solver choice or a portfolio has a plausible path — and NOT enough to claim any
> particular budget suffices. The deceleration is real and progress is not a
> termination guarantee. Do not upgrade this into "more time will finish it".
>
> Independent approval: **NOT OBTAINED. This amendment proceeded under a
> RECORDED EXCEPTION** to the separation-of-duties control, per
> `docs/safety/TALISMAN_AMENDMENT_POLICY.md` §2.2. The five mandatory fields
> follow. What is being accepted here is BOTH the kernel change AND the
> assurance reduction above — they are one decision, and an approval covering
> only the behaviour change would not satisfy this control.
>
> 1. **Author of the change.** `justinlooney` (via Claude Code operating on that
>    account) — commits on `claude/ros-bound-proposal-migration-nso5d2`,
>    PR #1254.
> 2. **Why no eligible independent reviewer was available.** Structural and
>    unchanged since #1242: a single-maintainer repository with no second human
>    principal holding review rights. No escalation path existed to attempt.
>    This is now the SECOND consecutive talisman amendment to take the fallback
>    path, which §2.3 says should be read as a standing gap in separation of
>    duties rather than as two isolated exceptions.
> 3. **Who accepted the exception.** `justinlooney` (repository owner),
>    2026-07-31, as the accountable authority owning the residual risk.
> 4. **Evidence independently machine-checked.** Named specifically, because
>    this is what substitutes for the missing human: 33/33 CI checks green on
>    `ddc06564`, of which the load-bearing ones are the per-PR Kani lane
>    (12/12 harnesses, 0 failures, including BOTH arms of K3's two-part
>    concrete mirror and K8's 2,880-point physical-dt grid); the mutation gate
>    at 11 mutants / 10 caught / 1 unviable / 0 missed, with its own
>    anti-vacuity check reporting 385 mutable containment mutants under the
>    active config (so the gate was not passing on an over-matching
>    `exclude_re`); the safety-constants provenance gate; and the four-location
>    blob pin gate re-run against
>    `851f3f44`. Separately, the regression suite was verified RED before the
>    fix — reproducer and reversal case red, control and residual green
>    throughout, which is the signature of a branch-specific defect rather than
>    a broken oracle — and the guard-restoration mutant, which cargo-mutants
>    cannot generate, was checked by hand-application.
> 5. **Residual risk from the absent human independence.** Three judgement
>    calls carry this change and no gate tests any of them.
>    **(a) Is the bounded property the right property?** The acceptance
>    criterion as originally written is unachievable: above the ceiling, the
>    ceiling itself forces a rate breach. Invariant 8 and K6 jointly decide the
>    ceiling wins, so K8's domain is bounded to `|current| ≤ ceiling` and the
>    excluded region is pinned by an EXPECTED-BUT-UNDESIRED fixture. That the
>    exclusion is a recorded conflict between two enforced bounds, rather than a
>    convenience that makes a proof pass, is an author's judgement.
>    **(b) Is the K3 demotion acceptable as a STANDING reduction?** No budget is
>    known to suffice for any true formulation — the measured evidence is
>    non-convergence at 15 minutes for both, not a measured cost. The view that
>    no cheap true K3 exists is engineering judgement; the timings are the
>    evidence. K8 is in the same position (no verdict at 25 or 55 min), and
>    unlike R2 neither was measured to merely exceed a known budget.
>    **This field was written believing the destination lane worked.** It did
>    not (see the CORRECTION above, #1260), so for a time the risk accepted here
>    was larger than the field as first written describes: not "K3 proved more
>    slowly" but "K3 not proved at all". **Resolved 2026-08-01: K3 discharges in
>    67 m 13 s and SG1's symbolic proof is restored** — so the risk this field
>    describes has SHRUNK to its originally-intended size (a slower tier), not
>    grown. The judgement that was actually wrong is the one this field asserts:
>    "no budget is known to suffice" was an inference from a 15-minute
>    non-result, and 67 minutes sufficed. The acceptance stands; the reasoning
>    behind it does not, and a reader should weigh the field accordingly.
>    K8 and R2 remain unproven and are NOT covered by this resolution.
>    **(c) Axiom A3** — the inverse-monotone relation coupling the `tan`/`atan`
>    model — remains assumed, not proved, and carries over from #1242 unchanged.
>
> **Sequencing: satisfied.** Unlike #1242, this record was written BEFORE merge,
> with CI already green and the merge deliberately held for it. That is the
> control working as intended on its second application.
>
> What the exception covers: the Priority-3/4 over-ceiling rate-bound change and
> its blob re-pin `6a61b74f…` → `851f3f44…`; the parallel fix to the parko
> diverse governor; the bounded acceptance property and its recorded residual;
> and the demotion of K3 and K8 to the weekly deep lane with their concrete
> mirrors as the standing per-PR gates.
>
> **#1242 re-pin — INTENTIONAL BEHAVIOUR CHANGE, not formatting drift.**
> `ed00f4da…` → `6a61b74f…`.
>
> WHAT CHANGED. Priority 2 (the effective-speed ceiling) previously `return`ed
> `ClampLinear` directly, which skipped P5a (rack limit), P5b (slew) and P6
> (lateral envelope) entirely: a command over the speed ceiling had its steering
> demand executed UNCHECKED. Measured before the fix — 200 deg passed through a
> 35 deg rack; 24 deg at the capped 5.225 m/s implied 4.34 m/s² against a 3.5
> envelope; and at a 35 m/s ceiling the envelope permits only ~0.46 deg, with any
> demand passing. Priority 2 now records its correction into `v`/`v_clamped` and
> the steering priorities ALWAYS run, so the single terminal
> `match (v_clamped, delta_clamped)` is the only executable exit.
>
> OBSERVABLE CHANGE, deliberately minimal: such commands now return
> `ClampBoth { linear, steering }` where they previously returned `ClampLinear`.
> The `linear` MAGNITUDE IS UNCHANGED — it is still exactly the ceiling.
>
> WHAT WAS DELIBERATELY NOT CHANGED **AT THE TIME — SUPERSEDED BY #1243, which
> removed this guard. The paragraph is kept as the record of what #1242 decided
> and why, not as a description of current behaviour.** P3/4 (the accel/brake
> bound) remained skipped
> when the ceiling binds — implemented as `!ceiling_bound` on the two assignment
> conditions rather than a wrapper around the block, so the frozen-file diff this
> pin certifies stays small (2 changed lines, not 25 re-indented). Letting it run would return the tighter of {ceiling,
> accel bound} and make **Kani K3** false — "SG1 P2 speed-ceiling clamp exact
> (magnitude = ceiling, direction preserved, ODD-cap min honored)" is one of the
> twelve machine-checked properties this safety case cites as proved. Amending a
> proved property does not belong inside a lateral-envelope fix. The consequence —
> that the accel limit is not applied to over-ceiling commands — is a REAL
> pre-existing gap of the same early-return class, tracked as **#1243** with its
> own evidence. Figure corrected there: the EXECUTED command implies 300 m/s²
> against a 2.5 limit (120x); the ~450 m/s² quoted earlier was the raw REQUEST's
> implied acceleration, which is never emitted.
>
> EVIDENCE. `docs/safety/TALISMAN_CHANGE_PLAN_1242.md`,
> `docs/safety/CLAMP_APPLICATION_INVENTORY.md`, `MUTATION_BASELINE.md` §8,
> Kani K3/K3b/K6 + K7's exhaustive grid mirror,
> `crates/kirra-core/tests/speed_cap_lateral_envelope.rs`,
> `crates/kirra-core/tests/rate_limit_epsilon_boundary.rs`.
>
> CORRECTION (post-merge). This note previously read "K1–K5 mirrors green (K3
> intact by construction)". **K3 was NOT intact.** Widening which paths are
> reachable put the P6 `tan` on the paths K3 quantifies over, and Kani fails a
> harness whenever an unsupported foreign call is REACHABLE, whether or not an
> assertion depends on its value — K3, K3b, K6 and K7 all failed with
> `call to foreign "C" function 'tan' is not currently supported`, with no
> assertion violated. Resolved by MODELLING `tan`/`atan`/`powi` in the proof
> crate (axioms A1/A2/A3; the talisman is not modified for the prover), after
> which 13/13 per-PR harnesses discharge. K7 is deferred to the weekly
> `deep-proofs` lane WITHOUT a measured budget — it was stopped at 23 min of
> CBMC time with no verdict — so its per-PR gate is a 306,180-point exhaustive
> grid, not a proof. The residual honest exclusion is narrower than before: the
> P6 numeric envelope VALUE is still not proved, because under the model the
> proofs never evaluate a real `tan`.
>
> The generalizable lesson for the NEXT talisman amendment: ask not "does my new
> property reach a construct CBMC cannot model?" but "does this change put such
> a construct on any path an EXISTING harness quantifies over?"
>
> Independent approval: **NOT OBTAINED. This amendment proceeded under a
> RECORDED EXCEPTION** to the separation-of-duties control, per
> `docs/safety/TALISMAN_AMENDMENT_POLICY.md` §2.2. It is deliberately NOT
> written as "approval with a caveat": a control that any caveat can satisfy is
> not a control, and the author's own acknowledgement is not second-principal
> approval. The five mandatory fields follow.
>
> 1. **Author of the change.** `justinlooney` (via Claude Code operating on that
>    account) — commits on `claude/ros-bound-proposal-migration-nso5d2`, merged
>    as PR #1244.
> 2. **Why no eligible independent reviewer was available.** Structural: this is
>    a single-maintainer repository with no second human principal holding
>    review rights. No escalation path existed to attempt, which is itself the
>    finding — the constraint is standing, not incidental to this change.
> 3. **Who accepted the exception.** `justinlooney` (repository owner),
>    2026-07-30, as the accountable authority owning the residual risk.
> 4. **Evidence independently machine-checked.** This is what substitutes for
>    the missing human, so it is named specifically rather than as "CI green":
>    13/13 per-PR Kani harnesses discharged under the transcendental model
>    (K1–K6 symbolic; K7 deferred, see the residual risk below); the 306,180-point
>    exhaustive P-CAP/P-RACK grid; the mutation gate at 19 mutants / 18 caught /
>    1 unviable / 0 missed, with the three tolerance-boundary kills verified by
>    hand-applying each mutation; K3b non-vacuity proven by two `kani::cover!`
>    properties; the `powi(2) == v * v` bit-identity measured across subnormals,
>    exponent extremes and the operating range; and the four-location blob pin
>    gate re-run against `6a61b74f`.
> 5. **Residual risk from the absent human independence.** Machine checks
>    establish that the stated properties hold; they cannot establish that they
>    are the RIGHT properties, and nobody independent examined the judgement
>    calls. Concretely: **axiom A3** — the inverse-monotone relation coupling
>    the `tan`/`atan` model — is assumed, not proved, and the deferred K7 P-RACK
>    result rests on it; a malformed A3 would not be caught by any gate here.
>    K7's demotion to the weekly lane is provisional with no measured budget, so
>    "deferred" could in principle become "never discharged" without anyone
>    noticing. And the scope boundary below (that #1243 is excluded) is an
>    author's judgement that no second party tested.
>
> **Sequencing also failed, separately.** This note required the approval to be
> written here BEFORE merge. It was not: #1244 merged with all 32 lanes green
> and this line still reading PENDING; the exception was recorded afterwards. A
> future audit should read this as "merged, then documented" — not "approved on
> schedule".
>
> What the exception covers: the Priority-2 accumulate change and its blob
> re-pin `ed00f4da…` → `6a61b74f…`; the proof-crate transcendental model and its
> axioms A1/A2/A3; K7's provisional, unbudgeted deferral; and the accepted
> `:544` equivalent mutant. It does NOT cover #1243, the acceleration-enforcement
> gap, which is tracked separately with its own evidence.
>
> Any FURTHER change re-pins again + re-runs the
> WCET/MC-DC/proptest gates.

Capture is
**purely additive at the call site** and must never:
- change the verdict, its inputs, or its timing/WCET;
- block, allocate, or do I/O on the verdict path;
- be required for the safety domain to function (it fails closed identically with capture on
  or off).

Capture is **off by default** behind a flag (`KIRRA_CAPTURE_ENABLED`); enabling it changes
no verdict.

## 1. Where it hooks (the seam — NOT the verdict path)
The slow-loop tick in `crates/kirra-ros2-adapter/src/node.rs` already computes everything
the safety side of the triple needs, right after:

```
let verdict = validate_trajectory_slow_capped(&traj.points, slow_corridor, &objects,
                                              &slow_state.config, odom, …);
```

At that point the tick holds: `traj` (the doer's PROPOSAL), `objects` (perception),
`odom` (ego), `posture`, `effective_perception_cap`, and `verdict` (the DECISION +
correction). The capture call lives **in `node.rs`, not in `kinematics_contract.rs`** — and
as built runs LATER in the same tick (after `update_trajectory(...)` and the WCET
measurement), so it captures the same verdict without counting against the slow-loop budget.
(A second record fires at the fast-loop command-gateway site.)

## 2. What Kirra emits — the verdict record (small, safety-side)
Keep the on-tick record tiny and fixed-shape; the bulky inputs are pulled from the bus by
the collector (§3). Fields:

| field | source | why |
|---|---|---|
| `decision_seq` | node-assigned monotonic counter | join key + ordering |
| `t_mono`, `t_wall` | clock | ordering / bus join |
| `corr_objects_ms` | `objects_ms` freshness stamp | join to the perception frame |
| `corr_traj_stamp` | the proposal trajectory's stamp | join to the proposal |
| `outcome` | from `verdict` | accept / clamp / MRC / reject |
| `deny_code` | `DenyCode` | which check fired (kinematic / corridor / RSS / staleness / perception-derate) |
| `applied_cap_mps` | `effective_max_speed_mps()` / `effective_perception_cap` | the CORRECTION Kirra imposed |
| `mrc` | bool | controlled-stop substitution |
| `posture` | `posture` | nominal / degraded / locked-out context |
| `derate_enabled` | `perception_derate_enabled()` | so passes are attributable |

This is the authoritative "what Kirra decided and did" — the **correction** half of the
triple — and only Kirra knows it. Note it does NOT carry the doer's model version (Kirra
doesn't know it); that's joined on the Linux side (§3).

## 3. Emission mechanism (WCET-safe, fire-and-forget) — as built on `main`
- The call-site `try_send`s the fixed record into a **bounded tokio mpsc channel**
  (`CAPTURE_QUEUE_BOUND = 2048`, `crates/kirra-core/src/capture.rs`). `try_send` is
  non-blocking; if the queue is **Full** (or the writer is **Closed**) the record is
  **dropped** (best-effort, LOUD-logged, a `capture_drops` counter increments) —
  capture never waits and never overwrites. *(A lock-free SPSC ring is the QNX-target
  refinement if/when the emit runs on the partition; the Linux bench uses the mpsc.)*
- A **separate low-priority drain task** (`spawn_capture_writer`, a `spawn_blocking`
  worker) empties the channel and appends records to the sink, coalescing an `fsync`
  per burst — so producers only ever `try_send`, never do I/O on the tick.
- **Sink [RESOLVED — D1]:** local **JSONL files**, one per emitting process (the writers as
  built append JSONL). DDS becomes a transport later iff live fleet aggregation is wanted.

## 4. The Linux collector — assembling the triple
A Linux-only service (never in the safety domain). It:
1. **Subscribes to the bus** and buffers, by correlation key (stamp/seq): the PROPOSAL
   (`traj` / pre-governed command), the PERCEPTION (`objects` → `PredictedObjects`), the
   EGO state (`odom`), and the **doer model version** (stamped by the doer on its
   telemetry — see decision below).
2. **Ingests verdict records** (from the sink).
3. **Joins** record ↔ bus telemetry on `decision_seq` (+ `corr_objects_ms` /
   `corr_traj_stamp` as cross-checks) → one **corrective-supervision sample**.
4. **Writes** the sample to the versioned dataset store (§5).

The collector is where all heavy data engineering lives — the certified checker stays tiny.

## 5. Dataset schema + versioning
One sample = the triple + provenance:

```
sample {
  sample_id, t_mono, t_wall,
  model_version,                 # doer's version (join), partition key
  scenario_tags[],               # optional labels (sim/real, scenario id)
  is_intervention,               # outcome != accept
  inputs   { perception: PredictedObjects, ego: Odometry, trajectory_in: Trajectory },
  proposal { command_or_trajectory },         # what the doer wanted
  verdict  { outcome, deny_code, applied_cap_mps, mrc, posture, derate_enabled }  # what Kirra did
}
```
- **Format [RESOLVED — D4]:** **Parquet/Arrow** for the joined tabular records; heavy sensor
  blobs stay in the rosbag/MCAP with a URI/offset `bulk_ref` in the Parquet row (don't copy
  multi-GB frames into Parquet). Partition `dataset/doer_version=<v>/source=<s>/*.parquet`.
- **Versioning:** partition by `(model_version, date)`; append-only; rotate/bound on the
  bench (it generates a lot). Each training run slices "samples generated by model vN."
- **Selection-bias note (from the architecture doc §5):** the dataset must include
  `is_intervention == false` samples (normal driving), not just corrections — the collector
  records every decision, with optional pass-sampling to control volume.

## 6. Decisions — RESOLVED (by `COLLECTOR_DESIGN.md` D1–D6, owner 2026-06-06)
All five sub-decisions this spec left open are now bound; the collector is where they take
effect. Recorded here for traceability:

1. **Sink → D1:** local **JSONL files** (one per emitting process), not DDS. DDS is a later
   transport option for live fleet aggregation. *(The writers as built append JSONL.)*
2. **Correlation key → D2:** `(source, decision_seq)` primary — `decision_seq` is
   **per-process**, and there are two emitting processes, so it is not globally unique alone;
   bounded by a `t_wall_ms` window and cross-checked with `traj.asset_id` /
   `traj.trajectory_id` / `traj.objects_ms`. *(Follow-up: a gateway-record asset/instance id
   is needed IFF multi-asset comes into scope — `COMMAND_GATEWAY` records carry no `asset_id`.)*
3. **Model-version attribution → D3:** doer-stamped, collector joins, **Kirra stays ignorant**.
   The bench run records the doer version (a latched `/kirra/doer_version` topic or bag
   metadata); the collector partitions the dataset by it.
4. **Dataset format → D4:** **Parquet/Arrow** for the tabular join + a `bulk_ref` URI/offset
   to the heavy blobs in the rosbag/MCAP (not copied into Parquet).
5. **Pass-sampling → D5:** **stratified** — keep ALL clamp/deny/MRC records always; sample
   PASS records at a configurable rate. Bench default `pass_rate = 1.0` (keep everything).

Plus **D6 (collector placement)**: a Rust in-repo `kirra-collector` binary reusing
`CaptureRecord` from the SDK lib for a type-safe join (one authoritative schema, no drift);
it never links the verdict path.

## 7. Build phases
1. **Verdict record + ring + drain + call-site hook** (Rust, in `node.rs`/adapter — NOT
   `kinematics_contract.rs`). Tests: capture never blocks; **the verdict path blob is
   unchanged by capture** (the reviewed-amended talisman `ed00f4da…`); verdicts identical
   with capture on vs off.
2. **Sink** (telemetry topic or file) + a tiny reader.
3. **Linux collector** (bus tap + record ingest + join → sample).
4. **Dataset store + schema + versioning.**
5. (Downstream) training/validation consumers read the dataset.

Phase 1 is the buildable-today piece and the only one that touches the repo's safety-
adjacent code — everything after is Linux-side tooling.

> **Build-time guardrails (restating §0 against the merged code):** Phase 1 lives in
> `crates/kirra-ros2-adapter/` (and a small `kirra-runtime-sdk` record type), gated behind
> `KIRRA_CAPTURE_ENABLED` (default OFF), mirroring the existing fire-and-forget emit
> discipline (`audit_writer_tx.try_send` — wait-free, drop-on-full) and the
> `KIRRA_PERCEPTION_DERATE_ENABLED` default-off precedent. The verdict path is unchanged by
> capture (the reviewed-amended talisman `ed00f4da…`); the on-tick push is a bounded, droppable enqueue that
> the verdict never waits on.
