//! EP-15 proofs — the FROZEN kinematics-contract talisman
//! (`crates/kirra-core/src/kinematics_contract.rs`, git blob `6a61b74f…`).
//!
//! Scope is the DECIDABLE prefix of the pipeline, per the EP-15 plan ("bounded
//! floats via `f64::is_finite` case-split; scope to provable forms honestly"):
//! the Priority-0/1 fail-closed guards, the Priority-2 speed ceiling, and the
//! Degraded decel-to-stop denial gates.
//!
//! #1242 changed what "decidable prefix" means here. K1–K5 were written when
//! Priority 2 RETURNED, so an over-ceiling command never reached the P6 bicycle
//! model and the `tan`/`atan` exclusion cost nothing. Making P2 accumulate puts
//! P6 on those paths. Rather than narrow the harnesses back to the shape the
//! solver liked — which would delete the newly-reachable composition cases from
//! the proofs — the transcendentals are MODELLED, in this crate, by
//! nondeterministic stubs constrained to postconditions that are theorems about
//! the real functions (see "Solver model" below). The exclusion that remains is
//! narrower and stated where it bites: the P6 numeric lateral-envelope VALUE is
//! still not proved here and must not be claimed as proved; it is discharged by
//! the concrete grid and the property suites.
//!
//! LANE SPLIT: K1, K2, K4, K5, K3b and K6 run per-PR in the blocking
//! `kani-proofs` lane. K3, K7 and K8 are behind the `deep-proofs` feature
//! (weekly `kani-deep-weekly`), each with a BLOCKING concrete mirror as its
//! per-PR gate. None has produced a verdict under any budget tried, so all
//! three demotions are provisional rather than budgeted. See K8's doc comment
//! for the cost pattern that predicts which harnesses land here.
//!
//! K3's demotion is NOT routine lane management: it is an ASSURANCE CHANGE
//! introduced by #1243, and SG1 no longer has a per-PR symbolic proof. Its
//! harness records the timings and the reasoning.
//!
//! Properties (cited from `docs/safety/GOVERNOR_INTEGRITY_EVIDENCE.md` §2):
//!  * K1 fail-closed NaN/Inf totality (SG9): ANY non-finite field in a command
//!    → `DenyBreach`, for every possible f64 bit pattern in every field; the
//!    first-priority field maps to its exact forensic code.
//!  * K2 non-physical time delta (SG3): finite fields with `dt ≤ 0` →
//!    `DenyBreach(InvalidTimeDelta)`, before any rate arithmetic.
//!  * K3 the P2 speed ceiling (SG1) — RESTATED by #1243. A command over the
//!    effective max never EXCEEDS the ceiling, and clamps to exactly the
//!    ceiling with the request's sign precisely when the ceiling is the only
//!    binding constraint. The former unconditional "exactly the ceiling,
//!    direction preserved" became false once the rate bound started running on
//!    this path; both statements are recorded at the harness.
//!  * K4/K5 Degraded decel-to-stop (issue #70, SS-002): re-initiation from a
//!    stop and any speed-magnitude increase are DENIED with their specific
//!    codes, for all finite inputs in those regions.
//!  * K8 (#1243) the acceleration bound holds on EVERY executable return, not
//!    just the within-ceiling path, over `|current| ≤ ceiling` — the domain
//!    restriction being a recorded conflict with invariant 8, not a
//!    convenience. See the harness.

#[allow(unused_imports)]
use crate::kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, DenyCode, EnforceAction,
    ProposedVehicleCommand, VehicleKinematicsContract, STOP_EPSILON_MPS,
};

/// A contract with every field a symbolic finite value in a sane positive
/// range, and a symbolic PRESENT-or-ABSENT ODD cap. Only the fields the proved
/// prefix reads matter (`max_speed_mps` / `odd_speed_cap_mps`); the rest are
/// fixed to the nominal reference values.
#[cfg(kani)]
fn any_bounded_contract() -> VehicleKinematicsContract {
    // Grid-scaled per the EP-15 integer-scaling approach: 0.1 m/s steps keep
    // the float derivation exact-by-construction and the solver space bounded.
    let max_speed_raw: u16 = kani::any();
    kani::assume(max_speed_raw >= 1 && max_speed_raw <= 1_000); // 0.1 ..= 100.0 m/s
    let cap_raw: u16 = kani::any();
    kani::assume(cap_raw >= 1 && cap_raw <= 1_000);
    let has_cap: bool = kani::any();

    VehicleKinematicsContract {
        max_speed_mps: f64::from(max_speed_raw) * 0.1,
        max_accel_mps2: 3.0,
        max_brake_mps2: 6.0,
        max_steering_deg: 35.0,
        max_steering_rate_deg_s: 30.0,
        min_follow_distance_m: 5.0,
        max_lateral_accel_mps2: 3.0,
        wheelbase_m: 2.8,
        width_m: 2.0,
        length_m: 4.6,
        overhang_front_m: 0.9,
        overhang_rear_m: 0.9,
        odd_speed_cap_mps: if has_cap {
            Some(f64::from(cap_raw) * 0.1)
        } else {
            None
        },
    }
}

#[cfg(kani)]
fn any_command() -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: kani::any(),
        current_velocity_mps: kani::any(),
        delta_time_s: kani::any(),
        steering_angle_deg: kani::any(),
        current_steering_angle_deg: kani::any(),
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// K1 — SG9 fail-closed totality: ANY non-finite field (any NaN payload,
    /// either Inf, in any of the five fields) is denied before arithmetic, and
    /// the first-priority field carries its exact forensic code.
    #[kani::proof]
    fn k1_nonfinite_input_always_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(
            !cmd.linear_velocity_mps.is_finite()
                || !cmd.current_velocity_mps.is_finite()
                || !cmd.steering_angle_deg.is_finite()
                || !cmd.current_steering_angle_deg.is_finite()
                || !cmd.delta_time_s.is_finite(),
        );

        let verdict = validate_vehicle_command(&cmd, &contract);
        assert!(
            matches!(verdict, EnforceAction::DenyBreach(_)),
            "a non-finite field can never Allow or Clamp"
        );
        if !cmd.linear_velocity_mps.is_finite() {
            assert_eq!(
                verdict,
                EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity),
                "the P0 first-priority field maps to its exact code"
            );
        }
    }

    /// K2 — SG3: with all fields finite, a zero-or-negative time delta is
    /// denied with `InvalidTimeDelta` for every such f64.
    #[kani::proof]
    fn k2_nonpositive_dt_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.steering_angle_deg.is_finite());
        kani::assume(cmd.current_steering_angle_deg.is_finite());
        kani::assume(cmd.delta_time_s.is_finite() && cmd.delta_time_s <= 0.0);

        assert_eq!(
            validate_vehicle_command(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta)
        );
    }

    /// K3 — SG1. An over-ceiling command returns the ceiling exactly, or a
    /// value the rate bound explains; nothing else may move it.
    ///
    /// LANE: `deep-proofs` (weekly) as of #1243, and this demotion is an
    /// ASSURANCE CHANGE, not proof maintenance. SG1 has lost its per-PR
    /// symbolic proof. Say that plainly wherever this is summarised.
    ///
    /// THE EVIDENCE (timings; these are the facts):
    ///
    ///   `|v| == ceiling`, unconditional        22 s — but FALSE after #1243
    ///   conditional on a reconstructed trigger  no verdict at 15 min
    ///   disjunctive, division-free              no verdict at 15 min
    ///
    /// Every formulation that is TRUE after #1243 is per-PR intractable. The
    /// only tractable shape measured on this kernel is a comparison against a
    /// CONTRACT FIELD, and no true statement of SG1 has that shape any more,
    /// because the property is now a relation between the return and two
    /// command fields.
    ///
    /// ENGINEERING JUDGEMENT, labelled as such and carrying less weight than
    /// the timings above: I do not believe a cheap true formulation exists. The
    /// content of the property is "the returned value is the tighter of two
    /// bounds", which is irreducibly relational, and the relational shape is
    /// the one that does not converge. Two earlier diagnoses of mine about this
    /// harness were wrong — first that a variant assertion was the problem,
    /// then that the division was — so treat this as a prior, not a finding.
    ///
    /// WHAT WAS DELIBERATELY NOT DONE. The property is not weakened to regain
    /// runtime. A cheaper K3 was available — assert only `|v| <= ceiling` — and
    /// it was rejected twice over: it is literally K6, so it would add nothing
    /// per-PR, and adopting it would let the safety case read as though nothing
    /// had changed. The assurance loss is recorded instead of engineered away.
    ///
    /// PER-PR GATE: the two-part concrete mirror, written in lock step with
    /// this harness — `k3_mirror_ceiling_exact_when_only_the_ceiling_binds` and
    /// `k3_mirror_rate_bound_wins_when_it_is_tighter`. Mandatory, blocking, and
    /// covering both arms of the disjunction.
    #[cfg(feature = "deep-proofs")]
    #[kani::proof]
    #[kani::stub(f64::powi, super::stub_powi)]
    #[kani::stub(f64::tan, super::stub_tan)]
    #[kani::stub(f64::atan, super::stub_atan)]
    fn k3_speed_ceiling_clamp_exact() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.steering_angle_deg.is_finite());
        kani::assume(cmd.current_steering_angle_deg.is_finite());
        kani::assume(cmd.delta_time_s.is_finite() && cmd.delta_time_s > 0.0);

        let max = contract.effective_max_speed_mps();
        if let Some(cap) = contract.odd_speed_cap_mps {
            assert!(
                max <= contract.max_speed_mps && max <= cap,
                "ceiling = the tighter bound"
            );
        }
        kani::assume(cmd.linear_velocity_mps.abs() > max);

        // #1242 RESTATEMENT — K3 asserts the LONGITUDINAL INVARIANT, not the
        // top-level action variant.
        //
        // It previously required `ClampLinear` specifically. That coupled a
        // longitudinal property to the enumeration used to REPORT composed
        // enforcement, and #1242 made the coupling false: once the steering
        // priorities always run, an over-ceiling command whose steering also
        // needs correcting reports `ClampBoth` while carrying the identical
        // linear value.
        //
        // This is NOT a weakening. Both substantive claims are unchanged and
        // still asserted below — magnitude is exactly the ceiling, direction is
        // preserved. What is removed is an accidental dependence on which
        // variant carries them. The input domain is deliberately NOT narrowed to
        // exclude commands with a steering demand: doing so would delete the
        // newly-exposed composition case from the proof rather than state the
        // invariant correctly.
        //
        // The variant question is a COMPOSITION property and lives in
        // `k3b_composition_reports_both_axes` below, where it belongs.
        //
        // #1243 RESTATEMENT — the unconditional form is no longer TRUE, so it
        // is no longer asserted. Recording both statements, because a proved
        // property that quietly changes meaning is worse than one that is
        // dropped:
        //
        //   BEFORE  an over-ceiling command clamps to EXACTLY the ceiling,
        //           direction preserved.
        //   AFTER   the executed magnitude never EXCEEDS the ceiling; and it is
        //           exactly the ceiling, with the request's sign, precisely
        //           when the ceiling is the only binding constraint.
        //
        // Both halves of the old statement became false when the rate bound
        // started running on this path (#1243):
        //
        //   * magnitude — the rate bound can be tighter (50.0 from 5.0 over
        //     0.1 s now returns 5.25, not the 35.0 ceiling);
        //   * direction — the bound steps `v` from CURRENT toward the request,
        //     so a +50.0 request from -5.0 returns -4.55. The sign follows the
        //     vehicle's travel, not the request. Physically right, and a real
        //     change to what this proof asserted.
        //
        // The conditional half is the part worth keeping: it is what stops a
        // future priority silently returning something arbitrarily BELOW the
        // ceiling and calling it enforcement. `no_tighter_rate_bound` is the
        // kernel's own trigger condition, restated here rather than imported,
        // so the proof states the hypothesis explicitly instead of trusting the
        // implementation to have used the same one.
        let action = validate_vehicle_command(&cmd, &contract);
        let linear = match action {
            EnforceAction::ClampLinear(v) => v,
            EnforceAction::ClampBoth { linear, .. } => linear,
            other => panic!("over-ceiling must clamp the linear axis, got {other:?}"),
        };

        assert!(
            linear.abs() <= max + 1e-9,
            "the executed magnitude never exceeds the ceiling"
        );

        // NO DIVISION — and that is the second correction #1243 forced here.
        //
        // The first restatement expressed the hypothesis by reconstructing the
        // kernel's trigger, `|speed_delta| / dt <= limit + 1e-9`. That is the
        // relational-arithmetic-over-two-command-fields shape that puts a
        // harness in the deep lane: K3 went from 22 s to no verdict at 15 min.
        // Reconstructing a float trigger is also fragile in its own right — a
        // hypothesis even slightly WEAKER than the kernel's real condition
        // makes the conditional conclusion unsound, and one made stronger to be
        // safe silently shrinks the region the theorem covers.
        //
        // Both problems dissolve by stating K3 as a DISJUNCTION with no
        // hypothesis to reconstruct. "v is the tighter of {ceiling, rate
        // bound}" is exactly:
        //
        //     |v| == ceiling   OR   v is one rate-step from current
        //
        // which is unconditional, division-free, and says the thing K3 exists
        // to say: nothing OTHER than those two bounds may move `v`. A future
        // priority returning something arbitrarily below the ceiling satisfies
        // neither arm and fails the proof, which was the whole point of the
        // conditional half.
        //
        // Slack terms are float-representation bounds, as in K8: `fl(a+b)` is
        // within half an ulp, `limit x dt` is a rounded product, and the
        // smallest subnormal covers the denormal corner. The `1e-9 x dt` term
        // is the kernel's own tolerance carried into velocity space.
        let from_rest = cmd.current_velocity_mps.abs() <= STOP_EPSILON_MPS;
        let speeding_up = (from_rest
            || cmd.linear_velocity_mps.signum() == cmd.current_velocity_mps.signum())
            && cmd.linear_velocity_mps.abs() > cmd.current_velocity_mps.abs();
        let applicable_limit = if speeding_up {
            contract.max_accel_mps2
        } else {
            contract.max_brake_mps2
        };

        let ceiling_is_exact = linear.abs() == max;

        let step = applicable_limit * cmd.delta_time_s;
        let float_slack =
            (cmd.current_velocity_mps.abs() + step.abs()) * f64::EPSILON + f64::from_bits(1);
        let within_one_rate_step = (linear - cmd.current_velocity_mps).abs()
            <= step + 1e-9 * cmd.delta_time_s + float_slack;

        assert!(
            ceiling_is_exact || within_one_rate_step,
            "an over-ceiling command must return the ceiling exactly or a value \
             the rate bound explains — anything else means a priority lowered it"
        );
    }

    /// K8 (#1243) — the acceleration bound holds on EVERY executable return,
    /// including the over-ceiling path that used to skip it.
    ///
    /// This is the property #1243 exists for, and it is the direct analogue of
    /// K6/P-CAP one axis over: stated across the whole executable class rather
    /// than per branch, so it closes the defect class instead of the one path.
    ///
    /// DOMAIN CAVEAT, and it is load-bearing rather than a convenience: the
    /// property is asserted only where `|current| <= ceiling`. Outside that
    /// region the ceiling ITSELF forces a rate breach — a 39.9 m/s request from
    /// 40.0 m/s is a lawful -1.0 m/s^2, and clamping it to a 35.0 ceiling
    /// implies -50 m/s^2. No ordering of the two priorities avoids it; the
    /// vehicle is already outside the envelope and cannot be inside it one tick
    /// later without exceeding the brake limit. INVARIANT 8 ("envelope cap
    /// always wins over rate priority") decides it, and K6 independently
    /// requires the return to respect the ceiling, so the ceiling wins and the
    /// breach stands. Narrowing the domain here is therefore recording a real
    /// conflict between two enforced bounds — NOT excluding an inconvenient
    /// case to make a proof pass. The excluded region is exercised by a
    /// concrete EXPECTED-BUT-UNDESIRED fixture in
    /// `crates/kirra-core/tests/over_ceiling_accel_bound.rs`, which fails the
    /// day the behaviour changes.
    /// LANE: `deep-proofs` (weekly), for the same reason as K7 and with the
    /// same honesty about it — measured, not assumed. The quotient form failed
    /// in 186 s (SAT is fast); proving the velocity form has no counterexample
    /// is the UNSAT direction, and it was stopped without a verdict at 25 min
    /// and again at 55 min. Two budgets, no answer.
    ///
    /// A PATTERN, worth naming so the next relational harness does not
    /// rediscover it the slow way. Under the same solver model and the same
    /// symbolic contract:
    ///
    ///   K3  22 s | K6  45 s — assertion compares a returned value against a
    ///                          CONTRACT FIELD (`effective_max_speed_mps`).
    ///   K7  >23 min | K8 >55 min — assertion is RELATIONAL ARITHMETIC over two
    ///                          command fields (`atan` vs the rack; the step vs
    ///                          `limit x dt`). No verdict within any budget
    ///                          tried.
    ///
    /// The per-PR budget accommodates the first shape and not the second.
    ///
    /// 🔴 LANE: NONE. **K8 IS DEMOTED OUT OF THE PROOF TIER** (#1260), by the
    /// same rule and on the same diagnosed grounds as K7. It sits behind
    /// `outside-proof-envelope`, which NO lane enables; `ci/deep_harnesses.py`
    /// matches only `deep-proofs`, and `ci/test_deep_harnesses.py` pins that
    /// this harness stays out of the weekly matrix.
    ///
    /// The demotion is on a PHASE DIAGNOSIS, not on the timeouts above — K3
    /// proved that elapsed time alone justifies nothing (it was demoted on a
    /// 15-minute non-result and then discharged in 67 minutes). What was
    /// measured for K8, each phase separately:
    ///
    ///   * full harness — symex 0.34 s, 404 -> 36 VCCs, conversion 1.43 s,
    ///     THREE solves totalling 101 s, then a fourth propositional
    ///     conversion that never reaches a solver. 43 of 45 minutes vanish
    ///     there. Identical signature to K7.
    ///   * isolated to its ONE real assertion (373 of 374 properties dropped)
    ///     — the CNF shrinks by 0.54% (848,226 -> 843,634 clauses), so the 312
    ///     pointer-dereference checks are not the cost, the program encoding
    ///     is. Conversion then completes in 1.0 s, CBMC REACHES the solver,
    ///     and the solver returns nothing in 40 minutes.
    ///
    /// So K8 fails two ways at once — conversion-pathological in
    /// multi-property mode, and solver-hard underneath — and fixing the first
    /// only exposes the second. Additional budget is not a credible remedy.
    /// Contrast R2, which never stalls in conversion at all: it reaches the
    /// solver and stays there. That is a PHASE difference, not a size one —
    /// R2's CNF is in fact LARGER (1,221,711 clauses vs K8's 848,226), so the
    /// tempting "R2 is the small one" argument is false and is not why it
    /// stays. It stays because its solver search is ALIVE — 120,888 conflicts
    /// over a 43-minute solve, accumulating throughout — even though instance
    /// reduction decelerates sharply after the first six minutes. That mix
    /// keeps it a budget/solver question. R2 is deliberately NOT demoted with
    /// K8, and equally is NOT known to finish given more time.
    ///
    /// NO COVERAGE IS LOST. Its blocking per-PR gate is
    /// `k8_mirror_acceleration_bound_on_the_physical_dt_grid` — 2,880 points,
    /// and in the ACCELERATION space this proof had to abandon, so the grid is
    /// not a weaker copy of the proof; it asserts something the proof could
    /// not. Only the symbolic tier is gone.
    ///
    /// Regaining it is a MODELLING redesign, not CI tuning, and must preserve
    /// the actual safety claim rather than substitute an easier neighbour.
    #[cfg(feature = "outside-proof-envelope")]
    #[kani::proof]
    #[kani::stub(f64::powi, super::stub_powi)]
    #[kani::stub(f64::tan, super::stub_tan)]
    #[kani::stub(f64::atan, super::stub_atan)]
    fn k8_executable_return_respects_the_rate_bound() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.steering_angle_deg.is_finite());
        kani::assume(cmd.current_steering_angle_deg.is_finite());
        kani::assume(cmd.delta_time_s.is_finite() && cmd.delta_time_s > 0.0);

        let max = contract.effective_max_speed_mps();
        kani::assume(cmd.current_velocity_mps.abs() <= max);

        let action = validate_vehicle_command(&cmd, &contract);
        let Some((linear, _)) = executed_pair(&cmd, &action) else {
            return; // DenyBreach executes nothing.
        };

        let from_rest = cmd.current_velocity_mps.abs() <= STOP_EPSILON_MPS;
        let speeding_up = (from_rest
            || cmd.linear_velocity_mps.signum() == cmd.current_velocity_mps.signum())
            && cmd.linear_velocity_mps.abs() > cmd.current_velocity_mps.abs();
        let applicable_limit = if speeding_up {
            contract.max_accel_mps2
        } else {
            contract.max_brake_mps2
        };

        // STATED IN VELOCITY SPACE, NOT ACCELERATION SPACE — and that is a
        // correction forced by a counterexample, not a stylistic choice.
        //
        // The obvious form, `|v - current| / dt <= limit + 1e-9`, FAILS, and
        // Kani found why in 186 s. The counterexample is denormal:
        //
        //   current = -3.204150e-306    dt = 5.928788e-323 (subnormal)
        //   brake x dt = 3.557e-322  <  ulp(current) = 7.115e-322
        //
        // The kernel's intended step is SMALLER THAN ONE ULP of `current`, so
        // `fl(current + step)` lands a whole ulp away: |v - current| is
        // 6.324e-322, about 1.78x the intended step. Dividing by a subnormal
        // `dt` turns that sub-ulp rounding into an apparent 10.67 m/s^2 against
        // a 6.0 limit. The executed SPEED is 3.2e-306 m/s — physically zero.
        //
        // So the quotient form asserts something the implementation never
        // computes. The kernel divides only to DECIDE whether to clamp; the
        // value it returns is `current + limit x dt`, a subtraction away from
        // the quantity actually bounded. Dividing re-introduces an error
        // amplification (~ulp(current)/dt) that exists nowhere in the code.
        //
        // The velocity form is exact in the same arithmetic the kernel uses,
        // and it is EQUIVALENT for every dt where the division is
        // well-conditioned. Note what was NOT done: the dt domain is NOT
        // narrowed. Constraining dt to a physical grid would also make this
        // pass, and would have buried a real fact about the contract's
        // numerics instead of recording it. The acceleration-space form is
        // kept — as the concrete grid mirror over physical dt, where it is the
        // meaningful statement.
        //
        // The slack terms are float-representation bounds, not safety margin:
        // `fl(a+b)` is within half an ulp of the true sum and `limit x dt` is
        // itself a rounded product, so both operands contribute a relative
        // EPSILON; the smallest subnormal covers the denormal corner where
        // relative bounds degenerate. The `1e-9 x dt` term is the kernel's own
        // acceleration-space tolerance carried into velocity space — a command
        // whose implied rate is within tolerance does not trigger the clamp at
        // all, so the executed step can legitimately reach it.
        let step = applicable_limit * cmd.delta_time_s;
        let tolerance = 1e-9 * cmd.delta_time_s;
        let float_slack =
            (cmd.current_velocity_mps.abs() + step.abs()) * f64::EPSILON + f64::from_bits(1);
        assert!(
            (linear - cmd.current_velocity_mps).abs() <= step + tolerance + float_slack,
            "executed command steps further than the applicable rate limit allows"
        );
    }

    /// K3b (#1242) — COMPOSITION semantics, split out of K3.
    ///
    /// K3 owns the longitudinal invariant; this owns which variant reports it.
    /// Keeping them apart means a future change to how composed enforcement is
    /// REPORTED cannot silently look like a change to what the longitudinal
    /// bound IS.
    ///
    /// If both axes were corrected, the action must say so — dropping either
    /// correction from the report is how a consumer ends up applying one bound
    /// and not the other.
    #[kani::proof]
    #[kani::stub(f64::powi, super::stub_powi)]
    #[kani::stub(f64::tan, super::stub_tan)]
    #[kani::stub(f64::atan, super::stub_atan)]
    fn k3b_composition_reports_both_axes() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.steering_angle_deg.is_finite());
        kani::assume(cmd.current_steering_angle_deg.is_finite());
        kani::assume(cmd.delta_time_s.is_finite() && cmd.delta_time_s > 0.0);

        let action = validate_vehicle_command(&cmd, &contract);
        if let Some((linear, steering)) = executed_pair(&cmd, &action) {
            let linear_corrected = linear != cmd.linear_velocity_mps;
            let steering_corrected = steering != cmd.steering_angle_deg;

            // NON-VACUITY. The assertion below is an IMPLICATION, so it holds
            // trivially wherever its antecedent is unsatisfiable — and the
            // antecedent is value-dependent, which under the #1242 solver model
            // makes it a live concern rather than a theoretical one: `steering`
            // comes from a nondeterministic `atan` restricted by axiom A3, and
            // an A3 that over-restricted could collapse `steering_corrected` to
            // false and turn this proof into a proof of nothing. It does not
            // (both covers below are SATISFIED), but that is a measurement of
            // today's model, not a property of the harness — a later change to
            // A3, to `any_bounded_contract`, or to the kernel could remove it
            // silently. Kani reports an unsatisfiable cover as a FAILURE, so
            // these two lines are what make that silent.
            //
            // The SECOND cover is the load-bearing one, and it is not implied
            // by the first: the antecedent could be reachable only on paths
            // where the conclusion is forced for some unrelated reason, which
            // would satisfy cover 1 while still exercising nothing about the
            // composition rule. Requiring the antecedent to be reachable
            // TOGETHER WITH `ClampBoth` pins the case the property is about.
            kani::cover!(
                linear_corrected && steering_corrected,
                "K3b antecedent is satisfiable"
            );
            kani::cover!(
                linear_corrected
                    && steering_corrected
                    && matches!(action, EnforceAction::ClampBoth { .. }),
                "K3b antecedent is reachable together with the ClampBoth conclusion"
            );

            if linear_corrected && steering_corrected {
                assert!(
                    matches!(action, EnforceAction::ClampBoth { .. }),
                    "both axes corrected must report ClampBoth"
                );
            }
        }
    }

    /// K4 — Degraded HOLD (issue #70 (c)): from a stop, ANY finite command to
    /// re-initiate motion is denied with the specific code. Early-returns
    /// before the envelope check — decidable for all finite f64 pairs.
    #[kani::proof]
    fn k4_degraded_reinitiation_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.abs() <= STOP_EPSILON_MPS);
        kani::assume(cmd.linear_velocity_mps.abs() > STOP_EPSILON_MPS);

        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied)
        );
    }

    /// K5 — Degraded non-increasing speed (issue #70 (b)): while moving in an
    /// unchanged direction, any meaningful speed-magnitude increase is denied
    /// with the specific code, for all finite f64s in the region.
    #[kani::proof]
    fn k5_degraded_speed_increase_denied() {
        let cmd = any_command();
        let contract = any_bounded_contract();
        kani::assume(cmd.linear_velocity_mps.is_finite());
        kani::assume(cmd.current_velocity_mps.is_finite());
        // Moving (not the (c) hold case), same direction (not the (c′) reversal
        // case), and a meaningful magnitude increase (the (b) region).
        kani::assume(cmd.current_velocity_mps.abs() > STOP_EPSILON_MPS);
        kani::assume(cmd.linear_velocity_mps.signum() == cmd.current_velocity_mps.signum());
        kani::assume(cmd.linear_velocity_mps.abs() > cmd.current_velocity_mps.abs() + 1e-9);

        assert_eq!(
            enforce_degraded_decel_to_stop(&cmd, &contract),
            EnforceAction::DenyBreach(DenyCode::DegradedSpeedIncreaseDenied)
        );
    }
}

// ---------------------------------------------------------------------------
// #1242 — executable-output bounds. See docs/safety/TALISMAN_CHANGE_PLAN_1242.md.
//
// HONEST SCOPE. The governing invariant is "no intermediate priority may
// finalize an executable command". That is a statement about CONTROL FLOW, and
// Kani cannot assert which `return` executed — so P-COMPOSE is not directly
// expressible. What IS expressible is its observable SHADOW: every executable
// return must satisfy every bound the pipeline is supposed to have applied. Any
// priority that finalizes early skips some of those bounds and is caught here.
//
// Both properties below are deliberately `tan`-free, so neither inherits the
// transcendental exclusion recorded in GOVERNOR_INTEGRITY_EVIDENCE.md §2. The
// numeric lateral-envelope property is NOT proved here and must not be claimed
// as proved; it is discharged by concrete grid instead.
//
// RED against the pre-fix kernel: the Priority-2 speed-cap branch returns
// `ClampLinear` without ever reaching P5a or P6, so it violates K7 outright
// (measured: a 200 deg demand passes through a 35 deg rack untouched).
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Solver model (#1242) — `powi`, and `tan`/`atan` as an over-approximating pair.
//
// Everything below lives in the PROOF crate. The talisman is not modified for
// the benefit of the prover: no `#[cfg(kani)]` appears in it, and the only
// kernel change #1242 makes is the Priority-2 accumulate fix itself.
//
// WHY THIS EXISTS. `f64::tan` and `f64::atan` are foreign C calls; CBMC cannot
// execute them, and Kani fails any harness in which one is *reachable*. Before
// #1242 the Priority-2 speed ceiling returned early, so the over-ceiling
// commands K3 quantifies over never reached the P6 bicycle model and the
// question never arose. Making P2 accumulate — the whole point of the fix —
// puts P6 on those paths, and K3/K3b/K6/K7 all began failing with
// `call to foreign "C" function 'tan' is not currently supported`. That is a
// TOOLING boundary, not a counterexample: no assertion was violated.
//
// WHAT THIS IS. Not an implementation of tan/atan. Each stub returns a
// NONDETERMINISTIC value constrained only by postconditions that are theorems
// about the real functions. The proofs are therefore discharged for EVERY
// implementation satisfying them — strictly stronger than a proof about one
// libm, and independent of the solver's inability to reason about
// transcendentals.
//
// THE AXIOMS. Each is a fact about the real functions. Nothing here is assumed
// for convenience; if any were false the proofs would be void, so they are
// stated individually to be checked by review:
//
//   A1  tan(x) is finite for every finite x. (No finite double is exactly
//       pi/2, so the pole is unreachable in f64.)
//   A2  |atan(y)| <= pi/2 for every y — the principal branch's range.
//   A3  atan inverts tan and both are increasing on (-pi/2, pi/2), so
//         |y| <= |tan(x)|  and  |x| <= pi/2   imply   |atan(y)| <= |x|.
//
// A3 is the one K7 needs, and it relates the two calls, so the model must be a
// PAIR rather than two independent nondet functions: P6 asks tan for the
// current angle, then asks atan for the back-solved bound, and the rack limit
// survives only because the second answer is no larger than the first
// question. `LAST_TAN` carries that link. CBMC runs one harness per process
// with statics at their initializers, so the recording is deterministic; the
// hypothesis is CHECKED before A3 is applied, so a call that does not match the
// recorded pair falls back to A2 alone and the model stays sound.
//
// AND `powi`. `v.powi(2)` lowers to the `llvm.powi.f64` intrinsic, which CBMC
// models as the UNINTERPRETED builtin `__builtin_powi`: `v2` becomes an
// arbitrary double, including NaN. One opaque value is enough to make the P6
// entry guard `v2 > 1e-6` undecidable, so `tan` is reachable even on paths
// where the enforced speed is orders of magnitude below the threshold. That
// was measured, on a throwaway harness pinning the ceiling at 0.0005 m/s so
// `v2 = 2.5e-7`, two and a half orders below the guard: it still failed the
// `tan` check, and passed the moment `powi` was modelled. Worth recording
// because the same harness, read without that explanation, looks like evidence
// AGAINST the reachability diagnosis — it was a broken instrument, not a
// refutation. `stub_powi` is an EXACT model, not an over-approximation: squaring
// by multiplication. That it agrees with `powi(2)` bit-for-bit is measured, not
// asserted — `crates/kirra-core/tests/powi_square_bit_identity.rs` walks
// subnormals, the exponent extremes, the overflow/flush-to-zero boundaries and
// the operating speed range, and a toolchain that broke the identity would red
// that test rather than silently weaken these proofs. The `assert!` keeps the
// model honest about its scope: any exponent other than 2 fails the proof
// instead of being modelled wrongly.
// ---------------------------------------------------------------------------

#[cfg(kani)]
pub(crate) fn stub_powi(x: f64, n: i32) -> f64 {
    assert!(n == 2, "the powi model covers exponent 2 only");
    x * x
}

/// `(|x|, |tan(x)|)` from the most recent [`stub_tan`] call, for axiom A3.
#[cfg(kani)]
static mut LAST_TAN: Option<(f64, f64)> = None;

#[cfg(kani)]
pub(crate) fn stub_tan(x: f64) -> f64 {
    let r: f64 = kani::any();
    kani::assume(r.is_finite()); // A1
    if x.is_finite() {
        unsafe { LAST_TAN = Some((x.abs(), r.abs())) };
    }
    r
}

#[cfg(kani)]
pub(crate) fn stub_atan(y: f64) -> f64 {
    let r: f64 = kani::any();
    kani::assume(r.is_finite() && r.abs() <= core::f64::consts::FRAC_PI_2); // A2

    // A3, applied only when its hypothesis is discharged by the recorded pair.
    if let Some((x_abs, tan_abs)) = unsafe { LAST_TAN } {
        if x_abs <= core::f64::consts::FRAC_PI_2 && y.abs() <= tan_abs {
            kani::assume(r.abs() <= x_abs);
        }
    }
    r
}

#[cfg(any(kani, test))]
fn executed_pair(cmd: &ProposedVehicleCommand, action: &EnforceAction) -> Option<(f64, f64)> {
    // The apply mapping, inlined rather than imported: this crate `#[path]`-
    // includes only the contract source, and duplicating three lines here is
    // preferable to pulling the whole sim module into the proof tree.
    match action {
        EnforceAction::Allow => Some((cmd.linear_velocity_mps, cmd.steering_angle_deg)),
        EnforceAction::ClampLinear(v) => Some((*v, cmd.steering_angle_deg)),
        EnforceAction::ClampSteering(d) => Some((cmd.linear_velocity_mps, *d)),
        EnforceAction::ClampBoth { linear, steering } => Some((*linear, *steering)),
        // Nothing is executed.
        EnforceAction::DenyBreach(_) => None,
    }
}

/// K6 (P-CAP) — every executable return respects the effective speed ceiling.
///
/// Guards against the fix shape that trades one breach for another: routing the
/// speed cap through the pipeline without making it a persistent magnitude
/// ceiling lets the accel priority overwrite it UPWARD from the raw command.
#[cfg(kani)]
#[kani::proof]
#[kani::stub(f64::powi, stub_powi)]
#[kani::stub(f64::tan, stub_tan)]
#[kani::stub(f64::atan, stub_atan)]
fn k6_executable_return_respects_the_speed_ceiling() {
    let contract = any_bounded_contract();
    let cmd = any_command();
    let action = validate_vehicle_command(&cmd, &contract);
    if let Some((linear, _)) = executed_pair(&cmd, &action) {
        // Finite by construction: Priority 0 denies every non-finite input, so
        // an executable return cannot carry one.
        assert!(linear.is_finite());
        assert!(linear.abs() <= contract.effective_max_speed_mps() + 1e-9);
    }
}

/// K7 (P-RACK) — every executable return respects the absolute steering limit.
///
/// This is the sharpest tan-free consequence of the #1242 defect: the speed-cap
/// branch returns before P5a, so the RAW steering demand is executed. Unlike the
/// lateral envelope, the rack limit is a physical hard stop — a demand beyond it
/// is not merely aggressive, it is unachievable by the mechanism.
///
/// 🔴 LANE: NONE. **K7 IS DEMOTED OUT OF THE PROOF TIER** (#1268). It is behind
/// `outside-proof-envelope`, a feature NO CI lane enables — not per-PR, not the
/// weekly deep lane. `ci/deep_harnesses.py` matches only `deep-proofs`, so this
/// harness cannot re-enter the weekly matrix by accident, and
/// `ci/test_deep_harnesses.py` pins that it does not.
///
/// The earlier text here said the demotion was provisional and that a bigger
/// budget might suffice. **That was wrong, and the measurement that refuted it
/// is worth keeping.** Isolating K7's two real assertions with CBMC `--property`
/// — discarding 368 of its 370 properties — shrank the formula by 0.5%
/// (738,811 → 735,393 clauses). The 318 pointer-dereference checks are not the
/// cost; the *program* encoding is. And in that isolated configuration CBMC does
/// reach the solver and still returns nothing after 30 minutes. So K7 fails two
/// ways at once: multi-property mode stalls in propositional conversion before a
/// solver is invoked, and the underlying instance is solver-hard regardless.
/// Fixing the first would only expose the second.
///
/// ~735k clauses for an 8.8k-step program expression is IEEE-754 bit-blasting.
/// K7 relates two separately bit-blasted f64 computation chains, which is
/// precisely the shape already recorded as never finishing. Additional runtime
/// budget is therefore not a credible remediation, and this harness must not be
/// re-enabled on the theory that a larger runner will do it.
///
/// The source is retained ONLY as the starting point for a future redesign,
/// which is a modelling task and not CI tuning. Useful directions: prove over a
/// reduced mathematical model; introduce analytically justified bounds around
/// the floating-point operations; restructure P-RACK so it does not directly
/// compare two bit-blasted f64 chains. Any such attempt must preserve the ACTUAL
/// safety claim — #1243 refused to weaken K3 to a cheaper `|v| <= ceiling`
/// precisely because that duplicates K6 and would let the safety case read as
/// though nothing had changed. Same discipline applies here.
///
/// NO COVERAGE IS LOST BY THIS DEMOTION. P-RACK's enforcement is, and remains,
/// `k6_k7_mirror_exhaustive_grid_over_the_executable_return` — 306,180 points
/// swept along ceiling × ODD cap × commanded speed × current speed × steering
/// demand × current steering × wheelbase × lateral cap, asserting this exact
/// bound, BLOCKING on every PR, and red against the pre-fix kernel on the
/// 200 deg demand. Only the symbolic tier for P-RACK is gone.
#[cfg(all(kani, feature = "outside-proof-envelope"))]
#[kani::proof]
#[kani::stub(f64::powi, stub_powi)]
#[kani::stub(f64::tan, stub_tan)]
#[kani::stub(f64::atan, stub_atan)]
fn k7_executable_return_respects_the_rack_limit() {
    let contract = any_bounded_contract();
    let cmd = any_command();
    let action = validate_vehicle_command(&cmd, &contract);
    if let Some((_, steering)) = executed_pair(&cmd, &action) {
        assert!(steering.is_finite());
        assert!(steering.abs() <= contract.max_steering_deg + 1e-9);
    }
}

// ---------------------------------------------------------------------------
// Concrete mirrors under plain `cargo test`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod mirrors {
    use super::*;

    // -----------------------------------------------------------------------
    // #1242 mirrors — GREEN as of the talisman fix. K7 was red and `#[ignore]`d
    // until Priority 2 stopped finalizing; the un-ignore is closure evidence.
    // -----------------------------------------------------------------------

    /// Concrete instance of K7 (P-RACK). The speed-cap branch returns before
    /// P5a, so the raw steering demand is what gets executed.
    #[test]
    fn k7_mirror_rack_limit_holds_on_the_speed_cap_branch() {
        let c = contract(5.225, None);
        for steer in [50.0_f64, 80.0, 200.0] {
            let cmd = ProposedVehicleCommand {
                linear_velocity_mps: 5.4,
                current_velocity_mps: 5.4,
                delta_time_s: 0.1,
                steering_angle_deg: steer,
                current_steering_angle_deg: steer,
            };
            let action = validate_vehicle_command(&cmd, &c);
            if let Some((_, steering)) = executed_pair(&cmd, &action) {
                assert!(
                    steering.abs() <= c.max_steering_deg + 1e-9,
                    "demand {steer} deg executed as {steering} deg against a {} deg rack; action was {action:?}",
                    c.max_steering_deg
                );
            }
        }
    }

    /// Concrete instance of K6 (P-CAP) on the shape the NAIVE fix would break:
    /// apply the cap once, then let the accel priority overwrite `v` from the raw
    /// command and the executed speed exceeds the ceiling. Passes today (the
    /// early return makes the cap final) and must keep passing after the fix — it
    /// guards the FIX SHAPE, not the current defect.
    #[test]
    fn k6_mirror_speed_ceiling_survives_the_accel_priority() {
        let c = contract(5.225, None);
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 20.0,
            current_velocity_mps: 5.0,
            delta_time_s: 0.1,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 5.0,
        };
        let action = validate_vehicle_command(&cmd, &c);
        if let Some((linear, _)) = executed_pair(&cmd, &action) {
            assert!(
                linear.abs() <= c.effective_max_speed_mps() + 1e-9,
                "executed {linear} m/s against a {} m/s ceiling; action was {action:?} - the accel priority must not raise the capped speed",
                c.effective_max_speed_mps()
            );
        }
    }

    /// EXHAUSTIVE grid mirror of K6 (P-CAP) and K7 (P-RACK) — the R2 pattern.
    ///
    /// The symbolic K6/K7 harnesses quantify over a bounded contract and five
    /// symbolic doubles; this walks a concrete grid swept along every axis the
    /// two properties can depend on and asserts the SAME two bounds on the
    /// executable pair. It exists for two reasons:
    ///
    ///   1. It is the per-PR gate for whichever of K6/K7 is too expensive for
    ///      the per-PR solver budget — the established repo pattern (R2's
    ///      full-lattice monotonicity is gated exactly this way).
    ///   2. It is blind in the opposite direction from the proofs. The grid can
    ///      miss a branch nobody sampled; the proofs cannot see whether the P6
    ///      arithmetic is numerically right, because under the #1242 solver
    ///      model they never evaluate a real `tan`. Neither substitutes for the
    ///      other, and this comment exists so that is not quietly forgotten.
    ///
    /// Axes: effective ceiling (max_speed × ODD cap, so the `min` selection is
    /// exercised both ways) × commanded speed relative to that ceiling ×
    /// current speed (accelerating, braking, from rest, reversing) × steering
    /// demand from inside the rack to far beyond it × steering rate demand ×
    /// wheelbase × lateral-accel cap. The last two move where P6 binds relative
    /// to P5a, which is the interaction the four-angle regression could not see.
    #[test]
    fn k6_k7_mirror_exhaustive_grid_over_the_executable_return() {
        let speeds = [0.5_f64, 2.0, 5.225, 13.4, 35.0];
        let caps = [None, Some(0.4_f64), Some(3.0), Some(100.0)];
        let commanded = [-40.0_f64, -5.0, -0.4, 0.0, 0.3, 1.0, 5.3, 14.0, 40.0];
        let currents = [-20.0_f64, -0.01, 0.0, 0.02, 4.9, 12.0, 36.0];
        let steers = [0.0_f64, 1.0, 12.0, 34.9, 35.0, 35.1, 50.0, 89.0, 200.0];
        let cur_steers = [-35.0_f64, 0.0, 34.0];
        let wheelbases = [0.5_f64, 2.8, 6.0];
        let lat_caps = [0.5_f64, 3.0, 12.0];

        let mut checked = 0u64;
        let mut executed = 0u64;
        for &max_speed in &speeds {
            for &cap in &caps {
                for &wheelbase_m in &wheelbases {
                    for &max_lateral_accel_mps2 in &lat_caps {
                        let c = VehicleKinematicsContract {
                            wheelbase_m,
                            max_lateral_accel_mps2,
                            ..contract(max_speed, cap)
                        };
                        let ceiling = c.effective_max_speed_mps();
                        for &linear_velocity_mps in &commanded {
                            for &current_velocity_mps in &currents {
                                for &steering_angle_deg in &steers {
                                    for &current_steering_angle_deg in &cur_steers {
                                        let command = ProposedVehicleCommand {
                                            linear_velocity_mps,
                                            current_velocity_mps,
                                            delta_time_s: 0.1,
                                            steering_angle_deg,
                                            current_steering_angle_deg,
                                        };
                                        let action = validate_vehicle_command(&command, &c);
                                        checked += 1;
                                        let Some((linear, steering)) =
                                            executed_pair(&command, &action)
                                        else {
                                            continue;
                                        };
                                        executed += 1;
                                        assert!(
                                            linear.is_finite() && steering.is_finite(),
                                            "non-finite executable pair ({linear}, {steering}) \
                                             for {command:?} under {c:?}; action {action:?}"
                                        );
                                        // P-CAP.
                                        assert!(
                                            linear.abs() <= ceiling + 1e-9,
                                            "executed {linear} m/s against a {ceiling} m/s \
                                             ceiling for {command:?}; action {action:?}"
                                        );
                                        // P-RACK.
                                        assert!(
                                            steering.abs() <= c.max_steering_deg + 1e-9,
                                            "executed {steering} deg against a {} deg rack \
                                             for {command:?}; action {action:?}",
                                            c.max_steering_deg
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Non-vacuity: the grid must actually reach executable returns, and in
        // bulk. A future guard that denied everything would otherwise turn this
        // exhaustive sweep into an exhaustive sweep of nothing.
        assert_eq!(
            checked, 306_180,
            "grid size changed; update the expectation"
        );
        assert!(
            executed > checked / 2,
            "only {executed} of {checked} grid points returned an executable pair"
        );
    }

    /// K8's ACCELERATION-SPACE form, on the concrete grid over physical dt.
    ///
    /// The symbolic K8 is stated in velocity space, because the quotient form
    /// is corrupted at subnormal `dt` by an error amplification the kernel
    /// never performs (see the harness). That correction is right, but it
    /// leaves the property people actually care about — "the executed command
    /// does not imply an acceleration outside the limit" — unasserted in the
    /// form it is written in. This closes that gap where the division is
    /// well-conditioned: a physical tick range, from 1 ms to 1 s.
    ///
    /// The two are blind in opposite directions, which is the point of keeping
    /// both. The symbolic proof covers every `dt` including the absurd ones but
    /// bounds a step rather than a rate; the grid bounds the rate directly but
    /// only where it was sampled.
    #[test]
    fn k8_mirror_acceleration_bound_on_the_physical_dt_grid() {
        let mut checked = 0u64;
        let mut executed = 0u64;
        for (max_speed, cap) in [
            (35.0_f64, None),
            (35.0, Some(22.35)),
            (5.225, None),
            (0.5, None),
        ] {
            let c = contract(max_speed, cap);
            let ceiling = c.effective_max_speed_mps();
            for dt_ms in [1_u32, 2, 5, 10, 20, 50, 100, 200, 500, 1000] {
                let dt = f64::from(dt_ms) / 1000.0;
                for cur_pct in [-100_i32, -60, -5, 0, 1, 5, 60, 100] {
                    // Only the K8 domain: |current| <= ceiling. Above it the
                    // ceiling forces a breach by design (invariant 8).
                    let current = ceiling * f64::from(cur_pct) / 100.0;
                    for req in [
                        -1000.0_f64,
                        -50.0,
                        -1.0,
                        -0.01,
                        0.0,
                        0.01,
                        1.0,
                        50.0,
                        1000.0,
                    ] {
                        let cmd = ProposedVehicleCommand {
                            linear_velocity_mps: req,
                            current_velocity_mps: current,
                            delta_time_s: dt,
                            steering_angle_deg: 0.0,
                            current_steering_angle_deg: 0.0,
                        };
                        let action = validate_vehicle_command(&cmd, &c);
                        checked += 1;
                        let Some((linear, _)) = executed_pair(&cmd, &action) else {
                            continue;
                        };
                        executed += 1;

                        let from_rest = current.abs() <= STOP_EPSILON_MPS;
                        let speeding_up = (from_rest || req.signum() == current.signum())
                            && req.abs() > current.abs();
                        let limit = if speeding_up {
                            c.max_accel_mps2
                        } else {
                            c.max_brake_mps2
                        };
                        let implied = (linear - current).abs() / dt;
                        assert!(
                            implied <= limit + 1e-9,
                            "executed {linear} from {current} over {dt} s implies \
                             {implied} m/s^2 against a {limit} limit; action {action:?}"
                        );
                    }
                }
            }
        }
        assert_eq!(checked, 2_880, "grid size changed; update the expectation");
        assert!(
            executed > checked / 2,
            "only {executed} of {checked} grid points returned an executable pair"
        );
    }

    fn contract(max_speed: f64, cap: Option<f64>) -> VehicleKinematicsContract {
        VehicleKinematicsContract {
            max_speed_mps: max_speed,
            max_accel_mps2: 3.0,
            max_brake_mps2: 6.0,
            max_steering_deg: 35.0,
            max_steering_rate_deg_s: 30.0,
            min_follow_distance_m: 5.0,
            max_lateral_accel_mps2: 3.0,
            wheelbase_m: 2.8,
            width_m: 2.0,
            length_m: 4.6,
            overhang_front_m: 0.9,
            overhang_rear_m: 0.9,
            odd_speed_cap_mps: cap,
        }
    }

    fn cmd(linear: f64, current: f64, dt: f64) -> ProposedVehicleCommand {
        ProposedVehicleCommand {
            linear_velocity_mps: linear,
            current_velocity_mps: current,
            delta_time_s: dt,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        }
    }

    #[test]
    fn k1_mirror_every_nonfinite_field_denied() {
        let c = contract(30.0, None);
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut probe = cmd(5.0, 5.0, 0.1);
            probe.linear_velocity_mps = bad;
            assert_eq!(
                validate_vehicle_command(&probe, &c),
                EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity)
            );
            for field in 0..4 {
                let mut probe = cmd(5.0, 5.0, 0.1);
                match field {
                    0 => probe.current_velocity_mps = bad,
                    1 => probe.steering_angle_deg = bad,
                    2 => probe.current_steering_angle_deg = bad,
                    _ => probe.delta_time_s = bad,
                }
                assert!(matches!(
                    validate_vehicle_command(&probe, &c),
                    EnforceAction::DenyBreach(_)
                ));
            }
        }
    }

    #[test]
    fn k2_mirror_nonpositive_dt() {
        let c = contract(30.0, None);
        for dt in [0.0, -0.0, -1.0, f64::MIN] {
            assert_eq!(
                validate_vehicle_command(&cmd(5.0, 5.0, dt), &c),
                EnforceAction::DenyBreach(DenyCode::InvalidTimeDelta)
            );
        }
    }

    /// K3 mirror, restated for #1243 in lock step with the harness.
    ///
    /// The old form drove every case from `current = 0.0` over `dt = 0.1`,
    /// which implies a rate in the hundreds of m/s^2 — so once #1243 let the
    /// rate bound run on this path, EVERY case in it became rate-bound rather
    /// than ceiling-bound and the "exactly the ceiling" assertion was false
    /// throughout. That is the mirror doing its job: it went red on the same
    /// semantic change the harness did.
    ///
    /// It now covers both halves of the restated property, because covering
    /// only the first would leave the interesting case unmirrored.
    #[test]
    fn k3_mirror_ceiling_exact_when_only_the_ceiling_binds() {
        // Cap tighter than physical max; cap absent; cap present but looser.
        for (c, max) in [
            (contract(30.0, Some(22.35)), 22.35),
            (contract(30.0, None), 30.0),
            (contract(10.0, Some(50.0)), 10.0), // cap present but looser
        ] {
            for sign in [1.0_f64, -1.0] {
                // Approach from just inside the ceiling over a long enough step
                // that the accel bound (3.0 m/s^2 in `contract`) is NOT the
                // tighter constraint: 5.1 m/s of delta over 2.0 s is 2.55.
                let current = sign * (max - 0.1);
                let request = sign * (max + 5.0);
                match validate_vehicle_command(&cmd(request, current, 2.0), &c) {
                    EnforceAction::ClampLinear(v) => {
                        assert_eq!(v.abs(), max, "ceiling is exact when it alone binds");
                        assert_eq!(v.signum(), sign, "direction preserved");
                    }
                    other => panic!("expected ClampLinear, got {other:?}"),
                }
            }
        }
    }

    /// The other half: when the rate bound is tighter, it wins and the executed
    /// magnitude is BELOW the ceiling. This is the #1243 defect case — before
    /// the fix it returned the ceiling and implied 300 m/s^2.
    #[test]
    fn k3_mirror_rate_bound_wins_when_it_is_tighter() {
        let c = contract(35.0, None); // accel 3.0, brake 6.0
        for sign in [1.0_f64, -1.0] {
            let action = validate_vehicle_command(&cmd(sign * 50.0, sign * 5.0, 0.1), &c);
            let EnforceAction::ClampLinear(v) = action else {
                panic!("expected ClampLinear, got {action:?}")
            };
            // 5.0 + 3.0 x 0.1 = 5.3, far below the 35.0 ceiling.
            assert_eq!(v, sign * 5.3);
            assert!(v.abs() < c.effective_max_speed_mps());
            let implied = (v - sign * 5.0).abs() / 0.1;
            assert!(
                implied <= c.max_accel_mps2 + 1e-9,
                "{implied} m/s^2 against a {} limit",
                c.max_accel_mps2
            );
        }
    }

    #[test]
    fn k4_mirror_reinitiation_denied() {
        let c = contract(30.0, None);
        for current in [0.0, 0.04, -0.05] {
            for proposed in [0.06, 1.0, -2.0] {
                assert_eq!(
                    enforce_degraded_decel_to_stop(&cmd(proposed, current, 0.1), &c),
                    EnforceAction::DenyBreach(DenyCode::DegradedReinitiationDenied)
                );
            }
        }
    }

    #[test]
    fn k5_mirror_speed_increase_denied() {
        let c = contract(30.0, None);
        for (proposed, current) in [(6.0, 5.0), (-6.0, -5.0), (0.2, 0.1)] {
            assert_eq!(
                enforce_degraded_decel_to_stop(&cmd(proposed, current, 0.1), &c),
                EnforceAction::DenyBreach(DenyCode::DegradedSpeedIncreaseDenied)
            );
        }
    }
}
