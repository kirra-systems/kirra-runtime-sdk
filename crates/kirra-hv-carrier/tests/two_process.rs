// Two-process host integration test (ADR-0030 Clause B): a guest process (this
// test) creates + publishes into a POSIX shared region; a SEPARATE governor
// process (the `shm_peer` bin) maps it READ-ONLY and validates+decodes the
// command. This is the real cross-address-space proof the in-process reference
// carrier (InProcessRegion) cannot give — the step the QNX HvRegion then only
// swaps the map primitive on.

use std::process::Command;

use kirra_contract_channel::{publish, VehicleCommandPayload};
use kirra_hv_carrier::PosixShmRegion;

fn demo(seq: u64) -> VehicleCommandPayload {
    VehicleCommandPayload {
        linear_velocity_mps: 3.0 + seq as f64,
        current_velocity_mps: 2.5,
        delta_time_s: 0.1,
        steering_angle_deg: -4.0,
        current_steering_angle_deg: -3.5,
    }
}

fn shm_peer() -> Command {
    // Cargo exposes the peer bin's path to integration tests.
    Command::new(env!("CARGO_BIN_EXE_shm_peer"))
}

#[test]
fn governor_process_reads_the_guest_process_command() {
    let name = format!("/kirra-hv-2p-{}", std::process::id());
    // Guest: create the region and publish a known command BEFORE spawning the
    // reader (latest-value-wins; the value persists in the mapping).
    let guest = PosixShmRegion::create(&name).expect("create");
    let payload = demo(5); // linear_velocity_mps = 8.0, sequence 5
    let body = payload.to_view(0, 5, 0, u64::MAX / 2);
    publish(&guest, 0, &body);

    // Governor process maps read-only and must validate/decode/match.
    let status = shm_peer()
        .arg(&name)
        .arg("5")
        .arg(format!("{}", payload.linear_velocity_mps))
        .status()
        .expect("spawn shm_peer");
    assert!(status.success(), "peer should accept the published command: {status:?}");

    // `guest` still owns the region here; it unlinks on drop after the peer ran.
    drop(guest);
}

#[test]
fn governor_process_rejects_a_sequence_mismatch() {
    // Negative control: the peer expecting the WRONG sequence must fail closed,
    // proving the test genuinely reads across the boundary (not a vacuous pass).
    let name = format!("/kirra-hv-2p-neg-{}", std::process::id());
    let guest = PosixShmRegion::create(&name).expect("create");
    let payload = demo(1);
    let body = payload.to_view(0, 1, 0, u64::MAX / 2);
    publish(&guest, 0, &body);

    let status = shm_peer()
        .arg(&name)
        .arg("999") // wrong sequence
        .arg(format!("{}", payload.linear_velocity_mps))
        .status()
        .expect("spawn shm_peer");
    assert!(!status.success(), "peer must reject a sequence mismatch");

    drop(guest);
}
