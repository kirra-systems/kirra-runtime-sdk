// End-to-end governor → actuator release flow (HVCHAN §3 / ADR-0030 Clause F):
// a proposal is published, the GOVERNOR validates + bounds it (`decide_cycle`),
// signs the exact validated view, and the ACTUATOR verifies the token before it
// would release — refusing anything the governor did not sign. Ties the L3
// consumer (kirra-core) to the release bridge (this crate).

use ed25519_dalek::SigningKey;

use kirra_contract_channel::reference::InProcessRegion;
use kirra_contract_channel::{
    publish, AcceptedWatermark, VehicleCommandPayload, MAX_SNAPSHOT_RETRIES,
};
use kirra_core::contract_consumer::{decide_cycle, GovernorOutcome};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_release_token::{issue_release_token, verify_release, ReleaseDenied};

fn in_envelope() -> VehicleCommandPayload {
    VehicleCommandPayload {
        linear_velocity_mps: 10.0,
        current_velocity_mps: 10.0, // accel 0
        delta_time_s: 0.1,
        steering_angle_deg: 1.0,
        current_steering_angle_deg: 1.0,
    }
}

#[test]
fn actuator_releases_only_the_governor_signed_command() {
    // The governor's signing identity (deterministic seed — no RNG in the test).
    let gov_key = SigningKey::from_bytes(&[7u8; 32]);
    let gov_pub = gov_key.verifying_key();

    // Guest publishes an in-envelope proposal.
    let region = InProcessRegion::new();
    publish(&region, 0, &in_envelope().to_view(0, 1, 0, u64::MAX / 2));

    // Governor: validate + bound, and get the exact validated view to sign.
    let contract = VehicleKinematicsContract::nominal_reference_profile();
    let mut wm = AcceptedWatermark::new();
    let cycle = decide_cycle(&region, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
    assert!(
        matches!(cycle.outcome, GovernorOutcome::Actuate(_)),
        "in-envelope must actuate"
    );
    // `view_to_sign()` is the correct gate: Some only because the outcome is Actuate.
    let view = *cycle.view_to_sign().expect("actuatable → a view to sign");
    let token = issue_release_token(&view, &gov_key);

    // Actuator: verify BEFORE releasing. The governor-signed view passes.
    assert_eq!(
        verify_release(&token, &view, &gov_pub),
        Ok(()),
        "actuator releases the governor-signed command"
    );

    // Tamper: a command the governor did NOT sign (one flipped byte) is refused —
    // the actuator will not release it (fail-closed, digest mismatch).
    let mut forged = view;
    forged.command[0] ^= 0xFF;
    assert_eq!(
        verify_release(&token, &forged, &gov_pub),
        Err(ReleaseDenied::DigestMismatch),
        "actuator must refuse a command the governor did not sign"
    );
}

#[test]
fn a_faulted_cycle_has_no_view_so_nothing_can_be_released() {
    let region = InProcessRegion::new();
    // Publish with an already-past deadline → the governor faults, no view.
    publish(&region, 0, &in_envelope().to_view(0, 1, 0, 1_000));

    let contract = VehicleKinematicsContract::nominal_reference_profile();
    let mut wm = AcceptedWatermark::new();
    let cycle = decide_cycle(&region, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
    assert_eq!(cycle.outcome, GovernorOutcome::SafeStop);
    assert!(
        cycle.view.is_none(),
        "a fault yields no view → there is nothing to sign or release"
    );
    // And the correct gate agrees: no actuatable view to sign.
    assert!(
        cycle.view_to_sign().is_none(),
        "SafeStop → view_to_sign() is None"
    );
}

#[test]
fn a_denied_command_is_received_but_never_signable() {
    // A command that PASSES transport/codec but FAILS the kinematic envelope:
    // `view` is populated (a command was received) yet the outcome is SafeStop.
    // This is the footgun `view_to_sign()` closes — `view.is_some()` but the
    // command must NEVER be signed for release.
    let region = InProcessRegion::new();
    let mut denied = in_envelope();
    // A zero time-step is FINITE (transport/codec pass — NaN/Inf would be rejected
    // earlier and yield no view) but the kinematic check DENIES it as
    // `InvalidTimeDelta` → DenyBreach → SafeStop. (An over-SPEED command would
    // merely clamp and actuate, so it is not a deny.)
    denied.delta_time_s = 0.0;
    publish(&region, 0, &denied.to_view(0, 1, 0, u64::MAX / 2));

    let contract = VehicleKinematicsContract::nominal_reference_profile();
    let mut wm = AcceptedWatermark::new();
    let cycle = decide_cycle(&region, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
    assert_eq!(
        cycle.outcome,
        GovernorOutcome::SafeStop,
        "over-envelope → SafeStop"
    );
    assert!(
        cycle.view.is_some(),
        "a command WAS received (transport/codec passed)"
    );
    assert!(
        cycle.view_to_sign().is_none(),
        "…but a denied command is never signable for release"
    );
}

#[test]
fn a_clamped_command_signs_the_enforced_bytes_not_the_proposal() {
    // The integrity claim under a Clamp: the governor must sign the ACTUATED
    // (clamped) bytes, not the guest's raw proposal — otherwise the bytes released
    // differ from the bytes signed. This is the regression test for that gap.
    let gov_key = SigningKey::from_bytes(&[9u8; 32]);
    let gov_pub = gov_key.verifying_key();

    // A command far over the speed cap → the governor CLAMPS it (Actuate with a
    // reduced velocity), so the actuated command differs from the proposal.
    let region = InProcessRegion::new();
    let mut over = in_envelope();
    over.linear_velocity_mps = 1_000.0;
    over.current_velocity_mps = 0.0;
    let proposal_view = over.to_view(0, 1, 0, u64::MAX / 2);
    publish(&region, 0, &proposal_view);

    let contract = VehicleKinematicsContract::nominal_reference_profile();
    let mut wm = AcceptedWatermark::new();
    let cycle = decide_cycle(&region, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);

    // It actuated a CLAMPED command — velocity reduced from the 1000 proposal.
    let actuated_velocity = match &cycle.outcome {
        GovernorOutcome::Actuate(c) => c.linear_velocity_mps,
        other => panic!("expected a clamp → Actuate, got {other:?}"),
    };
    assert!(
        actuated_velocity < 1_000.0,
        "velocity must have been clamped"
    );

    // The signable view is the ENFORCED view: it decodes to the clamped command,
    // NOT the guest's 1000 m/s proposal.
    let signed_view = *cycle.view_to_sign().expect("a clamp is actuatable");
    let signed_cmd =
        VehicleCommandPayload::from_validated_view(&signed_view).expect("enforced view decodes");
    assert_eq!(
        signed_cmd.linear_velocity_mps, actuated_velocity,
        "the signed bytes must be the CLAMPED (actuated) bytes, not the proposal"
    );

    // Sign + verify closes over the enforced bytes.
    let token = issue_release_token(&signed_view, &gov_key);
    assert_eq!(verify_release(&token, &signed_view, &gov_pub), Ok(()));

    // Critically: a token for the enforced command must NOT authorize the guest's
    // original, un-clamped proposal — proving we bound the actuated bytes.
    assert_eq!(
        verify_release(&token, &proposal_view, &gov_pub),
        Err(ReleaseDenied::DigestMismatch),
        "a token for the clamped command must not release the raw proposal"
    );
}
