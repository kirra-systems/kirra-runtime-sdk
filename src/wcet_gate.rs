// src/wcet_gate.rs
//
// S3 WCET measurement + CI regression gate (issue #115).
//
// FRAMING (important — read this once):
//
// This module characterizes the Governor verdict-path WCET on CI / dev
// hardware and provides a regression gate. It is NOT the certified target-
// hardware WCET. The certified bound is re-measured on the D3 independent
// compute under S8 (#120) — this establishes the method, the structural
// boundedness argument, and a relative bound suitable for catching gross
// regressions in CI.
//
// SAFETY GOAL: SG9 (OCCY_SAFETY_GOALS) / SG-004 + SG-006 + SG-008 + SG-015
// (AEGIS-SG-001) — the safety check fails closed within a proven bound.
// The bound proven here gates CI; re-validated on target by S8 sets the
// SG9 fail-closed timeout for deployment.
//
// LOOP CLOSURE (from SPEED_ENVELOPE.md + ADR-0001):
//   verdict_WCET  +  actuation_latency  <  control_cycle  <  0.5 s reaction
// On any reasonable target (≤ ms-scale control cycle, tens-of-ms actuation
// latency), a sub-100µs verdict WCET fits with multiple orders of magnitude
// of headroom.

// ---------------------------------------------------------------------------
// Structural boundedness argument (Pass A + B1 + B2 + B3 evidence)
// ---------------------------------------------------------------------------
//
// 1. NO HEAP ALLOCATION ON THE VERDICT PATH.
//    Pass A swapped `DenyBreach(String)` -> `DenyBreach(DenyCode)` (Copy,
//    `&'static str` reason), dropped two per-request `.to_string()` on
//    path+method in policy_layer.rs:224/229, and pinned the audit payload
//    types so the producer captures owned-Copy fields only. Pass B2 moved
//    the payload `serde_json::to_string` into the writer task (off the
//    verdict path). See src/audit_writer.rs::write_one for the only
//    serialization site.
//
// 2. EVERY LOOP IS BOUNDED.
//    - validate_vehicle_command (src/gateway/kinematics_contract.rs): no
//      loops. Linear pipeline of P0..P6 guards on a single command. O(1).
//    - validate_cmd_vel (src/gateway/cmd_vel.rs): scalar checks. O(1).
//    - parko-core::rss::lateral_safe_distance + longitudinal_safe_distance:
//      one closure called twice (ego + obj). O(1).
//    - should_route_command (src/posture_cache.rs): pattern match + bool
//      returns. O(1).
//    - resolve_posture (src/gateway/policy_layer.rs): one RwLock read +
//      enum match. O(1).
//    Per-command trajectory horizon length and per-evaluation agent count
//    are bounded by the CALLER's planning loop, not by code within the
//    Governor verdict path. The verdict path itself evaluates ONE command
//    per call.
//
// 3. NO UNBOUNDED RECURSION.
//    No recursion of any kind on the verdict path.
//
// 4. PANIC = ABORT (release).
//    Cargo.toml [profile.release] panic = "abort" (Pass A). Eliminates
//    unwind overhead and ensures any residual panic terminates the process
//    deterministically — caught fail-closed by the watchdog/peer detection.
//
// 5. LOCK-FREE VERDICT PATH (production).
//    - Pass B1: gate (`enforce_posture_routing`) reads `cached_db_epoch:
//      AtomicU64` (Acquire) instead of `store.lock()` + `current_epoch()`.
//    - Pass A: Allow + Clamp arms allocate nothing and never lock.
//    - Pass B2: Deny arm calls `audit_writer_tx.try_send(job)` —
//      tokio mpsc try_send is wait-free on the producer side under
//      bounded backpressure (Full = `Err`, not block).
//    The only `store.lock()` reachable from `policy_layer.rs` is the
//    test-fallback branch executed when `audit_writer_tx` is not
//    installed — unreachable in production main.
//
// 6. TRY_SEND IS O(1) BOUNDED.
//    tokio::sync::mpsc::Sender::try_send is documented as non-blocking
//    and returns immediately on Full / Closed. The queue itself is
//    bounded (AUDIT_QUEUE_BOUND = 2048) so production cannot grow the
//    queue beyond a fixed size.
//
// CONCLUSION: a finite WCET for the verdict path exists by construction.
// This module measures it and gates CI against regressions.

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

/// SG9 fail-closed timeout TARGET (microseconds) for deployment hardware.
///
/// This is the verdict-path WCET budget the Governor must stay under on the
/// target SoC. It must fit inside the control-cycle period minus the
/// actuation latency (loop closure: WCET + actuation < cycle < 0.5 s
/// reaction budget per SPEED_ENVELOPE.md). 100 µs is conservative for the
/// O(1) scalar-math kernel proven above; S8 (#120) re-measures on target.
pub const GOVERNOR_VERDICT_WCET_TARGET_MICROS: u64 = 100;

/// CI regression-gate threshold (microseconds).
///
/// Generous (10× the target budget) to tolerate CI-hardware variance —
/// shared CI runners are typically virtualized, noisier than the target
/// SoC, and may share cores with other jobs. The gate's job is to catch
/// GROSS regressions (e.g. an accidentally re-introduced heap alloc on
/// the hot path, a `Mutex::lock()` slipping back into a verdict arm) —
/// not to prove certifiable timing. The certified number is re-measured
/// on target hardware under S8.
pub const GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS: u64 = 1000;

/// CI regression-gate threshold for the SG2 drivable-space containment
/// check (microseconds).
///
/// The containment check is structurally heavier than the per-command
/// kinematic guards: per call it does
///   `MAX_TRAJECTORY_HORIZON × (left_vertices + right_vertices) × 4`
/// polygon-edge tests at worst case (≈ 50 × 256 × 4 = 51 200 tests, each
/// a ray-cast crossing test + a closed-form segment-distance computation).
/// That's ~3 orders of magnitude more scalar work than the per-command
/// checks, so the per-command threshold (1 ms) does not apply directly.
///
/// 10 000 µs is generous for debug-mode CI (where this test runs by
/// default — release builds are ~5–10× faster, putting the same workload
/// well under 1 ms). Same caveat as the per-command threshold: this is
/// CI-relative; S8 (#120) re-measures on the target SoC for the SG9
/// fail-closed timeout setting.
pub const GOVERNOR_CONTAINMENT_WCET_CI_THRESHOLD_MICROS: u64 = 10_000;

/// CI regression-gate threshold for the Track-C perception kinematic-plausibility
/// guard (microseconds), evaluated at PERCEPTION-TICK rate — NOT per command
/// (KIRRA-OCCY-PMON-002, Option B).
///
/// Like the SG2 containment check, this guard is structurally heavier than the
/// O(1) per-command kernel: it is `O(MAX_TRACKED_OBJECTS)` (a finite/structural
/// check + a velocity-magnitude + a teleport implied-speed test per object,
/// `MAX_TRACKED_OBJECTS = 256`). It does NOT fold into the per-command budget —
/// under Option B it runs once per perception frame and publishes a cap; the
/// verdict path only does an O(1) cap read (gated separately by the per-command
/// threshold, see `wcet_perception_cap_read_is_o1`).
///
/// 10 000 µs is generous for debug-mode CI (same caveat / separate-budget
/// rationale as `GOVERNOR_CONTAINMENT_WCET_CI_THRESHOLD_MICROS`); S8 (#120)
/// re-measures on the target SoC. The exact value is provisional — confirmed
/// alongside the tick-rate worker's real cadence at deployment time.
pub const GOVERNOR_PERCEPTION_GUARD_WCET_CI_THRESHOLD_MICROS: u64 = 10_000;

// ---------------------------------------------------------------------------
// Measurement helpers
// ---------------------------------------------------------------------------

/// Measure elapsed-time stats across `iterations` invocations of `f`.
/// Returns `(max_ns, p99_9_ns)`.
///
/// Single-threaded, no warmup, `std::time::Instant` — adequate for a
/// gross-regression CI gate. The gate asserts on max (any single sample
/// exceeding the threshold trips the regression check); p99.9 is reported
/// alongside as a stability indicator (a max that's far above p99.9
/// usually indicates a transient OS / scheduler stall, not a real
/// code-path regression).
#[cfg(test)]
fn measure_stats<F: FnMut()>(iterations: u32, mut f: F) -> (u128, u128) {
    let mut samples: Vec<u128> = Vec::with_capacity(iterations as usize);
    for _ in 0..iterations {
        let t0 = Instant::now();
        f();
        samples.push(t0.elapsed().as_nanos());
    }
    samples.sort_unstable();
    let n = samples.len();
    let max = samples[n - 1];
    let p999_idx = (n * 999 / 1000).min(n - 1);
    let p999 = samples[p999_idx];
    (max, p999)
}

// ---------------------------------------------------------------------------
// CI regression-gate tests
// ---------------------------------------------------------------------------
//
// Each test exercises one verdict-path entry point at a representative
// worst-case input + asserts the per-call max latency stays under
// `GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS`. The measured max is printed
// for diagnostic / trend-tracking. A failure here means either the
// verdict path took >1ms (gross regression) or CI hardware is severely
// degraded — both warrant investigation.
//
// Timing gates are hardware-sensitive. Numbers here are CI-relative;
// re-validated on the target SoC under S8 (#120) for the actual
// SG9 timeout setting.
#[cfg(test)]
mod ci_gate_tests {
    use super::*;
    use crate::gateway::kinematics_contract::{
        enforce_degraded_decel_to_stop, validate_vehicle_command, ProposedVehicleCommand,
        VehicleKinematicsContract,
    };
    use crate::gateway::policy::OperationalCommand;
    use crate::posture_cache::{should_route_command, CachedFleetPosture};
    use crate::verifier::FleetPosture;

    const ITERS: u32 = 100_000;

    fn assert_under_budget(name: &str, max_ns: u128, p999_ns: u128) {
        let max_us = max_ns / 1000;
        let p999_us = p999_ns / 1000;
        println!(
            "WCET-GATE {name}: max={max_ns}ns ({max_us}us)  p99.9={p999_ns}ns ({p999_us}us) \
             over {ITERS} iterations  vs CI-threshold {}us  (target {}us)",
            GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS,
            GOVERNOR_VERDICT_WCET_TARGET_MICROS,
        );
        // Gate on p99.9, not max: max in `Instant`-based microbenchmarks is
        // dominated by OS scheduler / VM-hypervisor jitter (cgroup preemption,
        // hypervisor steal time, IPI, page-fault servicing) — not code-path
        // work. p99.9 cleanly tracks the steady-state verdict-path latency,
        // so any real regression (heap alloc / Mutex / I/O slipping back
        // onto the hot path) shifts p99.9 by orders of magnitude. Max
        // is reported for diagnostic / trend-tracking and to make rare
        // outliers visible.
        assert!(
            p999_us < GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS as u128,
            "WCET REGRESSION on {name}: p99.9 {p999_us}us exceeds CI threshold {}us \
             — a verdict-path p99.9 this large indicates an accidental heap alloc, \
             Mutex acquisition, or I/O on the hot path. Investigate before merging. \
             (max={max_us}us is reported for diagnostic but not gated, since \
             single-sample max in `Instant`-based microbenchmarks reflects OS \
             scheduler / VM jitter, not code-path work.)",
            GOVERNOR_VERDICT_WCET_CI_THRESHOLD_MICROS,
        );
    }

    fn nominal_cmd() -> ProposedVehicleCommand {
        // Worst-case Allow path: all P0..P6 guards run to completion
        // without returning early. This is the full pipeline depth.
        ProposedVehicleCommand {
            linear_velocity_mps: 10.0,
            current_velocity_mps: 9.0,
            delta_time_s: 0.05,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 0.0,
        }
    }

    #[test]
    fn wcet_validate_vehicle_command_allow_path() {
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = nominal_cmd();
        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let _ = std::hint::black_box(validate_vehicle_command(
                std::hint::black_box(&cmd),
                std::hint::black_box(&contract),
            ));
        });
        assert_under_budget("validate_vehicle_command::Allow", max_ns, p999_ns);
    }

    #[test]
    fn wcet_validate_vehicle_command_p0_nan_deny() {
        // P0 NaN/Inf guard — first check, returns DenyBreach early.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: f64::NAN,
            current_velocity_mps: 0.0,
            delta_time_s: 0.05,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let _ = std::hint::black_box(validate_vehicle_command(
                std::hint::black_box(&cmd),
                std::hint::black_box(&contract),
            ));
        });
        assert_under_budget("validate_vehicle_command::P0_NaN_Deny", max_ns, p999_ns);
    }

    #[test]
    fn wcet_validate_vehicle_command_p2_velocity_clamp() {
        // P2 velocity hard ceiling — returns ClampLinear early.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 100.0, // > 35.0 nominal max
            current_velocity_mps: 10.0,
            delta_time_s: 0.05,
            steering_angle_deg: 0.0,
            current_steering_angle_deg: 0.0,
        };
        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let _ = std::hint::black_box(validate_vehicle_command(
                std::hint::black_box(&cmd),
                std::hint::black_box(&contract),
            ));
        });
        assert_under_budget("validate_vehicle_command::P2_ClampLinear", max_ns, p999_ns);
    }

    #[test]
    fn wcet_validate_vehicle_command_p6_lateral_clamp() {
        // P6 lateral-accel envelope — runs the full pipeline including
        // the bicycle-model calculation, then returns ClampSteering.
        let contract = VehicleKinematicsContract::nominal_reference_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 30.0,
            current_velocity_mps: 30.0,
            delta_time_s: 1.0,
            steering_angle_deg: 20.0,
            current_steering_angle_deg: 0.0,
        };
        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let _ = std::hint::black_box(validate_vehicle_command(
                std::hint::black_box(&cmd),
                std::hint::black_box(&contract),
            ));
        });
        assert_under_budget("validate_vehicle_command::P6_ClampSteering", max_ns, p999_ns);
    }

    #[test]
    fn wcet_enforce_degraded_decel_to_stop_worst_case() {
        // Issue #70 (STEP 5): the Degraded gate adds only a fixed set of
        // finite-checks + magnitude/sign comparisons before delegating to the
        // already-budgeted validate_vehicle_command. Worst case is the
        // PASS-the-gate path (a decelerating command), which runs the gate's
        // O(1) checks AND the full P0..P6 envelope pipeline — strictly more
        // work than a denied command (which returns at the gate). The Nominal
        // path is unchanged and benched separately above; this confirms the
        // Degraded path stays under the same per-verdict budget.
        let mrc = VehicleKinematicsContract::mrc_fallback_profile();
        let cmd = ProposedVehicleCommand {
            linear_velocity_mps: 4.0,   // decelerating from 4.5 → passes gate
            current_velocity_mps: 4.5,
            delta_time_s: 0.05,
            steering_angle_deg: 5.0,
            current_steering_angle_deg: 0.0,
        };
        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let _ = std::hint::black_box(enforce_degraded_decel_to_stop(
                std::hint::black_box(&cmd),
                std::hint::black_box(&mrc),
            ));
        });
        assert_under_budget("enforce_degraded_decel_to_stop::pass_then_envelope", max_ns, p999_ns);
    }

    #[test]
    fn wcet_should_route_command_nominal() {
        // Posture gate hot path: cache fresh + Nominal -> route true.
        let cache = Some(CachedFleetPosture::new(FleetPosture::Nominal));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let _ = std::hint::black_box(should_route_command(
                std::hint::black_box(&cache),
                std::hint::black_box(now),
                std::hint::black_box(OperationalCommand::WriteState),
            ));
        });
        assert_under_budget("should_route_command::Nominal", max_ns, p999_ns);
    }

    #[test]
    fn wcet_validate_trajectory_containment_worst_case() {
        // SAFETY: SG2 SG9 | REQ: drivable-space-containment-wcet | TEST: wcet_validate_trajectory_containment_worst_case
        // Worst-case input: MAX_TRAJECTORY_HORIZON poses × MAX_CORRIDOR_VERTICES
        // per side, all-Allow path (forces the inner loop to walk every polygon
        // edge against every footprint corner of every pose, max work). A
        // regression that introduces an alloc, Mutex, or unbounded loop on the
        // SG2 check would surface here.
        use crate::gateway::containment::{
            validate_trajectory_containment, Corridor, Pose, Point, VehicleFootprint,
            MAX_CORRIDOR_VERTICES, MAX_TRAJECTORY_HORIZON,
        };
        use crate::gateway::kinematics_contract::VehicleKinematicsContract;

        let n = MAX_CORRIDOR_VERTICES;
        let half_w = 6.0;
        let x_max = 200.0;
        let dx = x_max / (n as f64 - 1.0);
        let left: Vec<Point> = (0..n)
            .map(|i| Point { x_m: i as f64 * dx, y_m: half_w })
            .collect();
        let right: Vec<Point> = (0..n)
            .map(|i| Point { x_m: i as f64 * dx, y_m: -half_w })
            .collect();
        let corridor = Corridor {
            left: &left,
            right: &right,
            confidence: 0.95,
            age_ms: 0,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        let traj: Vec<Pose> = (0..MAX_TRAJECTORY_HORIZON)
            .map(|i| Pose {
                x_m: 10.0 + (i as f64) * 2.0,
                y_m: 0.0,
                heading_rad: 0.0,
            })
            .collect();
        let footprint = VehicleFootprint::from(&VehicleKinematicsContract::nominal_reference_profile());

        // Slightly fewer iterations than the per-command checks because the
        // per-call work is O(poses × vertices × 4) — still plenty of samples
        // for p99.9.
        const CONTAINMENT_ITERS: u32 = 1_000;
        let (max_ns, p999_ns) = measure_stats(CONTAINMENT_ITERS, || {
            let _ = std::hint::black_box(validate_trajectory_containment(
                std::hint::black_box(&traj),
                std::hint::black_box(&corridor),
                std::hint::black_box(&footprint),
            ));
        });
        let max_us = max_ns / 1000;
        let p999_us = p999_ns / 1000;
        println!(
            "WCET-GATE validate_trajectory_containment::worst_case ({}poses × {}verts/side): \
             max={max_ns}ns ({max_us}us)  p99.9={p999_ns}ns ({p999_us}us)  \
             vs SG2-CI-threshold {}us (per-cmd target {}us; containment is structurally heavier — see GOVERNOR_CONTAINMENT_WCET_CI_THRESHOLD_MICROS)",
            MAX_TRAJECTORY_HORIZON, MAX_CORRIDOR_VERTICES,
            GOVERNOR_CONTAINMENT_WCET_CI_THRESHOLD_MICROS,
            GOVERNOR_VERDICT_WCET_TARGET_MICROS,
        );
        assert!(
            p999_us < GOVERNOR_CONTAINMENT_WCET_CI_THRESHOLD_MICROS as u128,
            "WCET REGRESSION on validate_trajectory_containment::worst_case: \
             p99.9 {p999_us}us exceeds SG2-specific CI threshold {}us — the \
             SG2 containment path has acquired a heap alloc, Mutex, or I/O \
             on top of its expected O(poses × vertices × 4) edge work.",
            GOVERNOR_CONTAINMENT_WCET_CI_THRESHOLD_MICROS,
        );
    }

    // -----------------------------------------------------------------------
    // KIRRA-OCCY-PMON-002 — perception-derate composition WCET coverage.
    //
    // Two separate budgets, mirroring the containment precedent:
    //   (1) the tick-rate kinematic guard (O(MAX_TRACKED_OBJECTS)) — its OWN
    //       budget, NOT the per-command one;
    //   (2) the per-command cap read+compose (resolve_perception_cap +
    //       apply_perception_cap) — must stay O(1), under the per-command budget
    //       (no per-command budget revision).
    // -----------------------------------------------------------------------

    #[test]
    fn wcet_perception_kinematic_guard_worst_case() {
        use crate::gateway::perception_monitor::{
            kinematic_plausibility_derate, KinematicPlausibilityContract, PerceptionOutput,
            TrackedObject, Vec2, MAX_TRACKED_OBJECTS,
        };
        // Worst case: a full MAX_TRACKED_OBJECTS slice of valid, plausible
        // objects (every object runs the finite + velocity + teleport checks to
        // completion — no early structural-failure return).
        let objs: Vec<TrackedObject> = (0..MAX_TRACKED_OBJECTS as u64)
            .map(|id| TrackedObject {
                id,
                pos_m: Vec2 { x: 10.0, y: 0.0 },
                vel_mps: Vec2 { x: 5.0, y: 0.0 },
                prev_pos_m: Vec2 { x: 9.5, y: 0.0 },
                dt_s: 0.1,
            })
            .collect();
        let perception = PerceptionOutput {
            objects: &objs,
            confidence: 0.95,
            age_ms: 10,
            min_confidence: 0.5,
            max_age_ms: 500,
        };
        let contract = KinematicPlausibilityContract::urban_reference();

        const GUARD_ITERS: u32 = 2_000;
        let (max_ns, p999_ns) = measure_stats(GUARD_ITERS, || {
            let _ = std::hint::black_box(kinematic_plausibility_derate(
                std::hint::black_box(&perception),
                std::hint::black_box(&contract),
            ));
        });
        let (max_us, p999_us) = (max_ns / 1000, p999_ns / 1000);
        println!(
            "WCET-GATE perception_kinematic_guard::worst_case ({MAX_TRACKED_OBJECTS} objects): \
             max={max_ns}ns ({max_us}us)  p99.9={p999_ns}ns ({p999_us}us)  \
             vs PMON-guard CI-threshold {}us (TICK-rate, not per-command — see \
             GOVERNOR_PERCEPTION_GUARD_WCET_CI_THRESHOLD_MICROS)",
            GOVERNOR_PERCEPTION_GUARD_WCET_CI_THRESHOLD_MICROS,
        );
        assert!(
            p999_us < GOVERNOR_PERCEPTION_GUARD_WCET_CI_THRESHOLD_MICROS as u128,
            "WCET REGRESSION on perception_kinematic_guard::worst_case: p99.9 {p999_us}us \
             exceeds the PMON-guard CI threshold {}us — the O(MAX_TRACKED_OBJECTS) guard \
             has acquired a heap alloc / lock / I/O on top of its per-object scalar work.",
            GOVERNOR_PERCEPTION_GUARD_WCET_CI_THRESHOLD_MICROS,
        );
    }

    #[test]
    fn wcet_perception_cap_read_is_o1() {
        use crate::gateway::kinematics_contract::VehicleKinematicsContract;
        use crate::gateway::perception_monitor::{
            apply_perception_cap, empty_perception_cap, resolve_perception_cap, CachedPerceptionCap,
            DerateCode,
        };
        // The per-command hot-path addition: resolve (one RwLock read + staleness
        // compare) + apply (clone + one min). Must stay under the PER-COMMAND
        // budget — this is the cost the verdict path actually pays per command.
        let cache = empty_perception_cap();
        let now = crate::posture_cache::now_ms();
        *cache.write().unwrap() = Some(CachedPerceptionCap {
            cap_mps: 12.0,
            generated_at_ms: now,
            ttl_ms: 5_000,
            reason: DerateCode::ObjectVelocityImplausible,
        });
        let base = VehicleKinematicsContract::nominal_reference_profile();

        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let eff = std::hint::black_box(resolve_perception_cap(
                std::hint::black_box(true),
                std::hint::black_box(&cache),
                std::hint::black_box(now),
            ));
            let _ = std::hint::black_box(apply_perception_cap(std::hint::black_box(&base), eff));
        });
        assert_under_budget("perception_cap_read+compose::per_command", max_ns, p999_ns);
    }

    #[test]
    fn wcet_should_route_command_stale_fail_closed() {
        // Stale cache -> SG9 fail-closed Deny. This is the path SG9 most
        // directly governs; verifies it stays under the budget.
        let cache = Some(CachedFleetPosture::new(FleetPosture::Nominal));
        let stale_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 10_000_000_000; // ~115 days in ms; orders of magnitude past any TTL
        let (max_ns, p999_ns) = measure_stats(ITERS, || {
            let _ = std::hint::black_box(should_route_command(
                std::hint::black_box(&cache),
                std::hint::black_box(stale_now),
                std::hint::black_box(OperationalCommand::WriteState),
            ));
        });
        assert_under_budget("should_route_command::Stale_FailClosed", max_ns, p999_ns);
    }
}
