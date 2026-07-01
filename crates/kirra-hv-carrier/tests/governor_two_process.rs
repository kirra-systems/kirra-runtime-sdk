// Two-process producer→GOVERNOR test (ADR-0030 incr. 3). A guest process
// (`guest_peer`) publishes a proposal into a POSIX shared region; THIS process is
// the governor — it reads the region and runs the L3.3 `decide` (snapshot →
// validate → decode → bound) end-to-end, asserting the actuate-vs-safe-stop
// outcome. This is the full doer→checker path across a real address-space
// boundary — the step the in-process `InProcessRegion` tests cannot give — and
// the host analogue of the QNX guest-partition → governor-partition path.

use std::process::Command;

use kirra_contract_channel::{AcceptedWatermark, MAX_SNAPSHOT_RETRIES};
use kirra_core::contract_consumer::{decide, GovernorOutcome};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_hv_carrier::{PosixShmReader, PosixShmRegion};

fn guest_peer() -> Command {
    Command::new(env!("CARGO_BIN_EXE_guest_peer"))
}

const FUTURE_DEADLINE: u64 = u64::MAX / 2;

#[test]
fn governor_process_actuates_a_guest_in_envelope_proposal() {
    let name = format!("/kirra-hv-gov-{}", std::process::id());
    // The governor/test owns the region; the guest process opens it to publish.
    let region = PosixShmRegion::create(&name).expect("create");
    let contract = VehicleKinematicsContract::nominal_reference_profile();

    let status = guest_peer()
        .arg(&name)
        .arg("10.0")
        .arg(FUTURE_DEADLINE.to_string())
        .status()
        .expect("spawn guest_peer");
    assert!(status.success(), "guest publish failed: {status:?}");

    // The governor reads via a READ-ONLY mapping (R-HV-1); the region handle
    // stays alive to keep the object mapped + unlink it on drop.
    let reader = PosixShmReader::open(&name).expect("governor read-only mapping");
    let mut wm = AcceptedWatermark::new();
    match decide(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES) {
        GovernorOutcome::Actuate(c) => assert_eq!(c.linear_velocity_mps, 10.0),
        GovernorOutcome::SafeStop => panic!("an in-envelope cross-process proposal must actuate"),
    }
    assert_eq!(wm.last(), Some((2, 1))); // consumed the guest's seq 1 (gen 2)
    drop(region);
}

#[test]
fn governor_process_bounds_a_guest_over_envelope_proposal() {
    let name = format!("/kirra-hv-gov-over-{}", std::process::id());
    let region = PosixShmRegion::create(&name).expect("create");
    let contract = VehicleKinematicsContract::nominal_reference_profile();

    // 50 m/s, far over the 35 m/s ceiling.
    let status = guest_peer()
        .arg(&name)
        .arg("50.0")
        .arg(FUTURE_DEADLINE.to_string())
        .status()
        .expect("spawn guest_peer");
    assert!(status.success());

    // guest_peer sets current == desired (accel 0) + small steering, so the ONLY
    // envelope breach is the absolute speed → the nominal contract CLAMPS
    // (ClampLinear), and decide() must Actuate the clamped command (not SafeStop —
    // that would mask a clamping regression).
    let reader = PosixShmReader::open(&name).expect("governor read-only mapping");
    let mut wm = AcceptedWatermark::new();
    match decide(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES) {
        GovernorOutcome::Actuate(c) => assert!(
            c.linear_velocity_mps <= 35.0, // clamped into the 35 m/s envelope (< the 50 proposal)
            "over-speed must be clamped into the envelope, got {}",
            c.linear_velocity_mps
        ),
        GovernorOutcome::SafeStop => panic!("nominal over-speed must clamp (Actuate), not safe-stop"),
    }
    drop(region);
}

#[test]
fn governor_process_safe_stops_on_a_guest_expired_deadline() {
    let name = format!("/kirra-hv-gov-dl-{}", std::process::id());
    let region = PosixShmRegion::create(&name).expect("create");
    let contract = VehicleKinematicsContract::nominal_reference_profile();

    // Guest publishes with deadline 1_000; the governor's now is 5_000 → expired.
    let status = guest_peer()
        .arg(&name)
        .arg("10.0")
        .arg("1000")
        .status()
        .expect("spawn guest_peer");
    assert!(status.success());

    let reader = PosixShmReader::open(&name).expect("governor read-only mapping");
    let mut wm = AcceptedWatermark::new();
    assert_eq!(
        decide(&reader, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES),
        GovernorOutcome::SafeStop,
        "an expired-deadline proposal must fail closed across processes"
    );
    drop(region);
}
