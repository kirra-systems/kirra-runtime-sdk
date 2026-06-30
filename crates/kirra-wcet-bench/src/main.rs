//! Host-indicative WCET report generator (#274 / L1).
//!
//! Drives the host-buildable **governor verdict path**
//! (`kirra_core::kinematics_contract::validate_vehicle_command`) through the
//! [`kirra_timing`] primitives and prints a deterministic, environment-tagged
//! report: the human table + INDICATIVE banner to **stderr**, the machine CSV to
//! **stdout** (so `kirra-wcet-bench > report.csv` captures the artifact cleanly).
//!
//! ## Indicative by construction
//!
//! This binary uses [`StdMonotonicClock`] — an `Instant`-backed *host* clock — so
//! it can NEVER produce certified WCET. The environment is clamped to
//! `host` / `ci-runner` / `other`; a request for `qnx-target-fifo` is refused and
//! downgraded with a note. Certified WCET comes only from the QNX target under
//! FIFO scheduling via `tools/qnx-rtm-harness/wcet_measure.cpp` (the
//! `WCET_MEASUREMENT_METHODOLOGY.md` host-indicative rule).
//!
//! ## Configuration (env vars)
//!
//! - `KIRRA_WCET_ENV`    — `host` (default) | `ci-runner` | `other`.
//! - `KIRRA_WCET_ITERS`  — measured iterations per stage (default 100_000).
//! - `KIRRA_WCET_WARMUP` — warm-up iterations per stage, not recorded (default 10_000).
//!
//! Scope: only the governor verdict stage is host-buildable. The perception /
//! parko / actuator stages are `ros2`/target-gated and measured by the harness
//! wiring follow-up; they are listed here as NOT-MEASURED rather than faked.

use std::hint::black_box;

use kirra_core::kinematics_contract::{
    validate_vehicle_command, ProposedVehicleCommand, VehicleKinematicsContract,
};
use kirra_timing::{stage, MeasurementEnv, MonotonicClock, Report, StageReport, StdMonotonicClock};

/// Histogram resolution for the governor verdict path: 4096 buckets × 10 ns =
/// 0..40.96 µs, well above the sub-µs verdict and the ~10× CI threshold.
const BUCKETS: usize = 4096;
const BUCKET_WIDTH_NS: u64 = 10;

fn parse_env(raw: Option<String>) -> (MeasurementEnv, Option<&'static str>) {
    match raw.as_deref() {
        None | Some("host") | Some("") => (MeasurementEnv::Host, None),
        Some("ci-runner") | Some("ci") => (MeasurementEnv::CiRunner, None),
        Some("other") => (MeasurementEnv::Other, None),
        // Refuse to mint a certified tag from a host (std-clock) run.
        Some(other) => (
            MeasurementEnv::Host,
            Some(if other == "qnx-target-fifo" {
                "KIRRA_WCET_ENV=qnx-target-fifo refused: this host bench uses a std clock and \
                 cannot produce certified WCET. Use tools/qnx-rtm-harness on the QNX target. \
                 Tagging this run as 'host' (indicative)."
            } else {
                "unrecognized KIRRA_WCET_ENV; defaulting to 'host' (indicative)."
            }),
        ),
    }
}

fn parse_count(name: &str, default: u32) -> u32 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// The full-depth Allow command: every P0..P6 guard runs to completion (the
/// worst-case verdict path), matching the `wcet_gate.rs` `nominal_cmd` shape.
fn nominal_cmd() -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: 10.0,
        current_velocity_mps: 9.0,
        delta_time_s: 0.05,
        steering_angle_deg: 5.0,
        current_steering_angle_deg: 0.0,
    }
}

/// Measure `f` over `warmup` (discarded) + `iters` (recorded) iterations into a
/// fresh channel, returning the snapshot. The per-iteration clock read is
/// included in each sample — conservatively counted as observer overhead
/// (methodology §2: include rather than subtract).
fn measure(
    clock: &StdMonotonicClock,
    warmup: u32,
    iters: u32,
    mut f: impl FnMut(),
) -> kirra_timing::ChannelStats {
    for _ in 0..warmup {
        f();
    }
    let mut ch: kirra_timing::WcetChannel<BUCKETS> = kirra_timing::WcetChannel::new(BUCKET_WIDTH_NS);
    for _ in 0..iters {
        let t0 = clock.now_nanos();
        f();
        ch.record_nanos(clock.elapsed_nanos_since(t0));
    }
    ch.snapshot()
}

fn main() {
    let (env, note) = parse_env(std::env::var("KIRRA_WCET_ENV").ok());
    if let Some(msg) = note {
        eprintln!("[kirra-wcet-bench] {msg}");
    }
    let iters = parse_count("KIRRA_WCET_ITERS", 100_000);
    let warmup = parse_count("KIRRA_WCET_WARMUP", 10_000);
    eprintln!(
        "[kirra-wcet-bench] env={} iters={iters} warmup={warmup} (host-indicative)",
        env.label()
    );

    let clock = StdMonotonicClock::new();

    // Stage: governor verdict — the host-buildable SG9 safety check, worst-case
    // Allow path (all P0..P6 guards), plus the MRC-contract variant.
    let nominal_contract = VehicleKinematicsContract::nominal_reference_profile();
    let mrc_contract = VehicleKinematicsContract::mrc_fallback_profile();
    let cmd = nominal_cmd();

    let governor = measure(&clock, warmup, iters, || {
        let _ = black_box(validate_vehicle_command(black_box(&cmd), black_box(&nominal_contract)));
    });
    let governor_mrc = measure(&clock, warmup, iters, || {
        let _ = black_box(validate_vehicle_command(black_box(&cmd), black_box(&mrc_contract)));
    });

    let stages = [
        StageReport::new(stage::GOVERNOR_EXEC, governor),
        StageReport::new("governor_exec_mrc", governor_mrc),
    ];
    let report = Report::new(env, "host-default", &stages);

    // Human table + banner → stderr (diagnostic); CSV → stdout (the artifact).
    eprintln!("{report}");
    eprintln!(
        "[kirra-wcet-bench] NOT MEASURED on host (ros2/target-gated, see harness follow-up): {}, {}, {}",
        stage::PERCEPTION_INPUT,
        stage::PARKO_EVAL,
        stage::ACTUATOR_PUBLISH,
    );
    let mut csv = String::new();
    report.write_csv(&mut csv).expect("writing to a String cannot fail");
    print!("{csv}");
}
