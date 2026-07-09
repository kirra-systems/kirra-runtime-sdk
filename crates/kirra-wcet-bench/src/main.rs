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
//! - `KIRRA_WCET_ENV`    — `host` (default) | `ci-runner` (alias `ci`) | `other`.
//! - `KIRRA_WCET_ITERS`  — measured iterations per stage (default 100_000).
//! - `KIRRA_WCET_WARMUP` — warm-up iterations per stage, not recorded (default 10_000).
//!
//! Scope: only the governor verdict stage is host-buildable. The perception /
//! parko / actuator stages are `ros2`/target-gated and measured by the harness
//! wiring follow-up; they are listed here as NOT-MEASURED rather than faked.

use std::hint::black_box;

use kirra_core::kinematics_contract::{
    validate_vehicle_command, EnforceAction, ProposedVehicleCommand, VehicleKinematicsContract,
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

/// Full-depth **Allow** command under the NOMINAL profile (max_accel 2.5,
/// max_steering_rate 45 °/s, max_speed 35): accel `(10.1-10.0)/0.05 = 2.0 ≤ 2.5`,
/// steering rate `(1.5-1.0)/0.05 = 10 ≤ 45`, speed `10.1 ≤ 35`, lateral accel ≪
/// 3.5 — so NO guard clamps and every P0..P6 check runs to completion. That
/// full-pipeline Allow is the worst-case (longest) verdict path; an early clamp
/// or deny returns sooner and would UNDER-report WCET. `assert_allow` verifies
/// this at runtime so a contract change can't silently turn it into an
/// early-return measurement.
fn nominal_allow_cmd() -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: 10.1,
        current_velocity_mps: 10.0,
        delta_time_s: 0.05,
        steering_angle_deg: 1.5,
        current_steering_angle_deg: 1.0,
    }
}

/// Full-depth **Allow** command under the MRC profile (max_accel 1.0,
/// max_steering_rate 20 °/s, max_speed 5): accel `(3.04-3.0)/0.05 = 0.8 ≤ 1.0`,
/// rate `(0.7-0.5)/0.05 = 4 ≤ 20`, speed `3.04 ≤ 5` — Allow, full pipeline. The
/// previous bench reused the nominal 10 m/s command here, which exceeds the MRC
/// 5 m/s ceiling and returned an immediate P2 clamp (not the full MRC pipeline).
fn mrc_allow_cmd() -> ProposedVehicleCommand {
    ProposedVehicleCommand {
        linear_velocity_mps: 3.04,
        current_velocity_mps: 3.0,
        delta_time_s: 0.05,
        steering_angle_deg: 0.7,
        current_steering_angle_deg: 0.5,
    }
}

/// Verify a command actually reaches [`EnforceAction::Allow`] under `contract`
/// before it is used to characterize the worst-case Allow path. Fail loudly
/// (exit 2) rather than silently measure a shorter early-return path.
fn assert_allow(label: &str, cmd: &ProposedVehicleCommand, contract: &VehicleKinematicsContract) {
    let verdict = validate_vehicle_command(cmd, contract);
    if verdict != EnforceAction::Allow {
        eprintln!(
            "[kirra-wcet-bench] FATAL: the '{label}' command does not reach EnforceAction::Allow \
             (got {verdict:?}); the bench would measure an early-return path, not the worst-case \
             full pipeline. Adjust the command shape to the current contract limits."
        );
        std::process::exit(2);
    }
}

/// Measure `f` over `warmup` (discarded) + `iters` (recorded) iterations into a
/// fresh channel, returning the snapshot PLUS the raw per-iteration samples in
/// microseconds (EP-21: the EVT/pWCET tail fit needs the raw sample set, not the
/// bucketed histogram). Each sample includes the TWO per-iteration monotonic
/// clock reads (`now_nanos` at the start and again inside `elapsed_nanos_since`)
/// — conservatively counted as observer overhead (methodology §2: include rather
/// than subtract).
fn measure(
    clock: &StdMonotonicClock,
    warmup: u32,
    iters: u32,
    mut f: impl FnMut(),
) -> (kirra_timing::ChannelStats, Vec<f64>) {
    for _ in 0..warmup {
        f();
    }
    let mut ch: kirra_timing::WcetChannel<BUCKETS> =
        kirra_timing::WcetChannel::new(BUCKET_WIDTH_NS);
    let mut samples_us = Vec::with_capacity(iters as usize);
    for _ in 0..iters {
        let t0 = clock.now_nanos();
        f();
        let elapsed_ns = clock.elapsed_nanos_since(t0);
        ch.record_nanos(elapsed_ns);
        samples_us.push(elapsed_ns as f64 / 1_000.0);
    }
    (ch.snapshot(), samples_us)
}

/// EP-21 — the pWCET (EVT/MBPTA) tail analysis over one stage's raw samples.
/// Prints the estimate + the representativity diagnostics to stderr and returns
/// a machine CSV line (`pwcet,...`). ALWAYS labeled INDICATIVE: the EVT fit
/// characterizes the host sample's tail; it cannot upgrade the evidence class
/// (`WCET_MEASUREMENT_METHODOLOGY.md` §4a). A refused fit (too few exceedances,
/// degenerate tail, …) is reported as not-estimable rather than faked.
fn pwcet_line(stage_name: &str, samples_us: &[f64]) -> String {
    match kirra_timing::evt::estimate_pwcet(samples_us, PWCET_THRESHOLD_QUANTILE, PWCET_TARGET_PROB)
    {
        Ok(est) => {
            eprintln!(
                "[kirra-wcet-bench] pWCET[{stage_name}] (INDICATIVE, host tail-fit): \
                 {:.3} µs @ p={:.0e}/exec  (GPD ξ={:.3} σ={:.3}, u={:.3} µs, {} exceedances of {}) \
                 diagnostics: lag1_autocorr={:.3} (≈0 ⇒ i.i.d. plausible), \
                 stationarity_ratio={:.3} (≈1 ⇒ stable mean)",
                est.pwcet,
                est.target_prob,
                est.gpd.xi,
                est.gpd.sigma,
                est.threshold,
                est.n_exceed,
                est.n_total,
                est.lag1_autocorr,
                est.stationarity_ratio,
            );
            format!(
                "pwcet,{stage_name},{:.6},{:.0e},{:.6},{:.6},{},{},{:.6},{:.6},INDICATIVE\n",
                est.pwcet,
                est.target_prob,
                est.gpd.xi,
                est.gpd.sigma,
                est.n_exceed,
                est.n_total,
                est.lag1_autocorr,
                est.stationarity_ratio,
            )
        }
        Err(e) => {
            eprintln!(
                "[kirra-wcet-bench] pWCET[{stage_name}]: not estimable ({e}) — reported honestly, \
                 never fabricated"
            );
            // Sanitize the error into a single comma-free field so the row stays
            // machine-parseable CSV regardless of the message (the full text is
            // on stderr above).
            let reason: String = format!("{e}")
                .chars()
                .map(|c| match c {
                    ',' | '\n' | '\r' | '"' => ';',
                    c => c,
                })
                .collect();
            format!("pwcet,{stage_name},NOT-ESTIMABLE,{reason},,,,,,INDICATIVE\n")
        }
    }
}

/// POT threshold quantile for the pWCET fit (the top 5 % of samples form the tail).
const PWCET_THRESHOLD_QUANTILE: f64 = 0.95;
/// Target per-execution exceedance probability for the reported pWCET.
const PWCET_TARGET_PROB: f64 = 1e-5;

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
    // (full-pipeline) Allow path under the Nominal profile and under the MRC
    // profile. Each command is verified to actually reach Allow before measuring.
    let nominal_contract = VehicleKinematicsContract::nominal_reference_profile();
    let mrc_contract = VehicleKinematicsContract::mrc_fallback_profile();
    let nominal_command = nominal_allow_cmd();
    let mrc_command = mrc_allow_cmd();
    assert_allow(
        "governor_exec (nominal)",
        &nominal_command,
        &nominal_contract,
    );
    assert_allow("governor_exec_mrc", &mrc_command, &mrc_contract);

    let (governor, governor_samples) = measure(&clock, warmup, iters, || {
        let _ = black_box(validate_vehicle_command(
            black_box(&nominal_command),
            black_box(&nominal_contract),
        ));
    });
    let (governor_mrc, governor_mrc_samples) = measure(&clock, warmup, iters, || {
        let _ = black_box(validate_vehicle_command(
            black_box(&mrc_command),
            black_box(&mrc_contract),
        ));
    });

    let stages = [
        StageReport::new(stage::GOVERNOR_EXEC, governor),
        StageReport::new(stage::GOVERNOR_EXEC_MRC, governor_mrc),
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
    // EP-21 — pWCET tail analysis (EVT/MBPTA) per measured stage: the estimate +
    // representativity diagnostics on stderr, machine rows appended to the CSV.
    // CSV row: pwcet,<stage>,<pwcet_us>,<p>,<xi>,<sigma>,<n_exceed>,<n_total>,
    //          <lag1_autocorr>,<stationarity_ratio>,INDICATIVE
    let pwcet_gov = pwcet_line(stage::GOVERNOR_EXEC, &governor_samples);
    let pwcet_mrc = pwcet_line(stage::GOVERNOR_EXEC_MRC, &governor_mrc_samples);

    let mut csv = String::new();
    report
        .write_csv(&mut csv)
        .expect("writing to a String cannot fail");
    csv.push_str(&pwcet_gov);
    csv.push_str(&pwcet_mrc);
    print!("{csv}");
}
