//! L3.3 — the governor CONSUMER: read the frozen Clause-2 contract, validate it,
//! decode the command, and bound it. The CHECKER counterpart to the ROS
//! adapter's guest-side producer (L3.2, `kirra_ros2_adapter::contract_producer`).
//!
//! In the ROS2/QNX guest split (ADR-0006 / HVCHAN-001) the governor partition
//! reads the guest's proposal across the boundary and is the SOLE safety
//! authority. This module composes the full receive path in ONE fail-closed
//! pipeline:
//!
//! 1. [`read_coherent_snapshot`] — a torn-free owned copy (seqlock retry budget)
//! 2. [`validate`] — the transport contract (layout/magic/bounds/CRC/sequence/
//!    generation/deadline) against the monotonic [`AcceptedWatermark`]
//! 3. [`VehicleCommandPayload::from_validated_view`] — decode the frozen payload
//!    (fail-closed on wrong length / non-finite)
//! 4. [`validate_vehicle_command`] — bound the decoded command against the
//!    per-class [`VehicleKinematicsContract`] (the talisman)
//!
//! **Every** failure short-circuits to a typed reject; only a snapshot that
//! clears steps 1-3 reaches the kinematic bound, and only then does the watermark
//! advance (HVCHAN-001 §3.1 — a rejected snapshot never poisons it). Any verdict
//! that is not [`GovernorVerdict::Bounded`] with an actuatable [`EnforceAction`]
//! means the governor issues its MRC safe-stop; the guest is never trusted.

use kirra_contract_channel::{
    read_coherent_snapshot, validate, AcceptedWatermark, CommandCodecError, ContractFault,
    ContractReader, SnapshotFault, VehicleCommandPayload,
};

use crate::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
};

impl From<VehicleCommandPayload> for ProposedVehicleCommand {
    /// The frozen wire payload IS the checker's input type, field-for-field
    /// (L3.1 froze it as the on-wire form of `ProposedVehicleCommand`). This
    /// conversion is the seam where the transport contract hands off to the
    /// kinematic contract.
    fn from(p: VehicleCommandPayload) -> Self {
        ProposedVehicleCommand {
            linear_velocity_mps: p.linear_velocity_mps,
            current_velocity_mps: p.current_velocity_mps,
            delta_time_s: p.delta_time_s,
            steering_angle_deg: p.steering_angle_deg,
            current_steering_angle_deg: p.current_steering_angle_deg,
        }
    }
}

/// The outcome of consuming one command from the contract region. Fail-closed by
/// construction: the transport/codec faults are distinct reject variants, and
/// even a successful [`Bounded`](Self::Bounded) verdict can carry a
/// [`EnforceAction::DenyBreach`] the governor must treat as an MRC.
#[derive(Clone, Debug, PartialEq)]
pub enum GovernorVerdict {
    /// The transport contract and codec passed; this is the governor's kinematic
    /// decision. Actuatable iff the [`EnforceAction`] is `Allow` or a `Clamp*`;
    /// `DenyBreach` is a fail-closed rejection at the kinematic layer.
    Bounded(EnforceAction),
    /// No coherent snapshot within the retry budget (odd/churning generation).
    Snapshot(SnapshotFault),
    /// The transport contract rejected the snapshot (replay/regress/deadline/…).
    Contract(ContractFault),
    /// The payload failed to decode (wrong length / non-finite field).
    Codec(CommandCodecError),
}

impl GovernorVerdict {
    /// Whether the command may be actuated: only a `Bounded` verdict whose
    /// [`EnforceAction`] is `Allow` or a `Clamp*` is actuatable. Every other
    /// outcome — any fault, or a `DenyBreach` — means the governor issues its
    /// MRC safe-stop (fail-closed).
    pub fn is_actuatable(&self) -> bool {
        matches!(
            self,
            GovernorVerdict::Bounded(
                EnforceAction::Allow
                    | EnforceAction::ClampLinear(_)
                    | EnforceAction::ClampSteering(_)
            )
        )
    }
}

/// Consume one proposal from the contract `reader` and bound it, fail-closed at
/// every stage (see the module docs). `now_nanos` MUST be in the boundary clock
/// domain (HVCHAN-001 §5, R-HV-3); `max_retries` is the seqlock retry budget
/// (e.g. [`kirra_contract_channel::MAX_SNAPSHOT_RETRIES`]).
///
/// The `watermark` advances only when steps 1-3 all pass — i.e. a transport- and
/// codec-valid command was received — regardless of the subsequent kinematic
/// verdict, so a validly-received-but-kinematically-denied command still burns
/// its sequence (the guest must send a strictly newer one). A snapshot/contract/
/// codec fault leaves the watermark untouched.
///
/// `#[must_use]`: the returned [`GovernorVerdict`] is safety-critical — dropping
/// it would silently skip the actuation gate. (Matches `validate_vehicle_command`.)
#[must_use]
pub fn consume_and_bound<R: ContractReader>(
    reader: &R,
    watermark: &mut AcceptedWatermark,
    now_nanos: u64,
    contract: &VehicleKinematicsContract,
    max_retries: u32,
) -> GovernorVerdict {
    let snapshot = match read_coherent_snapshot(reader, max_retries) {
        Ok(view) => view,
        Err(fault) => return GovernorVerdict::Snapshot(fault),
    };
    if let Err(fault) = validate(&snapshot, now_nanos, watermark) {
        return GovernorVerdict::Contract(fault);
    }
    let payload = match VehicleCommandPayload::from_validated_view(&snapshot) {
        Ok(p) => p,
        Err(err) => return GovernorVerdict::Codec(err),
    };
    // Transport + codec passed: the command was validly received. Advance the
    // monotonic watermark now (HVCHAN-001 §3.1), before the kinematic bound —
    // the bound decides actuation, not receipt.
    watermark.record(&snapshot);
    GovernorVerdict::Bounded(validate_vehicle_command(&payload.into(), contract))
}

#[cfg(test)]
mod tests {
    use super::*;
    use kirra_contract_channel::reference::InProcessRegion;
    use kirra_contract_channel::{
        publish, CommandField, GovernorContractView, MAX_SNAPSHOT_RETRIES,
    };

    // Publish `payload` at `sequence` on `committed_gen`, returning the new
    // committed generation. (kirra-core can't use the adapter's producer — that
    // would be a dependency cycle — so we drive the seqlock directly.)
    fn publish_payload(
        region: &InProcessRegion,
        committed_gen: u64,
        sequence: u64,
        deadline_nanos: u64,
        payload: &VehicleCommandPayload,
    ) -> u64 {
        let body = payload.to_view(committed_gen, sequence, 0, deadline_nanos);
        publish(region, committed_gen, &body)
    }

    fn in_envelope() -> VehicleCommandPayload {
        // 1.0 m/s^2 accel, 10 deg/s steer rate — inside the nominal profile.
        VehicleCommandPayload {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 9.9,
            delta_time_s: 0.1,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 4.0,
        }
    }

    #[test]
    fn payload_converts_to_proposed_command_field_for_field() {
        let p = in_envelope();
        let cmd: ProposedVehicleCommand = p.into();
        assert_eq!(cmd.linear_velocity_mps, p.linear_velocity_mps);
        assert_eq!(cmd.current_velocity_mps, p.current_velocity_mps);
        assert_eq!(cmd.delta_time_s, p.delta_time_s);
        assert_eq!(cmd.steering_angle_deg, p.steering_angle_deg);
        assert_eq!(cmd.current_steering_angle_deg, p.current_steering_angle_deg);
    }

    #[test]
    fn valid_in_envelope_command_is_allowed_and_advances_the_watermark() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        publish_payload(&region, 0, 1, 10_000, &in_envelope());
        let verdict = consume_and_bound(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);

        assert_eq!(verdict, GovernorVerdict::Bounded(EnforceAction::Allow));
        assert!(verdict.is_actuatable());
        assert_eq!(wm.last(), Some((2, 1))); // generation 2 (even), sequence 1
    }

    #[test]
    fn over_envelope_command_is_bounded_but_not_allowed() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        // 50 m/s desired, way over the 35 m/s hard ceiling: the governor engages
        // its envelope (clamp/deny), NOT Allow — the transport/codec still passed.
        let over = VehicleCommandPayload {
            linear_velocity_mps: 50.0,
            current_velocity_mps: 34.0,
            delta_time_s: 0.1,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 4.0,
        };
        publish_payload(&region, 0, 1, 10_000, &over);
        let verdict = consume_and_bound(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);

        assert!(matches!(verdict, GovernorVerdict::Bounded(_)));
        assert_ne!(verdict, GovernorVerdict::Bounded(EnforceAction::Allow));
        assert_eq!(wm.last(), Some((2, 1))); // transport-valid → watermark advanced
    }

    #[test]
    fn a_replay_is_rejected_at_the_contract_and_leaves_the_watermark() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        let g1 = publish_payload(&region, 0, 1, 10_000, &in_envelope());
        assert!(consume_and_bound(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES)
            .is_actuatable());
        assert_eq!(wm.last(), Some((2, 1)));

        // Re-publish the SAME sequence 1 (generation advances). Contract rejects.
        publish_payload(&region, g1, 1, 10_000, &in_envelope());
        let verdict = consume_and_bound(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert_eq!(
            verdict,
            GovernorVerdict::Contract(ContractFault::SequenceRegressOrReplay {
                found: 1,
                last_accepted: 1,
            })
        );
        assert!(!verdict.is_actuatable());
        assert_eq!(wm.last(), Some((2, 1))); // unchanged by the rejected replay
    }

    #[test]
    fn an_expired_deadline_is_rejected_at_the_contract() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        publish_payload(&region, 0, 1, 1_000, &in_envelope()); // deadline 1_000
        let verdict = consume_and_bound(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert_eq!(
            verdict,
            GovernorVerdict::Contract(ContractFault::DeadlineExpired {
                now: 5_000,
                deadline: 1_000,
            })
        );
        assert_eq!(wm.last(), None); // nothing accepted
    }

    #[test]
    fn a_non_finite_payload_is_rejected_at_the_codec() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        // A NaN steering field: CRC/bounds pass (validate ok), decode fails closed.
        let mut bad = in_envelope();
        bad.steering_angle_deg = f64::NAN;
        publish_payload(&region, 0, 1, 10_000, &bad);
        let verdict = consume_and_bound(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert_eq!(
            verdict,
            GovernorVerdict::Codec(CommandCodecError::NonFinite {
                field: CommandField::SteeringAngleDeg,
            })
        );
        assert!(!verdict.is_actuatable());
        assert_eq!(wm.last(), None); // decode fault does not advance the watermark
    }

    /// A reader whose generation is perpetually ODD (a writer stuck mid-write):
    /// `read_coherent_snapshot` must exhaust its budget and fail closed.
    struct AlwaysWritingRegion(GovernorContractView);
    impl ContractReader for AlwaysWritingRegion {
        fn load_generation(&self) -> u64 {
            1 // odd forever
        }
        fn copy_view(&self) -> GovernorContractView {
            self.0
        }
    }

    #[test]
    fn a_perpetually_odd_generation_fails_closed_as_a_snapshot_fault() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();
        let region = AlwaysWritingRegion(in_envelope().to_view(1, 1, 0, 10_000));
        let verdict = consume_and_bound(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert!(matches!(verdict, GovernorVerdict::Snapshot(_)));
        assert!(!verdict.is_actuatable());
        assert_eq!(wm.last(), None);
    }
}
