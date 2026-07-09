//! inline_demo — the EP-01 cross-process demo: a guest PROCESS publishes
//! proposals into a POSIX shared region; THIS process runs the in-line
//! governor→actuator loop (`GovernorStation` → release token →
//! `ActuatorStation`) and prints what was released vs refused, plus an
//! INDICATIVE latency summary of the full in-line step. No HTTP anywhere.
//!
//! Roles (self-spawning, like kirra-l3-e2e):
//! ```text
//! inline_demo                                             — governor + actuator (the demo)
//! inline_demo --guest <shm> <linvel> <seq> <deadline_ns>  — guest publisher
//! ```
//!
//! Exit 0 iff every scripted cycle produced its expected outcome. Timing is
//! host-INDICATIVE — never a WCET claim (the QNX-target measurement is the
//! certified path; see the crate README).

use std::process::{Command, ExitCode};
use std::time::Instant;

use ed25519_dalek::SigningKey;
use kirra_contract_channel::{publish, ContractReader, VehicleCommandPayload};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_hv_carrier::{PosixShmReader, PosixShmRegion};
use kirra_inline_governor::{govern_and_release, ActuatorStation, GovernorStation};

const FUTURE_DEADLINE: u64 = u64::MAX / 2;

fn payload(linear: f64) -> VehicleCommandPayload {
    VehicleCommandPayload {
        linear_velocity_mps: linear,
        current_velocity_mps: linear,
        delta_time_s: 0.1,
        steering_angle_deg: 1.0,
        current_steering_angle_deg: 1.0,
    }
}

fn guest_main(args: &[String]) -> ExitCode {
    if args.len() != 4 {
        eprintln!("usage: inline_demo --guest <shm> <linvel> <seq> <deadline_ns>");
        return ExitCode::from(2);
    }
    let (Ok(linvel), Ok(seq), Ok(deadline)) = (
        args[1].parse::<f64>(),
        args[2].parse::<u64>(),
        args[3].parse::<u64>(),
    ) else {
        return ExitCode::from(2);
    };
    let region = match PosixShmRegion::open(&args[0]) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("guest: open {} failed: {e}", args[0]);
            return ExitCode::from(1);
        }
    };
    let committed = region.load_generation();
    if committed % 2 != 0 {
        eprintln!("guest: region generation is odd (torn)");
        return ExitCode::from(1);
    }
    publish(
        &region,
        committed,
        &payload(linvel).to_view(committed, seq, 0, deadline),
    );
    ExitCode::SUCCESS
}

fn spawn_guest(name: &str, linvel: f64, seq: u64, deadline: u64) -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    Command::new(exe)
        .args([
            "--guest",
            name,
            &linvel.to_string(),
            &seq.to_string(),
            &deadline.to_string(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn governor_main() -> ExitCode {
    println!("===== KIRRA IN-LINE GOVERNOR DEMO (EP-01) =====");
    println!("writer process → SHM → governor (decide+sign) → actuator (verify-before-release)");
    println!("no HTTP on the enforced path; timing below is host-INDICATIVE, never WCET\n");

    let name = format!("/kirra-inline-demo-{}", std::process::id());
    let region = PosixShmRegion::create(&name).expect("create SHM region");
    let reader = PosixShmReader::open(&name).expect("governor read-only mapping");

    let mut governor = GovernorStation::new(
        VehicleKinematicsContract::nominal_reference_profile(),
        // A demo-only key (deterministic; provenance is this demo, never production).
        SigningKey::from_bytes(&[42u8; 32]),
    );
    let mut actuator = ActuatorStation::new(governor.verifying_key());

    // The scripted cycles: (label, linvel, seq, deadline, expect_release).
    let script: &[(&str, f64, u64, u64, bool)] = &[
        ("in-envelope 10 m/s", 10.0, 1, FUTURE_DEADLINE, true),
        (
            "over-envelope 50 m/s (clamped)",
            50.0,
            2,
            FUTURE_DEADLINE,
            true,
        ),
        ("expired deadline", 10.0, 3, 1, false),
        ("replayed sequence 2", 10.0, 2, FUTURE_DEADLINE, false),
        ("recovery at sequence 4", 12.0, 4, FUTURE_DEADLINE, true),
    ];

    let mut all_ok = true;
    for (label, linvel, seq, deadline, expect_release) in script {
        let published = spawn_guest(&name, *linvel, *seq, *deadline);
        // now=2 so the `deadline_ns=1` row reads as expired without wall-clock coupling.
        let outcome = govern_and_release(&mut governor, &mut actuator, &reader, 2);
        let released = outcome.is_ok();
        let ok = published && released == *expect_release;
        all_ok &= ok;
        match &outcome {
            Ok(r) => println!(
                "{:<34} {}  RELEASED seq={} v={:.1} m/s",
                label,
                if ok { "PASS" } else { "FAIL" },
                r.sequence,
                r.command.linear_velocity_mps
            ),
            Err(refusal) => println!(
                "{:<34} {}  REFUSED ({refusal:?}) → MRC hold",
                label,
                if ok { "PASS" } else { "FAIL" }
            ),
        }
    }

    // INDICATIVE latency of the FULL in-line step (read → validate → bound →
    // sign → verify → release-decision), in the PRODUCTION shape: the same
    // long-lived station pair across every cycle, each iteration a fresh
    // strictly-advancing sequence (published in-process, outside the timer —
    // this demo process owns the RW mapping).
    let iters: u64 = std::env::var("KIRRA_INLINE_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(2_000);
    let mut samples = Vec::with_capacity(iters as usize);
    for i in 0..iters {
        let seq = 100 + i; // strictly above the script's last released sequence
        let gen = region.load_generation();
        publish(
            &region,
            gen,
            &payload(10.0).to_view(gen, seq, 0, FUTURE_DEADLINE),
        );
        let t0 = Instant::now();
        let out = govern_and_release(&mut governor, &mut actuator, &reader, 0);
        let dt = t0.elapsed();
        assert!(
            out.is_ok(),
            "latency loop cycle {i} unexpectedly refused: {out:?}"
        );
        std::hint::black_box(out.is_ok());
        samples.push(dt);
    }
    samples.sort_unstable();
    let pct = |p: f64| {
        samples[(((p / 100.0) * iters as f64).ceil() as usize).clamp(1, iters as usize) - 1]
    };
    println!(
        "\nfull in-line step (read+validate+bound+sign+verify), {iters} iters — INDICATIVE:\n\
         p50={:?}  p99={:?}  p99.9={:?}  max={:?}",
        pct(50.0),
        pct(99.0),
        pct(99.9),
        samples.last().unwrap()
    );

    drop(region);
    println!(
        "\n===== DEMO {} =====",
        if all_ok { "PASS" } else { "FAIL" }
    );
    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "--guest" {
        guest_main(&args[2..])
    } else {
        governor_main()
    }
}
