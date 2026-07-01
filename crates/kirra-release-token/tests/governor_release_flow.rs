// End-to-end governor → actuator release flow (HVCHAN §3 / ADR-0030 Clause F):
// a proposal is published, the GOVERNOR validates + bounds it (`decide_cycle`),
// signs the exact validated view, and the ACTUATOR verifies the token before it
// would release — refusing anything the governor did not sign. Ties the L3
// consumer (kirra-core) to the release bridge (this crate).

use ed25519_dalek::SigningKey;

use kirra_contract_channel::reference::InProcessRegion;
use kirra_contract_channel::{publish, AcceptedWatermark, VehicleCommandPayload, MAX_SNAPSHOT_RETRIES};
use kirra_core::contract_consumer::{decide_cycle, GovernorOutcome};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_release_token::{sign_view, verify_view};

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
    assert!(matches!(cycle.outcome, GovernorOutcome::Actuate(_)), "in-envelope must actuate");
    let view = cycle.view.expect("actuatable → a view to sign");
    let token = sign_view(&view, &gov_key);

    // Actuator: verify BEFORE releasing. The governor-signed view passes.
    assert!(verify_view(&view, &token, &gov_pub), "actuator releases the governor-signed command");

    // Tamper: a command the governor did NOT sign (one flipped byte) is refused —
    // the actuator will not release it (fail-closed).
    let mut forged = view;
    forged.command[0] ^= 0xFF;
    assert!(
        !verify_view(&forged, &token, &gov_pub),
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
    assert!(cycle.view.is_none(), "a fault yields no view → there is nothing to sign or release");
}
