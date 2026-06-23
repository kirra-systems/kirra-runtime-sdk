// crates/kirra-governor-service/src/main.rs
//
// kirra-governor-service — minimal over-the-wire (UDP) KIRRA governor for the
// two-box governed-car prototype (docs/adr/KIRRA_BRINGUP_RUNBOOK.md, Prompt A).
//
// It wraps the EXISTING verdict core — the FROZEN kinematics-contract talisman —
// which now lives in the lean `kirra-core` crate (de-monolith Stage 3). This binary
// depends on that crate directly; because `kirra-core` imports only `serde` + `std`,
// it pulls in nothing heavy: this binary's entire dependency tree is serde + bincode
// + std — NO tokio, ROS 2, r2r, or DDS, per ADR-0001 (the governor is the minimal,
// async/ROS-free checker; the QNX cert target has none of those anyway). The contract
// logic is the real, unmodified one — the talisman is never forked, only relocated.
//
// PROTOTYPE STAGE (QM, not the cert build): regular Rust over UDP. The
// Ferrocene / `no_std` / ASIL-D factoring and the shared-memory mailbox are a
// later stage and do not block the demo — see ADR-0001 and the bring-up runbook.

use kirra_core::kinematics_contract::{
    validate_vehicle_command, DenyCode, EnforceAction, ProposedVehicleCommand,
    VehicleKinematicsContract,
};
use serde::{Deserialize, Serialize};
use std::net::UdpSocket;

/// Default listen address (override with `KIRRA_GOVERNOR_ADDR`).
const DEFAULT_ADDR: &str = "0.0.0.0:9760";

/// Wire request, car -> governor. Carries ONLY the proposed command: the safety
/// envelope (`VehicleKinematicsContract`) is the GOVERNOR's policy, never the
/// doer's to choose. `ProposedVehicleCommand` is the verdict core's real input
/// type (serde-(de)serializable in the core) — no kinematic fields are invented
/// here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    /// Monotonic request sequence (echoed in the verdict; basis for the M6 watchdog).
    pub seq: u64,
    /// Car-side send timestamp (nanoseconds). Used for staleness once the M6
    /// watchdog is wired; carried now so the schema is stable.
    pub ts_nanos: u128,
    /// The proposed command — the verdict core's real input type, verbatim.
    pub command: ProposedVehicleCommand,
}

/// Wire response, governor -> car. `action` is the verdict core's own output
/// (`EnforceAction`, Serialize-only in the core, which is all a one-way encode
/// needs). `reason_code` is a stable numeric a car can branch on WITHOUT
/// deserializing `EnforceAction`.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub seq: u64,
    pub action: EnforceAction,
    pub reason_code: u32,
}

/// Stable numeric reason code: `0` = no breach (Accept / Clamp); `1..=10` map
/// 1:1 to `DenyCode`. Kept as an explicit match so adding a `DenyCode` variant
/// upstream forces a compile error here (no silent gap).
fn reason_code(action: &EnforceAction) -> u32 {
    match action {
        EnforceAction::Allow
        | EnforceAction::ClampLinear(_)
        | EnforceAction::ClampSteering(_) => 0,
        EnforceAction::DenyBreach(code) => deny_code_num(*code),
    }
}

fn deny_code_num(code: DenyCode) -> u32 {
    match code {
        DenyCode::NanInfLinearVelocity => 1,
        DenyCode::NanInfCurrentVelocity => 2,
        DenyCode::NanInfSteeringAngle => 3,
        DenyCode::NanInfCurrentSteering => 4,
        DenyCode::NanInfDeltaTime => 5,
        DenyCode::InvalidTimeDelta => 6,
        DenyCode::AssetLockedOut => 7,
        DenyCode::DrivableSpaceDeparture => 8,
        DenyCode::DegradedReinitiationDenied => 9,
        DenyCode::DegradedSpeedIncreaseDenied => 10,
    }
}

/// The decision: run the verdict core VERBATIM against the governor's contract.
/// Pure (no I/O) so it is unit-testable without a socket.
fn decide(proposal: &Proposal, contract: &VehicleKinematicsContract) -> Verdict {
    let action = validate_vehicle_command(&proposal.command, contract);
    let reason_code = reason_code(&action);
    Verdict {
        seq: proposal.seq,
        action,
        reason_code,
    }
}

/// Minimal M6 watchdog state (staleness check stubbed for the prototype; the
/// safe-state wiring comes later per the runbook). For now it only flags a
/// non-monotonic sequence, which would indicate a reordered/replayed proposal.
#[derive(Default)]
struct WatchdogState {
    last_seq: Option<u64>,
    last_ts_nanos: Option<u128>,
}

impl WatchdogState {
    /// Records the proposal and returns `false` if the sequence did not advance
    /// (the caller logs it). Staleness-vs-deadline and the safe-state emission
    /// are deliberately NOT implemented yet (M6).
    fn observe(&mut self, proposal: &Proposal) -> bool {
        let monotonic = self.last_seq.map(|p| proposal.seq > p).unwrap_or(true);
        self.last_seq = Some(proposal.seq);
        self.last_ts_nanos = Some(proposal.ts_nanos);
        monotonic
    }
}

fn main() -> std::io::Result<()> {
    let addr = std::env::var("KIRRA_GOVERNOR_ADDR").unwrap_or_else(|_| DEFAULT_ADDR.to_string());

    // The governor enforces its OWN envelope. Nominal reference profile for the
    // prototype; the MRC fallback profile is available for a degraded mode.
    let contract = VehicleKinematicsContract::nominal_reference_profile();

    let socket = UdpSocket::bind(&addr)?;
    eprintln!(
        "kirra-governor-service: listening on {addr} (UDP), \
         contract = nominal_reference_profile, effective_max_speed = {:.2} m/s",
        contract.effective_max_speed_mps()
    );

    let mut watchdog = WatchdogState::default();
    // One UDP datagram per proposal; 64 KiB is far above the fixed-schema size.
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let (n, peer) = socket.recv_from(&mut buf)?;

        let proposal: Proposal = match bincode::deserialize(&buf[..n]) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("decode error from {peer}: {e}");
                continue;
            }
        };

        if !watchdog.observe(&proposal) {
            eprintln!(
                "watchdog: non-monotonic seq {} from {peer} (staleness/safe-state is M6, stubbed)",
                proposal.seq
            );
        }

        let verdict = decide(&proposal, &contract);

        match bincode::serialize(&verdict) {
            Ok(bytes) => {
                if let Err(e) = socket.send_to(&bytes, peer) {
                    eprintln!("send error to {peer}: {e}");
                }
            }
            Err(e) => eprintln!("encode error for seq {}: {e}", verdict.seq),
        }
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;

    fn steady(linear: f64, steering: f64) -> Proposal {
        Proposal {
            seq: 1,
            ts_nanos: 0,
            command: ProposedVehicleCommand {
                linear_velocity_mps: linear,
                current_velocity_mps: linear,
                delta_time_s: 0.1,
                steering_angle_deg: steering,
                current_steering_angle_deg: steering,
            },
        }
    }

    #[test]
    fn steady_low_speed_command_is_accepted() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let v = decide(&steady(1.0, 0.0), &contract);
        assert_eq!(v.action, EnforceAction::Allow, "a steady 1 m/s straight command must pass");
        assert_eq!(v.reason_code, 0);
        assert_eq!(v.seq, 1);
    }

    #[test]
    fn nan_linear_velocity_is_denied_with_code_1() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let v = decide(&steady(f64::NAN, 0.0), &contract);
        assert_eq!(
            v.action,
            EnforceAction::DenyBreach(DenyCode::NanInfLinearVelocity),
            "NaN linear velocity must be denied by the verdict core verbatim"
        );
        assert_eq!(v.reason_code, 1, "NaN-linear maps to reason_code 1");
    }

    #[test]
    fn verdict_echoes_request_seq() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let mut p = steady(1.0, 0.0);
        p.seq = 42;
        assert_eq!(decide(&p, &contract).seq, 42);
    }

    #[test]
    fn proposal_round_trips_over_bincode() {
        // The decode side of the wire (Proposal: Deserialize) must round-trip.
        let p = steady(2.5, 3.0);
        let bytes = bincode::serialize(&p).expect("encode");
        let back: Proposal = bincode::deserialize(&bytes).expect("decode");
        assert_eq!(back.seq, p.seq);
        assert_eq!(back.command.linear_velocity_mps, p.command.linear_velocity_mps);
        assert_eq!(back.command.steering_angle_deg, p.command.steering_angle_deg);
    }

    #[test]
    fn verdict_serializes_for_the_wire() {
        // The encode side of the wire (Verdict: Serialize) must succeed for both
        // an accept and a deny.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let accept = decide(&steady(1.0, 0.0), &contract);
        let deny = decide(&steady(f64::INFINITY, 0.0), &contract);
        assert!(bincode::serialize(&accept).is_ok());
        assert!(bincode::serialize(&deny).is_ok());
    }

    #[test]
    fn watchdog_flags_non_monotonic_seq() {
        let mut wd = WatchdogState::default();
        let mut p = steady(1.0, 0.0);
        p.seq = 5;
        assert!(wd.observe(&p), "first observation is always monotonic");
        p.seq = 4;
        assert!(!wd.observe(&p), "a lower seq must be flagged as non-monotonic");
        p.seq = 6;
        assert!(wd.observe(&p), "an advancing seq is monotonic again");
    }
}
