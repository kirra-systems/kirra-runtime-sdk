// shm_peer — the GOVERNOR-side reader peer for the two-process host test
// (tests/two_process.rs). A separate OS process that maps the same POSIX shared
// region READ-ONLY (the R-HV-1 governor shape) and validates+decodes the command
// the guest process published, proving the frozen contract crosses a real
// address-space boundary.
//
// Args: <shm_name> <expected_sequence> <expected_linear_velocity_mps>
// Exit:  0 = a coherent snapshot validated, decoded, and matched the expected
//            sequence + linear velocity;  1 = any mismatch/fault (fail-closed).

use std::process::ExitCode;

use kirra_contract_channel::{
    read_coherent_snapshot, validate, AcceptedWatermark, VehicleCommandPayload,
    MAX_SNAPSHOT_RETRIES,
};
use kirra_hv_carrier::PosixShmReader;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: shm_peer <shm_name> <expected_seq> <expected_linvel>");
        return ExitCode::from(2);
    }
    let name = &args[1];
    let expected_seq: u64 = match args[2].parse() {
        Ok(v) => v,
        Err(_) => return ExitCode::from(2),
    };
    let expected_linvel: f64 = match args[3].parse() {
        Ok(v) => v,
        Err(_) => return ExitCode::from(2),
    };

    let reader = match PosixShmReader::open(name) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("shm_peer: open {name} failed: {e}");
            return ExitCode::from(1);
        }
    };

    let snapshot = match read_coherent_snapshot(&reader, MAX_SNAPSHOT_RETRIES) {
        Ok(s) => s,
        Err(f) => {
            eprintln!("shm_peer: snapshot fault: {f:?}");
            return ExitCode::from(1);
        }
    };
    // now = 0: the guest publishes a far-future deadline, so the freshness check
    // passes regardless of wall time (this test is about cross-process fidelity,
    // not the boundary clock).
    let wm = AcceptedWatermark::new();
    if let Err(fault) = validate(&snapshot, 0, &wm) {
        eprintln!("shm_peer: contract fault: {fault:?}");
        return ExitCode::from(1);
    }
    let cmd = match VehicleCommandPayload::from_validated_view(&snapshot) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("shm_peer: decode fault: {e:?}");
            return ExitCode::from(1);
        }
    };
    if snapshot.sequence != expected_seq || (cmd.linear_velocity_mps - expected_linvel).abs() > 1e-9
    {
        eprintln!(
            "shm_peer: mismatch: seq {} (want {expected_seq}), linvel {} (want {expected_linvel})",
            snapshot.sequence, cmd.linear_velocity_mps
        );
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
