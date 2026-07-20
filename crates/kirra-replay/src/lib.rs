//! EP-19 — deterministic replay: feed a captured session back through the REAL
//! gateway checker and assert BIT-IDENTICAL verdicts.
//!
//! The whole point is incident reconstruction with zero reimplementation:
//!
//!   * the replayed verdict is computed by the SAME functions the deployed
//!     gateway ran (`validate_vehicle_command` at Nominal,
//!     `enforce_degraded_decel_to_stop` at Degraded, over the SAME per-class
//!     contract profiles `contract_for` / `mrc_fallback_for`);
//!   * the recomputed verdict is mapped back to record shape by the SAME
//!     `kirra_core::capture::record_from_verdict` the deployed emit site uses —
//!     the comparison can never drift from the deployed mapping;
//!   * equality is BIT-identical: `f64::to_bits` on `safe_value`, exact string
//!     equality on outcome/deny-code/posture semantics.
//!
//! Honest scope (what makes replay DETERMINISTIC here): a `CommandGateway`
//! record carries the checker's COMPLETE input (the five command fields; the
//! contract is class-derived; `dt` rides in the command, so no wall clock
//! enters the verdict). Records that do NOT carry their complete inputs are
//! CLASSIFIED, never guessed:
//!
//!   * `SlowLoopTrajectory` records carry a bounded O(1) summary (endpoints +
//!     counts), not the full trajectory/objects — `NotReplayable`;
//!   * `derate_enabled = true` records composed a perception cap the schema
//!     does not carry — `NotReplayable` (context incomplete) rather than a
//!     silently-wrong recompute;
//!   * `LOCKED_OUT` records cannot exist from the gateway emit site (the
//!     posture gate short-circuits first) — `NotReplayable` (foreign record);
//!   * a NaN/Inf-INPUT denial does not round-trip the JSONL schema
//!     (`serde_json` writes non-finite floats as `null`, which fails f64
//!     deserialization → a loud `parse_errors` entry, never a silent skip).
//!     The NaN fail-closed guarantee itself is machine-checked for every f64
//!     bit pattern by the EP-15 Kani proof K1 — stronger than replay could be.

use std::str::FromStr;

use kirra_capture_schema::{CaptureOutcome, CaptureRecord, CaptureSource};
use kirra_core::capture::record_from_verdict;
use kirra_verifier::gateway::contract_profiles::{contract_for, mrc_fallback_for, VehicleClass};
use kirra_verifier::gateway::kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, ProposedVehicleCommand,
    VehicleKinematicsContract,
};
use kirra_verifier::verifier::FleetPosture;

/// W4 (#1043): the cross-platform (x86 replay/CI vs aarch64 Orin/QNX) libm
/// spread on a `tan`/`atan`-derived P6 steering clamp. A correctly-rounded IEEE
/// result would be bit-identical; real libm implementations differ by ~1–2 ulps.
/// 64 ulps is generous headroom for that spread while remaining astronomically
/// tighter than any physically-meaningful steering tamper (changing a clamp by
/// even 1e-6° is ~10^10 ulps), so the tamper non-vacuity is preserved.
const MAX_PLATFORM_APPROX_ULPS: u64 = 64;

/// The bit-comparable projection of a verdict record (the fields the emit
/// mapping derives from the verdict; timing/join fields excluded by design).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictImage {
    pub outcome: CaptureOutcome,
    pub deny_code: Option<String>,
    /// `f64::to_bits` of `safe_value` — BIT-identical, not approximate.
    pub safe_value_bits: Option<u64>,
    pub mrc: bool,
}

impl VerdictImage {
    fn of(rec: &CaptureRecord) -> Self {
        Self {
            outcome: rec.outcome,
            deny_code: rec.deny_code.clone(),
            safe_value_bits: rec.safe_value.map(f64::to_bits),
            mrc: rec.mrc,
        }
    }
}

/// One record's replay result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayResult {
    /// The recomputed verdict is bit-identical to the recorded one.
    Identical,
    /// THE ALARM: the same inputs through the same checker produced a
    /// different verdict than the record claims.
    Divergent {
        recorded: VerdictImage,
        recomputed: VerdictImage,
    },
    /// W4 (#1043): the recomputed verdict matches on every field EXCEPT the exact
    /// bits of a P6 lateral-accel clamp's steering value, which is `tan`/`atan`-
    /// derived and therefore NOT bit-portable across x86 (CI/replay) and aarch64
    /// (Orin/QNX) libm — the two values agree to within `MAX_PLATFORM_APPROX_ULPS`.
    /// Classified platform-approximate rather than raised as a FALSE incident
    /// divergence. (The EP-15 Kani proofs honestly EXCLUDE this transcendental
    /// path for the same non-portability reason.) Everything else — a different
    /// outcome / deny-code / mrc, a non-P6 clamp value, or a value more than a few
    /// ulps off — stays [`Divergent`](Self::Divergent), so tamper detection holds.
    PlatformApproximate {
        recorded: VerdictImage,
        recomputed: VerdictImage,
        /// ulp distance between the recorded and recomputed steering value.
        ulps: u64,
    },
    /// The record does not carry its complete checker inputs; classified,
    /// never guessed. The reason says exactly what is missing.
    NotReplayable { reason: String },
}

/// Replay ONE record through the real checker.
#[must_use]
pub fn replay_record(rec: &CaptureRecord, class: VehicleClass) -> ReplayResult {
    if rec.source != CaptureSource::CommandGateway {
        return ReplayResult::NotReplayable {
            reason: "slow-loop trajectory record carries a bounded summary, not the full \
                     trajectory/objects inputs"
                .into(),
        };
    }
    if rec.derate_enabled {
        return ReplayResult::NotReplayable {
            reason: "record was emitted with the perception-derate cap enabled; the cap value \
                     is not in the capture schema, so the Nominal contract cannot be \
                     reconstructed bit-identically"
                .into(),
        };
    }
    let Some(p) = rec.proposed else {
        return ReplayResult::NotReplayable {
            reason: "command-gateway record without a proposed-command snapshot".into(),
        };
    };
    let posture = match rec.posture.as_str() {
        "NOMINAL" => FleetPosture::Nominal,
        "DEGRADED" => FleetPosture::Degraded,
        "LOCKED_OUT" => {
            return ReplayResult::NotReplayable {
                reason: "LOCKED_OUT records cannot originate from the gateway emit site (the \
                         posture gate short-circuits before the verdict)"
                    .into(),
            };
        }
        other => {
            return ReplayResult::NotReplayable {
                reason: format!("unknown posture token {other:?}"),
            };
        }
    };

    let cmd = ProposedVehicleCommand {
        linear_velocity_mps: p.linear_velocity_mps,
        current_velocity_mps: p.current_velocity_mps,
        delta_time_s: p.delta_time_s,
        steering_angle_deg: p.steering_angle_deg,
        current_steering_angle_deg: p.current_steering_angle_deg,
    };

    // The SAME verdict computation the deployed gateway arm runs.
    let verdict = match posture {
        FleetPosture::Nominal => validate_vehicle_command(&cmd, &contract_for(class)),
        FleetPosture::Degraded => enforce_degraded_decel_to_stop(&cmd, &mrc_fallback_for(class)),
        FleetPosture::LockedOut => unreachable!("classified above"),
    };

    // The SAME verdict→record mapping the deployed emit site uses.
    let recomputed_rec = record_from_verdict(
        rec.decision_seq,
        rec.t_wall_ms,
        &verdict,
        posture,
        &cmd,
        rec.derate_enabled,
    );

    let recorded = VerdictImage::of(rec);
    let recomputed = VerdictImage::of(&recomputed_rec);
    if recorded == recomputed {
        return ReplayResult::Identical;
    }
    // W4 (#1043): before declaring a divergence, check whether the ONLY difference
    // is the value of a P6 lateral-accel steering clamp — which is `tan`/`atan`-
    // derived and not bit-portable across libm/arch. If so, and the values agree
    // within a few ulps, it is a platform-approximate match, not an incident.
    if let Some(ulps) = p6_clamp_platform_approx(&recorded, &recomputed, &cmd, class, posture) {
        return ReplayResult::PlatformApproximate {
            recorded,
            recomputed,
            ulps,
        };
    }
    ReplayResult::Divergent {
        recorded,
        recomputed,
    }
}

/// W4 (#1043): `Some(ulps)` iff `recorded`/`recomputed` differ ONLY in a P6
/// lateral-accel steering-clamp value that is within `MAX_PLATFORM_APPROX_ULPS` —
/// the tan/atan-derived, non-bit-portable case. `None` (→ a real divergence) for
/// any different outcome/deny-code/mrc, a non-`ClampSteering` verdict (only
/// `ClampSteering` carries the atan value in `safe_value`; `ClampBoth` records the
/// deterministic longitudinal clamp), a value more than a few ulps off, or a clamp
/// P6 was not the binding constraint for.
fn p6_clamp_platform_approx(
    recorded: &VerdictImage,
    recomputed: &VerdictImage,
    cmd: &ProposedVehicleCommand,
    class: VehicleClass,
    posture: FleetPosture,
) -> Option<u64> {
    // Only a ClampSteering verdict exposes the atan value in `safe_value`; both
    // sides must agree it is a steering clamp, and nothing else may differ.
    if recorded.outcome != CaptureOutcome::ClampSteering
        || recomputed.outcome != CaptureOutcome::ClampSteering
        || recorded.deny_code != recomputed.deny_code
        || recorded.mrc != recomputed.mrc
    {
        return None;
    }
    let (Some(a_bits), Some(b_bits)) = (recorded.safe_value_bits, recomputed.safe_value_bits)
    else {
        return None;
    };
    // Was the tan/atan P6 arm the BINDING steering constraint here? (A P5a hard
    // limit / P5b rate ceiling also yields ClampSteering, but tan/atan-free and
    // fully deterministic — those stay bit-compared.)
    if !p6_was_the_binding_steering_constraint(cmd, class, posture) {
        return None;
    }
    let ulps = ulps_apart(f64::from_bits(a_bits), f64::from_bits(b_bits));
    (ulps <= MAX_PLATFORM_APPROX_ULPS).then_some(ulps)
}

/// Black-box probe: is the P6 lateral-accel envelope (the `tan`/`atan` arm) the
/// binding steering constraint for this command? Recompute with the lateral-accel
/// ceiling raised to `+∞` — so the talisman's P6 `>` can never fire and its `atan`
/// is never reached — and see if the verdict changes. If it does, P6 was binding
/// and the produced steering is atan-derived. This never touches the frozen
/// kinematics talisman; it only feeds it a different contract.
fn p6_was_the_binding_steering_constraint(
    cmd: &ProposedVehicleCommand,
    class: VehicleClass,
    posture: FleetPosture,
) -> bool {
    let (base, mut no_p6) = match posture {
        FleetPosture::Nominal => (contract_for(class), contract_for(class)),
        FleetPosture::Degraded => (mrc_fallback_for(class), mrc_fallback_for(class)),
        FleetPosture::LockedOut => return false,
    };
    no_p6.max_lateral_accel_mps2 = f64::INFINITY;
    recompute_image(cmd, &base, posture) != recompute_image(cmd, &no_p6, posture)
}

/// The [`VerdictImage`] the checker produces for `cmd` under `posture` against an
/// explicit `contract` (used by the P6-binding probe to compare with vs without
/// the lateral-accel envelope). Same functions the deployed arms run.
fn recompute_image(
    cmd: &ProposedVehicleCommand,
    contract: &VehicleKinematicsContract,
    posture: FleetPosture,
) -> VerdictImage {
    let verdict = match posture {
        FleetPosture::Nominal => validate_vehicle_command(cmd, contract),
        FleetPosture::Degraded => enforce_degraded_decel_to_stop(cmd, contract),
        FleetPosture::LockedOut => unreachable!("classified before this point"),
    };
    // seq / t_wall / derate do not affect the compared fields.
    VerdictImage::of(&record_from_verdict(0, 0, &verdict, posture, cmd, false))
}

/// ulp distance between two SAME-SIGN finite f64s (the recorded and recomputed
/// steering share the command's `signum`). Returns `u64::MAX` for a sign flip or a
/// non-finite value — those are never "a few ulps apart" and stay divergent.
fn ulps_apart(a: f64, b: f64) -> u64 {
    if a == b {
        return 0;
    }
    if !a.is_finite() || !b.is_finite() || a.is_sign_negative() != b.is_sign_negative() {
        return u64::MAX;
    }
    // For two same-sign finite floats the raw bit patterns are monotonic in
    // magnitude, so the absolute bit difference IS the ulp distance.
    (a.to_bits() as i128 - b.to_bits() as i128).unsigned_abs() as u64
}

/// A whole-session replay summary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReplaySummary {
    pub total: usize,
    pub identical: usize,
    pub not_replayable: usize,
    /// W4 (#1043): records that matched on everything but a P6 tan/atan clamp
    /// value within a few ulps — a cross-platform libm spread, NOT a divergence.
    pub platform_approximate: usize,
    /// `(decision_seq, detail)` for every divergent record.
    pub divergences: Vec<(u64, String)>,
    /// `(decision_seq, reason)` for every classified-out record.
    pub skipped: Vec<(u64, String)>,
    /// Lines that did not parse as `CaptureRecord` (line number, error).
    pub parse_errors: Vec<(usize, String)>,
}

impl ReplaySummary {
    /// The session replays deterministically: no true divergence and every line
    /// parsed. W4 (#1043): platform-approximate matches (a P6 tan/atan clamp
    /// within a few ulps across x86/aarch64 libm) are NOT divergences and do not
    /// break determinism — the incident-reconstruction verdict is unchanged.
    #[must_use]
    pub fn is_deterministic(&self) -> bool {
        self.divergences.is_empty() && self.parse_errors.is_empty()
    }
}

/// Replay a captured session (the capture writer's JSONL: one
/// `CaptureRecord` per line; blank lines ignored).
#[must_use]
pub fn replay_session_jsonl(jsonl: &str, class: VehicleClass) -> ReplaySummary {
    let mut summary = ReplaySummary::default();
    for (lineno, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: CaptureRecord = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                summary.parse_errors.push((lineno + 1, e.to_string()));
                continue;
            }
        };
        summary.total += 1;
        match replay_record(&rec, class) {
            ReplayResult::Identical => summary.identical += 1,
            ReplayResult::PlatformApproximate { .. } => summary.platform_approximate += 1,
            ReplayResult::Divergent {
                recorded,
                recomputed,
            } => summary.divergences.push((
                rec.decision_seq,
                format!("recorded {recorded:?} != recomputed {recomputed:?}"),
            )),
            ReplayResult::NotReplayable { reason } => {
                summary.not_replayable += 1;
                summary.skipped.push((rec.decision_seq, reason));
            }
        }
    }
    summary
}

/// Parse the operator-facing class argument (same fail-closed parse as the
/// deployment env: a typo is an error, never a silent other-class envelope).
pub fn parse_class(s: &str) -> Result<VehicleClass, String> {
    VehicleClass::from_str(s)
}

// ---------------------------------------------------------------------------
// EP-19 DoD tests: capture → replay → identical verdicts, in CI. Plus the
// non-vacuity drill: a tampered record DIVERGES.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Run REAL commands through the REAL checker exactly like the deployed
    /// Nominal/Degraded arms, emit records with the REAL emit mapping, and
    /// serialize to the REAL JSONL shape — a synthetic but fully-faithful
    /// captured session.
    fn capture_session(class: VehicleClass) -> String {
        let commands = [
            // Allow (nominal cruise).
            (5.0, 5.0, 0.1, 2.0, 1.5, FleetPosture::Nominal),
            // P2 ceiling clamp (way over any class max).
            (500.0, 5.0, 0.1, 0.0, 0.0, FleetPosture::Nominal),
            // Deny: non-physical dt.
            (5.0, 5.0, 0.0, 0.0, 0.0, FleetPosture::Nominal),
            // Steering-rate clamp.
            (5.0, 5.0, 0.05, 30.0, -30.0, FleetPosture::Nominal),
            // Degraded: speed increase denied.
            (6.0, 5.0, 0.1, 0.0, 0.0, FleetPosture::Degraded),
            // Degraded: re-initiation from stop denied.
            (2.0, 0.0, 0.1, 0.0, 0.0, FleetPosture::Degraded),
            // Degraded: converging decel admitted by the MRC envelope.
            (4.9, 5.0, 0.1, 0.0, 0.0, FleetPosture::Degraded),
        ];
        let mut out = String::new();
        for (seq, (lin, cur, dt, steer, cur_steer, posture)) in commands.iter().enumerate() {
            // `black_box`: force RUNTIME evaluation of the checker on these
            // literals. Without it LLVM const-folds the P5b/P6 transcendental
            // path (tan/atan) at COMPILE time, whose rounding can differ from
            // the runtime libm by one ulp — the emitted record then can't be
            // reproduced by the (runtime) replay. Production captures runtime
            // data, so the emit site is always the runtime flavor; the fixture
            // must match. (Cross-BUILD replay of tan/atan-path clamps is
            // libm-dependent — documented in the incident-reconstruction doc.)
            let cmd = ProposedVehicleCommand {
                linear_velocity_mps: std::hint::black_box(*lin),
                current_velocity_mps: std::hint::black_box(*cur),
                delta_time_s: std::hint::black_box(*dt),
                steering_angle_deg: std::hint::black_box(*steer),
                current_steering_angle_deg: std::hint::black_box(*cur_steer),
            };
            let verdict = match posture {
                FleetPosture::Nominal => validate_vehicle_command(&cmd, &contract_for(class)),
                FleetPosture::Degraded => {
                    enforce_degraded_decel_to_stop(&cmd, &mrc_fallback_for(class))
                }
                FleetPosture::LockedOut => unreachable!(),
            };
            let rec = record_from_verdict(
                seq as u64,
                1_000 + seq as u64,
                &verdict,
                *posture,
                &cmd,
                false,
            );
            out.push_str(&serde_json::to_string(&rec).expect("serialize record"));
            out.push('\n');
        }
        out
    }

    /// EP-19 DoD: the captured session replays with BIT-IDENTICAL verdicts,
    /// for every vehicle class.
    #[test]
    fn capture_replay_identical_verdicts_all_classes() {
        for class in [
            VehicleClass::Courier,
            VehicleClass::DeliveryAv,
            VehicleClass::Robotaxi,
        ] {
            let session = capture_session(class);
            let summary = replay_session_jsonl(&session, class);
            assert_eq!(summary.total, 7, "class {class:?}");
            assert_eq!(
                summary.identical, 7,
                "class {class:?}: {:?}",
                summary.divergences
            );
            assert!(summary.is_deterministic());
        }
    }

    /// Non-vacuity: TAMPER with a recorded verdict → the replay DIVERGES.
    /// (A comparator that can't fail proves nothing.)
    #[test]
    fn tampered_record_diverges() {
        let session = capture_session(VehicleClass::Robotaxi);
        // Flip the first record's outcome: ALLOW -> DENY with a forged code.
        let mut lines: Vec<String> = session.lines().map(String::from).collect();
        let mut rec: CaptureRecord = serde_json::from_str(&lines[0]).expect("parse");
        assert_eq!(rec.outcome, CaptureOutcome::Allow, "fixture sanity");
        rec.outcome = CaptureOutcome::Deny;
        rec.deny_code = Some("INVALID_TIME_DELTA".to_string());
        lines[0] = serde_json::to_string(&rec).expect("serialize");
        let tampered = lines.join("\n");

        let summary = replay_session_jsonl(&tampered, VehicleClass::Robotaxi);
        assert_eq!(summary.divergences.len(), 1, "{summary:?}");
        assert_eq!(summary.divergences[0].0, 0, "the tampered decision_seq");
        assert!(!summary.is_deterministic());
    }

    /// A one-ulp input perturbation is a DIFFERENT session, not noise: the
    /// replayed verdict may legitimately differ, but replay of the ORIGINAL
    /// bytes stays identical — pinning that the comparator is bitwise.
    #[test]
    fn wrong_class_or_mutated_safe_value_diverges() {
        let session = capture_session(VehicleClass::Robotaxi);
        // Replaying a robotaxi session under the courier envelope must flag
        // divergences (different ceilings ⇒ different clamps), not mask them.
        let cross = replay_session_jsonl(&session, VehicleClass::Courier);
        assert!(
            !cross.divergences.is_empty(),
            "cross-class replay must diverge somewhere: {cross:?}"
        );

        // Bit-level: nudge a recorded clamp value by one ulp → divergent.
        let mut lines: Vec<String> = session.lines().map(String::from).collect();
        let mut rec: CaptureRecord = serde_json::from_str(&lines[1]).expect("parse");
        let v = rec.safe_value.expect("record 1 is the ceiling clamp");
        rec.safe_value = Some(f64::from_bits(v.to_bits() ^ 1));
        lines[1] = serde_json::to_string(&rec).expect("serialize");
        let summary = replay_session_jsonl(&lines.join("\n"), VehicleClass::Robotaxi);
        assert_eq!(summary.divergences.len(), 1, "{summary:?}");
    }

    /// Classification honesty: slow-loop, derate-enabled, and LOCKED_OUT
    /// records are NotReplayable with a reason — never guessed at.
    #[test]
    fn incomplete_context_is_classified_not_guessed() {
        let session = capture_session(VehicleClass::Robotaxi);
        let mut rec: CaptureRecord =
            serde_json::from_str(session.lines().next().expect("line")).expect("parse");

        rec.derate_enabled = true;
        assert!(matches!(
            replay_record(&rec, VehicleClass::Robotaxi),
            ReplayResult::NotReplayable { .. }
        ));

        rec.derate_enabled = false;
        rec.posture = "LOCKED_OUT".to_string();
        assert!(matches!(
            replay_record(&rec, VehicleClass::Robotaxi),
            ReplayResult::NotReplayable { .. }
        ));

        rec.posture = "NOMINAL".to_string();
        rec.source = CaptureSource::SlowLoopTrajectory;
        assert!(matches!(
            replay_record(&rec, VehicleClass::Robotaxi),
            ReplayResult::NotReplayable { .. }
        ));
    }

    /// Re-parse a record through the JSONL round-trip (the tests avoid relying on
    /// `CaptureRecord: Clone`; production replay reads JSONL lines anyway).
    fn reparse(rec: &CaptureRecord) -> CaptureRecord {
        serde_json::from_str(&serde_json::to_string(rec).expect("serialize")).expect("parse")
    }

    /// W4 (#1043): a P6 lateral-accel steering clamp is `tan`/`atan`-derived, so a
    /// few-ulp cross-platform (x86/aarch64 libm) perturbation of its recorded value
    /// is classified `PlatformApproximate` — not a false incident divergence — while
    /// a LARGE perturbation (a tamper) still diverges.
    #[test]
    fn p6_clamp_ulp_perturbation_is_platform_approximate() {
        let class = VehicleClass::Robotaxi;
        // A PURE P6 lateral-accel clamp: high speed + in-range steering that breaches
        // ONLY the lateral-accel envelope (20° < the 35° hard limit; 20°/s < the rate
        // ceiling), so the clamp value comes from the atan back-solve.
        let p6_cmd = ProposedVehicleCommand {
            linear_velocity_mps: 30.0,
            current_velocity_mps: 30.0,
            delta_time_s: 1.0,
            steering_angle_deg: 20.0,
            current_steering_angle_deg: 0.0,
        };
        let verdict = validate_vehicle_command(&p6_cmd, &contract_for(class));
        let rec = record_from_verdict(0, 0, &verdict, FleetPosture::Nominal, &p6_cmd, false);
        assert_eq!(
            rec.outcome,
            CaptureOutcome::ClampSteering,
            "fixture must be a P6 steering clamp"
        );
        assert!(
            p6_was_the_binding_steering_constraint(&p6_cmd, class, FleetPosture::Nominal),
            "fixture sanity: P6 must be the binding constraint"
        );

        // Same-platform replay is EXACTLY identical (no excusal on a real match).
        assert!(matches!(
            replay_record(&rec, class),
            ReplayResult::Identical
        ));

        // Perturb the recorded steering by 4 ulps → PlatformApproximate.
        let mut approx = reparse(&rec);
        let v = approx
            .safe_value
            .expect("P6 clamp carries a steering value");
        approx.safe_value = Some(f64::from_bits(v.to_bits() + 4));
        match replay_record(&approx, class) {
            ReplayResult::PlatformApproximate { ulps, .. } => assert_eq!(ulps, 4),
            other => panic!("expected PlatformApproximate, got {other:?}"),
        }
        // A whole-session summary counts it approximate, NOT a divergence, and stays
        // deterministic — an ulp-level libm spread is not an incident.
        let summary = replay_session_jsonl(&serde_json::to_string(&approx).unwrap(), class);
        assert_eq!(summary.platform_approximate, 1);
        assert!(summary.divergences.is_empty());
        assert!(summary.is_deterministic());

        // A LARGE perturbation (≫ any libm spread) is still a real divergence.
        let mut big = reparse(&rec);
        big.safe_value = Some(big.safe_value.unwrap() + 1.0); // ~10^15 ulps — a tamper
        assert!(matches!(
            replay_record(&big, class),
            ReplayResult::Divergent { .. }
        ));
    }

    /// W4 non-vacuity: a `tan`/`atan`-FREE steering clamp (a P5b rate ceiling) is
    /// fully deterministic, so even a 4-ulp perturbation must DIVERGE — the
    /// platform-approximate excusal is scoped to the P6 arm only.
    #[test]
    fn p5_rate_clamp_ulp_perturbation_still_diverges() {
        let class = VehicleClass::Robotaxi;
        // Low speed (P6 never fires) but a large steering STEP over a short dt → P5b
        // rate ceiling clamps it, deterministically.
        let p5_cmd = ProposedVehicleCommand {
            linear_velocity_mps: 2.0,
            current_velocity_mps: 2.0,
            delta_time_s: 0.05,
            steering_angle_deg: 30.0,
            current_steering_angle_deg: -30.0,
        };
        let verdict = validate_vehicle_command(&p5_cmd, &contract_for(class));
        let rec = record_from_verdict(0, 0, &verdict, FleetPosture::Nominal, &p5_cmd, false);
        assert_eq!(
            rec.outcome,
            CaptureOutcome::ClampSteering,
            "fixture must be a P5 rate clamp"
        );
        assert!(
            !p6_was_the_binding_steering_constraint(&p5_cmd, class, FleetPosture::Nominal),
            "fixture sanity: this clamp is P5, not P6"
        );

        let mut nudged = reparse(&rec);
        let v = nudged.safe_value.unwrap();
        nudged.safe_value = Some(f64::from_bits(v.to_bits() + 4));
        assert!(
            matches!(
                replay_record(&nudged, class),
                ReplayResult::Divergent { .. }
            ),
            "a deterministic P5 clamp must still diverge on ANY bit change"
        );
    }

    /// Fail-closed class parse (a typo must never select another envelope).
    #[test]
    fn class_parse_is_fail_closed() {
        assert!(parse_class("robotaxi").is_ok());
        assert!(parse_class("Robotaxi ").is_ok());
        assert!(parse_class("robotaxxi").is_err());
        assert!(parse_class("").is_err());
    }
}
