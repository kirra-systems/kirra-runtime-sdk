// src/verdicts.rs — EP-17 explainable safety verdicts.
//
// Every actuator denial already carries a machine `DenyCode` and is written to
// the SHA-256 hash-chained (and, with a signing key installed, Ed25519-signed)
// audit ledger. EP-17 makes that denial RETRIEVABLE as a signed, HUMAN-READABLE
// artifact:
//
//   1. the deny arm mints a `verdict_id` (this module) and binds it INTO the
//      chained audit payload, then returns it in the 400 response body;
//   2. `GET /verdicts/{id}` (auditor tier) loads the chained record and renders
//      code → human explanation (this module's table) → inputs digest, plus the
//      chain fields (sequence / record hash / signature / key id) that make the
//      artifact independently verifiable.
//
// PURE module: no I/O, no state beyond the id counter. The explanation table is
// deliberately NOT in the frozen kinematics talisman (`DenyCode` lives there;
// its pin is a git blob hash) — mapping tokens here keeps the talisman
// untouched while every variant still gets a reviewed operator sentence.

// The verdict-id codec (`mint_verdict_id` / `is_valid_verdict_id`) is a
// content-addressed SHA-256 audit id — it moved to the lean `kirra-audit-hash`
// crate (ADR-0035 — the kirra-persistence enabling work) so the persistence layer
// can validate ids without naming this module. Re-exported so every existing
// `crate::verdicts::{mint,is_valid}_verdict_id` path (deny arm, handler) is unchanged.
pub use kirra_audit_hash::{is_valid_verdict_id, mint_verdict_id};

/// Fallback explanation for a token this build does not recognize (a NEWER
/// verifier wrote the record). Deliberately loud about being generic.
pub const EXPLAIN_UNKNOWN: &str = "Unrecognized denial code (recorded by a newer verifier \
     version). The command was rejected fail-closed; consult the audit record's raw payload.";

/// Operator-grade explanation for each `DenyCode` wire token
/// (`DenyCode::reason()` / the `violation` field of the chained audit payload).
///
/// Keep in lock-step with the talisman's `DenyCode` — the
/// `every_deny_code_has_a_specific_explanation` test walks the real enum, so
/// adding a variant without a sentence here fails CI.
#[must_use]
pub fn explain_deny_token(token: &str) -> &'static str {
    match token {
        "NAN_INF_LINEAR_VELOCITY" => {
            "The commanded linear velocity was NaN or infinite. Non-finite values silently \
             poison every downstream safety computation (NaN comparisons are always false), \
             so the command was rejected before any arithmetic — fail-closed (SG9)."
        }
        "NAN_INF_CURRENT_VELOCITY" => {
            "The reported CURRENT velocity was NaN or infinite, so no rate-of-change \
             invariant could be computed soundly. Rejected before any arithmetic (SG9)."
        }
        "NAN_INF_STEERING_ANGLE" => {
            "The commanded steering angle was NaN or infinite. Rejected before any \
             arithmetic — a non-finite angle would silently bypass the lateral-acceleration \
             envelope (SG9)."
        }
        "NAN_INF_CURRENT_STEERING" => {
            "The reported CURRENT steering angle was NaN or infinite, so the steering-rate \
             ceiling could not be evaluated soundly. Rejected before any arithmetic (SG9)."
        }
        "NAN_INF_DELTA_TIME" => {
            "The planning time step was NaN or infinite. Every rate limit divides by this \
             value, so it must be a finite positive number. Rejected (SG9)."
        }
        "INVALID_TIME_DELTA" => {
            "The planning time step was zero or negative, which makes acceleration and \
             steering-rate checks undefined. A well-formed command carries the positive \
             duration of its planning tick (SG3)."
        }
        "ASSET_LOCKED_OUT" => {
            "The asset's safety posture was LockedOut when the command arrived. Under \
             LockedOut every actuator command is denied and the vehicle executes its \
             minimal-risk safe-stop; recovery requires operator clearance."
        }
        "DRIVABLE_SPACE_DEPARTURE" => {
            "The proposed trajectory left the drivable corridor: at least one pose's \
             vehicle footprint was not contained in the perceived drivable space (SG2). \
             The doer proposes; the checker refused the departure."
        }
        "DEGRADED_REINITIATION_DENIED" => {
            "The fleet posture was Degraded and the vehicle was at a stop; the command \
             attempted to re-initiate motion (or reverse direction through the stop). \
             Degraded is a controlled decel-to-stop-and-HOLD — the governor never authors \
             re-acceleration; recovery to Nominal restores motion (issue #70, SS-002)."
        }
        "DEGRADED_SPEED_INCREASE_DENIED" => {
            "The fleet posture was Degraded and the command increased speed magnitude. \
             Degraded admits only non-increasing speed along the decel-to-stop envelope \
             (issue #70, SS-002); the denial converges the vehicle to a controlled stop."
        }
        "FRAME_INTEGRITY_UNTRUSTED" => {
            "The command arrived over the contract channel in a frame whose integrity \
             verification failed (torn/stale/corrupt per the frozen channel contract). An \
             untrusted frame is never interpreted — rejected fail-closed."
        }
        "TRAJECTORY_MRC_FALLBACK" => {
            "The slow-loop checker refused to promote the proposed trajectory (containment, \
             per-pose kinematics, RSS, occlusion, VRU, or posture — the refusal reason \
             side-channel carries which), so the fast loop holds the minimal-risk fallback. \
             The doer proposes; the checker disposed."
        }
        "TRAJECTORY_HORIZON_EXCEEDED" => {
            "The proposed trajectory carried more poses than the bounded validation \
             horizon. The bound is what makes the checker's worst-case execution time \
             provable; an over-horizon proposal is rejected rather than truncated."
        }
        _ => EXPLAIN_UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_core::kinematics_contract::DenyCode;

    /// Walks the REAL `DenyCode` enum: every variant's wire token must map to
    /// a specific (non-fallback) explanation. Adding a variant to the talisman
    /// without adding its operator sentence here fails this test.
    #[test]
    fn every_deny_code_has_a_specific_explanation() {
        let all = [
            DenyCode::NanInfLinearVelocity,
            DenyCode::NanInfCurrentVelocity,
            DenyCode::NanInfSteeringAngle,
            DenyCode::NanInfCurrentSteering,
            DenyCode::NanInfDeltaTime,
            DenyCode::InvalidTimeDelta,
            DenyCode::AssetLockedOut,
            DenyCode::DrivableSpaceDeparture,
            DenyCode::DegradedReinitiationDenied,
            DenyCode::DegradedSpeedIncreaseDenied,
            DenyCode::FrameIntegrityUntrusted,
            DenyCode::TrajectoryHorizonExceeded,
        ];
        for code in all {
            let explanation = explain_deny_token(code.reason());
            assert_ne!(
                explanation,
                EXPLAIN_UNKNOWN,
                "DenyCode::{code:?} ({}) has no operator explanation",
                code.reason()
            );
            assert!(
                explanation.len() > 40,
                "explanation for {code:?} is too thin"
            );
        }
    }

    /// The slow-loop capture deny code (`record_from_trajectory_verdict`,
    /// kirra-core capture) must have a specific explanation — it previously
    /// fell through to EXPLAIN_UNKNOWN (the Part-3 known gap).
    #[test]
    fn trajectory_mrc_fallback_has_a_specific_explanation() {
        assert_ne!(
            explain_deny_token("TRAJECTORY_MRC_FALLBACK"),
            EXPLAIN_UNKNOWN
        );
    }

    #[test]
    fn unknown_token_gets_the_loud_fallback() {
        assert_eq!(explain_deny_token("SOME_FUTURE_CODE"), EXPLAIN_UNKNOWN);
    }

    #[test]
    fn minted_ids_are_valid_and_distinct() {
        let a = mint_verdict_id(1_000, "{\"x\":1}");
        let b = mint_verdict_id(1_000, "{\"x\":1}"); // same ms, same payload
        assert!(is_valid_verdict_id(&a), "{a}");
        assert!(is_valid_verdict_id(&b), "{b}");
        assert_ne!(
            a, b,
            "the counter must discriminate same-ms same-payload mints"
        );
    }

    #[test]
    fn id_validation_rejects_metacharacters_and_shapes() {
        assert!(!is_valid_verdict_id(""));
        assert!(!is_valid_verdict_id("abc"));
        assert!(!is_valid_verdict_id(&"a".repeat(31)));
        assert!(!is_valid_verdict_id(&"a".repeat(33)));
        assert!(!is_valid_verdict_id(&format!("{}%", "a".repeat(31))));
        assert!(!is_valid_verdict_id(&format!("{}_", "a".repeat(31))));
        assert!(
            !is_valid_verdict_id(&"A".repeat(32)),
            "uppercase is not canonical"
        );
        assert!(is_valid_verdict_id("0123456789abcdef0123456789abcdef"));
    }
}
