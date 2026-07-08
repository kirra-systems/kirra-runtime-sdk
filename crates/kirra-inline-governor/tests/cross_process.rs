//! EP-01 — the in-line loop across a REAL address-space boundary: a guest
//! PROCESS (the demo bin's `--guest` role) publishes into a POSIX shared
//! region; this process runs governor + actuator. The rows the in-process
//! FDIT matrix cannot give: the release chain over an actual process boundary,
//! plus post-publish corruption injected through the raw RW mapping.

use std::process::Command;

use ed25519_dalek::SigningKey;
use kirra_contract_channel::{
    read_coherent_snapshot, ContractWriter, MAX_SNAPSHOT_RETRIES,
};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_hv_carrier::{PosixShmReader, PosixShmRegion};
use kirra_inline_governor::{
    govern_and_release, ActuatorStation, GovernorStation, ReleaseRefusal,
};

const FUTURE_DEADLINE: u64 = u64::MAX / 2;

fn spawn_guest(name: &str, linvel: f64, seq: u64, deadline: u64) -> bool {
    Command::new(env!("CARGO_BIN_EXE_inline_demo"))
        .args(["--guest", name, &linvel.to_string(), &seq.to_string(), &deadline.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn stations() -> (GovernorStation, ActuatorStation) {
    let gov = GovernorStation::new(
        VehicleKinematicsContract::nominal_reference_profile(),
        SigningKey::from_bytes(&[21u8; 32]),
    );
    let act = ActuatorStation::new(gov.verifying_key());
    (gov, act)
}

/// Valid proposal from another PROCESS → verified token → released.
#[test]
fn cross_process_proposal_is_released_with_verified_token() {
    let name = format!("/kirra-inline-xp-ok-{}", std::process::id());
    let region = PosixShmRegion::create(&name).expect("create region");
    let reader = PosixShmReader::open(&name).expect("governor RO mapping");
    let (mut gov, mut act) = stations();

    assert!(spawn_guest(&name, 10.0, 1, FUTURE_DEADLINE), "guest publish");
    let released = govern_and_release(&mut gov, &mut act, &reader, 0)
        .expect("a cross-process in-envelope proposal must release");
    assert_eq!(released.sequence, 1);
    assert_eq!(released.command.linear_velocity_mps, 10.0);
    drop(region);
}

/// Post-publish corruption (CRC flipped through the raw RW mapping, the way a
/// misbehaving co-resident writer would) → the assembled loop refuses.
#[test]
fn cross_process_corruption_never_releases() {
    let name = format!("/kirra-inline-xp-crc-{}", std::process::id());
    let region = PosixShmRegion::create(&name).expect("create region");
    let reader = PosixShmReader::open(&name).expect("governor RO mapping");
    let (mut gov, mut act) = stations();

    assert!(spawn_guest(&name, 10.0, 1, FUTURE_DEADLINE), "guest publish");
    // Corrupt the committed snapshot IN PLACE through the RW mapping: flip a
    // CRC bit in the body (the generation stays even/committed, so the reader
    // gets a coherent-but-invalid snapshot — the transport contract must catch it).
    let mut snap = read_coherent_snapshot(&reader, MAX_SNAPSHOT_RETRIES).expect("snapshot");
    snap.crc32 ^= 0x1;
    region.store_body(&snap);

    assert_eq!(
        govern_and_release(&mut gov, &mut act, &reader, 0),
        Err(ReleaseRefusal::NoToken),
        "a corrupted cross-process snapshot must produce no token and no release"
    );
    assert_eq!(act.last_released(), None);
    drop(region);
}

/// A governor signing with a DIFFERENT key than the actuator trusts → the
/// actuator refuses the release even for a fully valid proposal (key
/// provisioning is part of the trust chain).
#[test]
fn cross_process_wrong_governor_key_is_refused_at_the_actuator() {
    let name = format!("/kirra-inline-xp-key-{}", std::process::id());
    let region = PosixShmRegion::create(&name).expect("create region");
    let reader = PosixShmReader::open(&name).expect("governor RO mapping");

    let mut rogue_gov = GovernorStation::new(
        VehicleKinematicsContract::nominal_reference_profile(),
        SigningKey::from_bytes(&[66u8; 32]), // NOT the key the actuator trusts
    );
    let trusted = GovernorStation::new(
        VehicleKinematicsContract::nominal_reference_profile(),
        SigningKey::from_bytes(&[21u8; 32]),
    );
    let mut act = ActuatorStation::new(trusted.verifying_key());

    assert!(spawn_guest(&name, 10.0, 1, FUTURE_DEADLINE), "guest publish");
    let outcome = govern_and_release(&mut rogue_gov, &mut act, &reader, 0);
    assert!(
        matches!(outcome, Err(ReleaseRefusal::Denied(_))),
        "a token from an untrusted signer must be refused at the actuator, got {outcome:?}"
    );
    assert_eq!(act.last_released(), None);
    drop(region);
}
