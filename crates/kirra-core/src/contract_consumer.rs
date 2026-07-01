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
    ContractReader, GovernorContractView, SnapshotFault, VehicleCommandPayload,
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
    match receive(reader, watermark, now_nanos, max_retries) {
        Received::Command { command, .. } => {
            GovernorVerdict::Bounded(validate_vehicle_command(&command, contract))
        }
        Received::Snapshot(f) => GovernorVerdict::Snapshot(f),
        Received::Contract(f) => GovernorVerdict::Contract(f),
        Received::Codec(e) => GovernorVerdict::Codec(e),
    }
}

/// The receive stage shared by [`consume_and_bound`] and [`decide`]: the
/// transport half of the pipeline (snapshot → validate → decode), returning the
/// decoded command or the first fault. On success the monotonic `watermark`
/// advances (HVCHAN-001 §3.1) — before any kinematic bound, so receipt (not
/// actuation) governs sequence advancement. A fault leaves the watermark
/// untouched. Kept private so there is ONE transport pipeline, never two to drift.
enum Received {
    /// The decoded command AND the exact validated snapshot it came from — the
    /// snapshot is the release-token signing input (HVCHAN §3.5-6), carried so the
    /// governor signs the same bytes it validated.
    Command { command: ProposedVehicleCommand, view: GovernorContractView },
    Snapshot(SnapshotFault),
    Contract(ContractFault),
    Codec(CommandCodecError),
}

fn receive<R: ContractReader>(
    reader: &R,
    watermark: &mut AcceptedWatermark,
    now_nanos: u64,
    max_retries: u32,
) -> Received {
    let snapshot = match read_coherent_snapshot(reader, max_retries) {
        Ok(view) => view,
        Err(fault) => return Received::Snapshot(fault),
    };
    if let Err(fault) = validate(&snapshot, now_nanos, watermark) {
        return Received::Contract(fault);
    }
    let payload = match VehicleCommandPayload::from_validated_view(&snapshot) {
        Ok(p) => p,
        Err(err) => return Received::Codec(err),
    };
    watermark.record(&snapshot);
    Received::Command { command: payload.into(), view: snapshot }
}

/// Apply a kinematic verdict to the decoded command: `Allow`/`Clamp*` become
/// [`GovernorOutcome::Actuate`] (the clamp folded in), a `DenyBreach` becomes
/// [`GovernorOutcome::SafeStop`]. Shared by [`decide`] and [`decide_cycle`].
fn apply(action: EnforceAction, command: ProposedVehicleCommand) -> GovernorOutcome {
    match action {
        EnforceAction::Allow => GovernorOutcome::Actuate(command),
        EnforceAction::ClampLinear(v) => {
            let mut c = command;
            c.linear_velocity_mps = v;
            GovernorOutcome::Actuate(c)
        }
        EnforceAction::ClampSteering(s) => {
            let mut c = command;
            c.steering_angle_deg = s;
            GovernorOutcome::Actuate(c)
        }
        EnforceAction::DenyBreach(_) => GovernorOutcome::SafeStop,
    }
}

/// The governor's actuation decision for one cycle — the fail-closed reduction of
/// a [`GovernorVerdict`] to what the actuator does.
#[derive(Clone, Debug, PartialEq)]
pub enum GovernorOutcome {
    /// Actuate this command, with the kinematic bound ALREADY APPLIED (a `Clamp`
    /// verdict returns the clamped value; `Allow` returns the command as received).
    Actuate(ProposedVehicleCommand),
    /// Issue the MRC safe-stop: any transport/codec fault, or a kinematic
    /// `DenyBreach`. The governor never actuates a guest command in this case.
    SafeStop,
}

/// Consume one proposal and reduce it to an actuation decision, fail-closed:
/// `receive` → `validate_vehicle_command` → apply the verdict. `Allow`/`Clamp*`
/// become [`GovernorOutcome::Actuate`] (the clamp already folded into the command);
/// a `DenyBreach` or ANY snapshot/contract/codec fault becomes
/// [`GovernorOutcome::SafeStop`]. This is the governor loop's per-cycle step; the
/// watermark advances exactly as in [`consume_and_bound`] (they share `receive`).
///
/// `#[must_use]`: the outcome gates actuation vs. MRC — dropping it is a safety bug.
#[must_use]
pub fn decide<R: ContractReader>(
    reader: &R,
    watermark: &mut AcceptedWatermark,
    now_nanos: u64,
    contract: &VehicleKinematicsContract,
    max_retries: u32,
) -> GovernorOutcome {
    match receive(reader, watermark, now_nanos, max_retries) {
        Received::Command { command, .. } => apply(validate_vehicle_command(&command, contract), command),
        // Any transport/codec fault → fail closed to the safe stop.
        Received::Snapshot(_) | Received::Contract(_) | Received::Codec(_) => GovernorOutcome::SafeStop,
    }
}

/// The result of one governor cycle: the actuation [`outcome`](Self::outcome) plus
/// the signable [`view`](Self::view) — the release-token signing input (HVCHAN
/// §3.5-6). `view` is `Some` iff a command was received (transport + codec passed)
/// and `None` on a snapshot/contract/codec fault.
///
/// **The view binds the ENFORCED command, not the guest's raw proposal.** On an
/// [`Actuate`](GovernorOutcome::Actuate) outcome the view is rebuilt over the
/// post-enforcement command (see [`decide_cycle`]), so a `Clamp*` verdict's folded
/// bound is reflected in the bytes that get signed — the governor signs *exactly*
/// what the actuator will release. `Allow` leaves it byte-identical to the received
/// snapshot. On a received-but-denied `SafeStop` the field carries the raw received
/// view (never signable — see [`view_to_sign`](Self::view_to_sign)).
#[derive(Clone, Debug, PartialEq)]
pub struct GovernorCycle {
    pub outcome: GovernorOutcome,
    pub view: Option<GovernorContractView>,
}

impl GovernorCycle {
    /// The view to sign for the release token — `Some` **only** when the outcome
    /// is [`GovernorOutcome::Actuate`], and then it is the ENFORCED view (the
    /// post-clamp command). This is the correct gate for the release seam: `view`
    /// is populated whenever a command was *received* (transport + codec passed),
    /// so it is `Some` even on a kinematic `DenyBreach → SafeStop`. Signing on
    /// `view.is_some()` would therefore sign a DENIED command; signing on
    /// `view_to_sign()` cannot, and it signs the actuated (clamped) bytes rather
    /// than the guest's proposal. "Sign only what is actuatable" at the type.
    #[must_use]
    pub fn view_to_sign(&self) -> Option<&GovernorContractView> {
        if matches!(self.outcome, GovernorOutcome::Actuate(_)) {
            self.view.as_ref()
        } else {
            None
        }
    }
}

/// Like [`decide`], but also surfaces the signable [`GovernorContractView`] for the
/// release token (HVCHAN §3.5-6 / ADR-0013). Same pipeline, same watermark
/// advancement, same fail-closed reduction.
///
/// On an actuatable verdict the returned `view` is the **enforced** view — the
/// received snapshot's header with the *post-enforcement* command (a `Clamp*`
/// verdict folds its bound in). This closes the integrity gap where the governor
/// would otherwise sign the guest's raw proposal while the actuator drives the
/// clamped command: here the signed bytes are exactly the actuated bytes. `Allow`
/// is byte-identical to the received snapshot (enforced command == received).
///
/// `#[must_use]`: the outcome gates actuation vs. MRC — dropping it is a safety bug.
#[must_use]
pub fn decide_cycle<R: ContractReader>(
    reader: &R,
    watermark: &mut AcceptedWatermark,
    now_nanos: u64,
    contract: &VehicleKinematicsContract,
    max_retries: u32,
) -> GovernorCycle {
    match receive(reader, watermark, now_nanos, max_retries) {
        Received::Command { command, view } => {
            let outcome = apply(validate_vehicle_command(&command, contract), command);
            // Bind the token to the bytes the actuator will ACTUALLY drive. On a
            // Clamp verdict `apply` folded the bound into the command, so the raw
            // received `view` no longer matches it — rebuild the enforced view. A
            // denied command keeps the raw received view (never signable).
            let view = match &outcome {
                GovernorOutcome::Actuate(actuated) => enforced_view(actuated, &view),
                GovernorOutcome::SafeStop => view,
            };
            GovernorCycle { outcome, view: Some(view) }
        }
        Received::Snapshot(_) | Received::Contract(_) | Received::Codec(_) => {
            GovernorCycle { outcome: GovernorOutcome::SafeStop, view: None }
        }
    }
}

/// Rebuild the contract view over the POST-ENFORCEMENT `command`, reusing the
/// received snapshot's header (generation / sequence / timestamps / deadline) and
/// a freshly computed CRC. The release token is signed over this view's canonical
/// image, so on a `Clamp*` verdict the signed bytes are the clamped (actuated)
/// bytes, not the guest's proposal. Byte-identical to `received` when the verdict
/// was `Allow` (the command is unchanged).
fn enforced_view(
    command: &ProposedVehicleCommand,
    received: &GovernorContractView,
) -> GovernorContractView {
    let payload = VehicleCommandPayload {
        linear_velocity_mps: command.linear_velocity_mps,
        current_velocity_mps: command.current_velocity_mps,
        delta_time_s: command.delta_time_s,
        steering_angle_deg: command.steering_angle_deg,
        current_steering_angle_deg: command.current_steering_angle_deg,
    };
    payload.to_view(
        received.generation,
        received.sequence,
        received.publication_nanos,
        received.deadline_nanos,
    )
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

    // ---- decide() — the actuate-vs-safe-stop reduction ---------------------

    #[test]
    fn decide_actuates_an_in_envelope_command_unchanged() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        publish_payload(&region, 0, 1, 10_000, &in_envelope());
        let outcome = decide(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        // Allow → Actuate with the command exactly as received.
        assert_eq!(outcome, GovernorOutcome::Actuate(in_envelope().into()));
        assert_eq!(wm.last(), Some((2, 1))); // watermark advanced, like consume_and_bound
    }

    #[test]
    fn decide_bounds_an_over_envelope_command_below_the_proposal() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        // 50 m/s desired over the 35 m/s ceiling, current == desired (accel 0) so
        // ONLY the absolute speed bound trips → the nominal contract CLAMPS.
        let over = VehicleCommandPayload {
            linear_velocity_mps: 50.0,
            current_velocity_mps: 50.0,
            ..in_envelope()
        };
        publish_payload(&region, 0, 1, 10_000, &over);
        match decide(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES) {
            // ClampLinear → Actuate the clamped command (asserting Actuate, not
            // accepting SafeStop, so a clamping regression can't hide here).
            GovernorOutcome::Actuate(c) => assert!(
                c.linear_velocity_mps <= 35.0, // clamped into the 35 m/s envelope (< the 50 proposal)
                "over-speed must be clamped into the envelope, got {}",
                c.linear_velocity_mps
            ),
            GovernorOutcome::SafeStop => panic!("nominal over-speed must clamp (Actuate), not safe-stop"),
        }
    }

    #[test]
    fn decide_safe_stops_on_an_expired_deadline() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        publish_payload(&region, 0, 1, 1_000, &in_envelope()); // deadline 1_000
        let outcome = decide(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert_eq!(outcome, GovernorOutcome::SafeStop); // now > deadline → fail closed
        assert_eq!(wm.last(), None);
    }

    #[test]
    fn decide_safe_stops_on_a_non_finite_payload() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        let mut bad = in_envelope();
        bad.linear_velocity_mps = f64::INFINITY;
        publish_payload(&region, 0, 1, 10_000, &bad);
        let outcome = decide(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert_eq!(outcome, GovernorOutcome::SafeStop); // codec fail-closed
        assert_eq!(wm.last(), None);
    }

    // ---- decide_cycle() — outcome + the validated view for the release token --

    #[test]
    fn decide_cycle_surfaces_the_validated_view_when_actuatable() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        publish_payload(&region, 0, 1, 10_000, &in_envelope());
        let cycle = decide_cycle(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert_eq!(cycle.outcome, GovernorOutcome::Actuate(in_envelope().into()));
        // The view is present and IS the exact validated snapshot (the sign input).
        let view = cycle.view.expect("actuatable → view present for the release token");
        assert_eq!(view.sequence, 1);
        assert_eq!(
            VehicleCommandPayload::from_validated_view(&view),
            Ok(in_envelope())
        );
    }

    #[test]
    fn decide_cycle_has_no_view_on_a_fault() {
        let region = InProcessRegion::new();
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut wm = AcceptedWatermark::new();

        publish_payload(&region, 0, 1, 1_000, &in_envelope()); // deadline 1_000
        let cycle = decide_cycle(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        assert_eq!(cycle.outcome, GovernorOutcome::SafeStop);
        assert_eq!(cycle.view, None); // nothing to sign — a fault never releases
    }
}
