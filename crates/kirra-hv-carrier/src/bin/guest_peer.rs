// guest_peer — the GUEST-side writer peer for the two-process governor test
// (tests/governor_two_process.rs). A separate OS process that OPENS the shared
// region (created by the governor/test process) and publishes one proposal, so
// the governor process reads + validates + decodes + BOUNDS a command that
// genuinely crossed an address-space boundary.
//
// Args: <shm_name> <linear_velocity_mps> <deadline_nanos>
// Exit:  0 = published;  1 = open failed;  2 = bad args.
//
// The payload fixes current == desired (accel 0) and a small steering (1 deg),
// so the governor's decision turns ONLY on the absolute speed bound — making the
// two-process assertions predictable.

use std::process::ExitCode;

use kirra_contract_channel::{publish, VehicleCommandPayload};
use kirra_hv_carrier::PosixShmRegion;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: guest_peer <shm_name> <linear_velocity_mps> <deadline_nanos>");
        return ExitCode::from(2);
    }
    let name = &args[1];
    let linear: f64 = match args[2].parse() {
        Ok(v) => v,
        Err(_) => return ExitCode::from(2),
    };
    let deadline: u64 = match args[3].parse() {
        Ok(v) => v,
        Err(_) => return ExitCode::from(2),
    };

    // The guest OPENS (never creates) the region — the governor/platform owns it.
    let region = match PosixShmRegion::open(name) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("guest_peer: open {name} failed: {e}");
            return ExitCode::from(1);
        }
    };
    let payload = VehicleCommandPayload {
        linear_velocity_mps: linear,
        current_velocity_mps: linear, // == desired → accel 0
        delta_time_s: 0.1,
        steering_angle_deg: 1.0, // small → never trips the steering bound
        current_steering_angle_deg: 1.0,
    };
    let body = payload.to_view(0, 1, 0, deadline);
    publish(&region, 0, &body);
    ExitCode::SUCCESS
}
