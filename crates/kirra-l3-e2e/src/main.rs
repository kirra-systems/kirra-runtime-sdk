//! kirra_l3_e2e — the on-target L3 end-to-end harness (ADR-0030 incr. 4 Phase-I).
//!
//! Runs the ENTIRE L3 doer→checker→release chain across two real OS processes
//! over the POSIX-SHM carrier, then measures the governor path. Cross-compiles
//! for `x86_64-pc-nto-qnx800` (QNX 8.0) and runs identically on host Linux —
//! host runs verify the harness; the QNX run supplies the target evidence.
//!
//! Row gate = VERDICT CORRECTNESS only (the #274 harness discipline). Timing is
//! printed with the honesty banner: host/VM numbers are INDICATIVE, never WCET;
//! the results-file label (e.g. INDICATIVE-KVM) is applied when recording.
//!
//! Roles:
//!   kirra_l3_e2e                                     — governor/orchestrator (the matrix + timing)
//!   kirra_l3_e2e --guest <shm> <linvel> <seq> <deadline_ns>   — guest publisher (spawned per row)
//!
//! Env: KIRRA_LAT_ITERS (default 100_000), KIRRA_E2E_FIFO=1 (attempt SCHED_FIFO).

use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;

use kirra_contract_channel::{
    publish, read_coherent_snapshot, AcceptedWatermark, ContractReader, VehicleCommandPayload,
    MAX_SNAPSHOT_RETRIES,
};
use kirra_core::contract_consumer::{decide_cycle, GovernorOutcome};
use kirra_core::kinematics_contract::VehicleKinematicsContract;
use kirra_hv_carrier::{PosixShmReader, PosixShmRegion};
use kirra_release_token::{issue_release_token, verify_release, ReleaseDenied};

const FUTURE_DEADLINE: u64 = u64::MAX / 2;

/// The harness governor signing key — a FIXED TEST key (deterministic, no RNG on
/// target). Provenance is this harness; never a production identity.
fn governor_key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn payload(linear: f64) -> VehicleCommandPayload {
    VehicleCommandPayload {
        linear_velocity_mps: linear,
        current_velocity_mps: linear, // == desired → accel 0; the speed bound decides
        delta_time_s: 0.1,
        steering_angle_deg: 1.0,
        current_steering_angle_deg: 1.0,
    }
}

// ---------------------------------------------------------------------------
// Guest role — a separate PROCESS that opens (never creates) the region and
// publishes one proposal. Mirrors the node.rs open-only ownership discipline.
// ---------------------------------------------------------------------------
fn guest_main(args: &[String]) -> ExitCode {
    if args.len() != 4 {
        eprintln!("usage: kirra_l3_e2e --guest <shm> <linvel> <seq> <deadline_ns>");
        return ExitCode::from(2);
    }
    let (name, linvel, seq, deadline) = (&args[0], &args[1], &args[2], &args[3]);
    let (Ok(linvel), Ok(seq), Ok(deadline)) =
        (linvel.parse::<f64>(), seq.parse::<u64>(), deadline.parse::<u64>())
    else {
        return ExitCode::from(2);
    };
    let region = match PosixShmRegion::open(name) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("guest: open {name} failed: {e}");
            return ExitCode::from(1);
        }
    };
    // Publish on top of the region's current committed (even) generation so a
    // re-spawned guest advances the seqlock like a live producer would.
    let committed = region.load_generation();
    if committed % 2 != 0 {
        eprintln!("guest: region generation is odd (writer died mid-publish?)");
        return ExitCode::from(1);
    }
    let body = payload(linvel).to_view(committed, seq, 0, deadline);
    publish(&region, committed, &body);
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// Governor role — the matrix + timing.
// ---------------------------------------------------------------------------

/// Spawn ourselves as the guest for `name` and wait for the publish to complete.
fn spawn_guest(name: &str, linvel: f64, seq: u64, deadline: u64) -> bool {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };
    Command::new(exe)
        .arg("--guest")
        .arg(name)
        .arg(linvel.to_string())
        .arg(seq.to_string())
        .arg(deadline.to_string())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Print one matrix row and return its pass/fail (the only thing the gate needs).
fn row(id: &str, what: &str, pass: bool, detail: String) -> bool {
    println!("{id:<7} {what:<52} {}  {detail}", if pass { "PASS" } else { "FAIL" });
    pass
}

/// Attempt SCHED_FIFO at max priority on the calling thread (POSIX
/// `pthread_setschedparam` — present on both Linux and QNX/nto). Returns whether
/// it was granted (needs privilege; without it the run is time-shared).
fn try_fifo() -> bool {
    // SAFETY: a zeroed sched_param is valid; we set only sched_priority and pass
    // SCHED_FIFO for the calling thread. Pure FFI, no invariants touched.
    unsafe {
        let mut p: libc::sched_param = std::mem::zeroed();
        p.sched_priority = libc::sched_get_priority_max(libc::SCHED_FIFO);
        libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &p) == 0
    }
}

fn percentile(sorted: &[Duration], pct: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = (((pct / 100.0) * n as f64).ceil() as usize).clamp(1, n);
    sorted[rank - 1].as_nanos()
}

fn print_timing(name: &str, mut samples: Vec<Duration>) {
    samples.sort_unstable();
    println!(
        "{name:<34} {:>9} {:>9} {:>9} {:>11}",
        percentile(&samples, 50.0),
        percentile(&samples, 99.0),
        percentile(&samples, 99.9),
        samples.last().copied().unwrap_or(Duration::ZERO).as_nanos(),
    );
}

fn governor_main() -> ExitCode {
    let contract = VehicleKinematicsContract::nominal_reference_profile();
    let gov_key = governor_key();
    let gov_pub = gov_key.verifying_key();
    let pid = std::process::id();
    let mut rows: Vec<bool> = Vec::new();

    println!("===== KIRRA-L3E2E START =====");
    println!(
        "platform: os={} arch={}  (row gate = verdict correctness; timing INDICATIVE)",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!();

    // ---- L3-01: in-envelope proposal → Actuate unchanged + token verifies ----
    {
        let name = format!("/kirra-l3e2e-01-{pid}");
        let region = PosixShmRegion::create(&name).expect("create region");
        let published = spawn_guest(&name, 10.0, 1, FUTURE_DEADLINE);
        let reader = PosixShmReader::open(&name).expect("governor RO mapping");
        let mut wm = AcceptedWatermark::new();
        let cycle = decide_cycle(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
        let actuated = matches!(&cycle.outcome, GovernorOutcome::Actuate(c) if c.linear_velocity_mps == 10.0);
        let (signed, verified) = match cycle.view_to_sign() {
            Some(view) => {
                let token = issue_release_token(view, &gov_key);
                (true, verify_release(&token, view, &gov_pub).is_ok())
            }
            None => (false, false),
        };
        rows.push(row(
            "L3-01",
            "in-envelope: Actuate(10.0) + token verifies",
            published && actuated && signed && verified,
            format!("published={published} actuated={actuated} signed={signed} verified={verified}"),
        ));
        drop(region);
    }

    // ---- L3-02: over-envelope → clamped; token binds the ENFORCED bytes ----
    {
        let name = format!("/kirra-l3e2e-02-{pid}");
        let region = PosixShmRegion::create(&name).expect("create region");
        let published = spawn_guest(&name, 50.0, 1, FUTURE_DEADLINE);
        let reader = PosixShmReader::open(&name).expect("governor RO mapping");
        // The raw proposal view, for the must-NOT-release check below.
        let proposal_view = read_coherent_snapshot(&reader, MAX_SNAPSHOT_RETRIES).expect("snapshot");
        let mut wm = AcceptedWatermark::new();
        let cycle = decide_cycle(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
        let clamped_vel = match &cycle.outcome {
            GovernorOutcome::Actuate(c) => Some(c.linear_velocity_mps),
            GovernorOutcome::SafeStop => None,
        };
        let clamped = clamped_vel.is_some_and(|v| v <= 35.0);
        let enforced_bound = match cycle.view_to_sign() {
            Some(view) => {
                let token = issue_release_token(view, &gov_key);
                let enforced_ok = verify_release(&token, view, &gov_pub).is_ok();
                // The token for the CLAMPED bytes must not release the raw proposal.
                let proposal_refused = verify_release(&token, &proposal_view, &gov_pub)
                    == Err(ReleaseDenied::DigestMismatch);
                enforced_ok && proposal_refused
            }
            None => false,
        };
        rows.push(row(
            "L3-02",
            "over-envelope 50: clamped <=35; token binds enforced bytes",
            published && clamped && enforced_bound,
            format!("published={published} clamped_vel={clamped_vel:?} enforced_bound={enforced_bound}"),
        ));
        drop(region);
    }

    // ---- L3-03: expired deadline → SafeStop, nothing signable ----
    {
        let name = format!("/kirra-l3e2e-03-{pid}");
        let region = PosixShmRegion::create(&name).expect("create region");
        let published = spawn_guest(&name, 10.0, 1, 1_000); // deadline 1_000, now 5_000
        let reader = PosixShmReader::open(&name).expect("governor RO mapping");
        let mut wm = AcceptedWatermark::new();
        let cycle = decide_cycle(&reader, &mut wm, 5_000, &contract, MAX_SNAPSHOT_RETRIES);
        let safe = cycle.outcome == GovernorOutcome::SafeStop && cycle.view_to_sign().is_none();
        rows.push(row(
            "L3-03",
            "expired deadline: SafeStop, nothing signable",
            published && safe,
            format!("published={published} outcome={:?} signable={}", cycle.outcome, cycle.view_to_sign().is_some()),
        ));
        drop(region);
    }

    // ---- L3-04: replay (same sequence re-published) → SafeStop ----
    {
        let name = format!("/kirra-l3e2e-04-{pid}");
        let region = PosixShmRegion::create(&name).expect("create region");
        let reader = PosixShmReader::open(&name).expect("governor RO mapping");
        let mut wm = AcceptedWatermark::new();
        let first_pub = spawn_guest(&name, 10.0, 7, FUTURE_DEADLINE);
        let first = decide_cycle(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
        let first_ok = matches!(first.outcome, GovernorOutcome::Actuate(_));
        // The guest re-publishes the SAME sequence (generation advances) — replay.
        let second_pub = spawn_guest(&name, 10.0, 7, FUTURE_DEADLINE);
        let second = decide_cycle(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
        let replay_refused =
            second.outcome == GovernorOutcome::SafeStop && second.view_to_sign().is_none();
        rows.push(row(
            "L3-04",
            "replay (same seq re-published): SafeStop",
            first_pub && first_ok && second_pub && replay_refused,
            format!("first_ok={first_ok} replay_refused={replay_refused}"),
        ));
        drop(region);
    }

    // ---- L3-05: zero/unwritten region → SafeStop (fail-closed cold start) ----
    {
        let name = format!("/kirra-l3e2e-05-{pid}");
        let region = PosixShmRegion::create(&name).expect("create region");
        let reader = PosixShmReader::open(&name).expect("governor RO mapping");
        let mut wm = AcceptedWatermark::new();
        let cycle = decide_cycle(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
        let safe = cycle.outcome == GovernorOutcome::SafeStop && cycle.view.is_none();
        rows.push(row(
            "L3-05",
            "unwritten region (cold start): SafeStop",
            safe,
            format!("outcome={:?}", cycle.outcome),
        ));
        drop(region);
    }

    let gate_pass = rows.iter().all(|&p| p);
    println!("\nGATE (verdict correctness): {}", if gate_pass { "PASS" } else { "FAIL" });

    // ---- Timing (INDICATIVE) — the governor path over the SHM carrier ----
    let iters: usize = std::env::var("KIRRA_LAT_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(100_000);
    let warmup = (iters / 10).max(1_000);
    let want_fifo = matches!(std::env::var("KIRRA_E2E_FIFO").as_deref(), Ok("1") | Ok("true"));
    let fifo = want_fifo && try_fifo();
    if want_fifo && !fifo {
        eprintln!("[l3e2e] WARN: SCHED_FIFO not granted (need privilege) — timing is time-shared");
    }

    let name = format!("/kirra-l3e2e-lat-{pid}");
    let region = PosixShmRegion::create(&name).expect("create region");
    let _ = spawn_guest(&name, 10.0, 1, FUTURE_DEADLINE);
    let reader = PosixShmReader::open(&name).expect("governor RO mapping");

    println!(
        "\n=== timing — INDICATIVE (sched={}, iters={iters}) — never a WCET claim ===",
        if fifo { "SCHED_FIFO" } else { "default" }
    );
    println!("{:<34} {:>9} {:>9} {:>9} {:>11}", "path", "p50_ns", "p99_ns", "p999_ns", "max_ns");
    println!("{}", "-".repeat(78));

    // (a) the seqlock read alone
    let mut s = Vec::with_capacity(iters);
    for i in 0..(iters + warmup) {
        let t0 = Instant::now();
        let snap = read_coherent_snapshot(&reader, MAX_SNAPSHOT_RETRIES).unwrap();
        let dt = t0.elapsed();
        std::hint::black_box(snap.sequence);
        if i >= warmup {
            s.push(dt);
        }
    }
    print_timing("shm read_coherent_snapshot", s);

    // (b) the full per-cycle governor step (fresh watermark each iter so the
    //     monotonic gate never rejects — we time the pipeline, not replay logic)
    let mut s = Vec::with_capacity(iters);
    for i in 0..(iters + warmup) {
        let mut wm = AcceptedWatermark::new();
        let t0 = Instant::now();
        let cycle = decide_cycle(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES);
        let dt = t0.elapsed();
        std::hint::black_box(matches!(cycle.outcome, GovernorOutcome::Actuate(_)));
        if i >= warmup {
            s.push(dt);
        }
    }
    print_timing("decide_cycle (read+val+bound)", s);

    // (c)+(d) the release-token crypto (Ed25519) — fewer iters, it is µs-scale
    let crypto_iters = (iters / 10).max(1_000);
    let view = {
        let mut wm = AcceptedWatermark::new();
        *decide_cycle(&reader, &mut wm, 0, &contract, MAX_SNAPSHOT_RETRIES)
            .view_to_sign()
            .expect("valid region → signable view")
    };
    let mut s = Vec::with_capacity(crypto_iters);
    let mut last_token = None;
    for i in 0..(crypto_iters + crypto_iters / 10) {
        let t0 = Instant::now();
        let token = issue_release_token(&view, &gov_key);
        let dt = t0.elapsed();
        last_token = Some(token);
        if i >= crypto_iters / 10 {
            s.push(dt);
        }
    }
    print_timing("issue_release_token (sign)", s);

    let token = last_token.expect("signed at least once");
    let mut s = Vec::with_capacity(crypto_iters);
    for i in 0..(crypto_iters + crypto_iters / 10) {
        let t0 = Instant::now();
        let ok = verify_release(&token, &view, &gov_pub).is_ok();
        let dt = t0.elapsed();
        std::hint::black_box(ok);
        if i >= crypto_iters / 10 {
            s.push(dt);
        }
    }
    print_timing("verify_release (actuator)", s);
    drop(region);

    println!("\n===== KIRRA-L3E2E DONE (gate={}) =====", if gate_pass { "PASS" } else { "FAIL" });
    if gate_pass {
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
