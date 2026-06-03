// crates/kirra-ros2-adapter/src/validation.rs
//
// S131 Phase 2A — slow-loop trajectory validator.
//
// Composes the three safety-critical kernel checks into a single per-
// trajectory verdict:
//   A) Containment — `validate_trajectory_containment` (SG2)
//   B) Per-pose kinematics — `validate_vehicle_command` (P0–P6) on
//      every consecutive pose pair
//   C) RSS over horizon — `longitudinal_safe_distance` /
//      `lateral_safe_distance` (SG1) per object × per pose
// The result is `TrajectoryVerdict::Accept | Clamp | MRCFallback`.
//
// First-rejection-wins: containment failure or any DenyBreach or any
// RSS violation short-circuits to MRCFallback. A Clamp from per-pose
// kinematics is recorded but does NOT short-circuit — containment +
// RSS still get a vote.

use kirra_runtime_sdk::gateway::containment::{
    self as containment, Corridor, Pose as KernelPose,
    Point as KernelPoint,
};
use kirra_runtime_sdk::gateway::kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, EnforceAction, ProposedVehicleCommand,
};
use kirra_runtime_sdk::verifier::FleetPosture;
use parko_core::rss::{
    lateral_safe_distance, longitudinal_safe_distance,
};

use crate::config::VehicleConfig;
use crate::corridor::{CorridorSource, Point};
use crate::state::{
    AcceptedTrajectory, EgoOdom, PerceivedObject, Pose, TrajectoryPoint,
    TrajectoryVerdict,
};

/// Minimum corridor confidence the slow loop accepts. Tracks the
/// `kirra_runtime_sdk::gateway::containment::Corridor::min_confidence`
/// gate; below this the kernel returns DrivableSpaceDeparture
/// regardless of geometry.
const SLOW_LOOP_MIN_CORRIDOR_CONFIDENCE: f32 = 0.5;

/// Max corridor age (ms). One planning cycle (~100 ms) + jitter.
const SLOW_LOOP_MAX_CORRIDOR_AGE_MS: u64 = 500;

/// RSS reaction time (s). Per IEEE 2846-2022 §5.1 the canonical value
/// is 0.5 s for SAE-Level-4 stacks; we use the conservative end.
const RSS_REACTION_TIME_S: f64 = 0.5;

/// Distance below which two objects are considered laterally aligned
/// (and therefore subject to RSS longitudinal evaluation). Anything
/// beyond this lateral offset is in another corridor; containment
/// covers it.
const RSS_LATERAL_ALIGNMENT_TOLERANCE_M: f64 = 4.0;

/// Compose the three slow-loop checks into one verdict. First-rejection-
/// wins on containment and RSS; Clamp is recorded but does not
/// short-circuit (containment + RSS still vote).
///
/// Returns:
///   Accept       — clean: containment + per-pose kinematics + RSS all green
///   Clamp        — per-pose requested a Clamp on ≥ 1 pose; containment + RSS green
///   MRCFallback  — containment fail / per-pose DenyBreach / RSS violation /
///                  posture = LockedOut
///
/// `latest_odom`: the most recent ego odometry snapshot, used by the
/// per-pose mapping to derive the FIRST segment's `current_steering_angle_deg`
/// from `omega · L / v_x`. `None` (no odom yet) → falls back to `0.0`
/// (the Phase 2A behaviour), which is the conservative direction (the
/// kernel's P5b steering-rate check still bounds the implied change).
///
/// `posture` (M1): selects the effective per-pose kinematics contract.
/// `Nominal` is the unchanged Phase-2A behaviour (full envelope);
/// `Degraded` swaps in the MRC-derated contract (mirror of parko-kirra's
/// posture mapping); `LockedOut` short-circuits to `MRCFallback` without
/// running geometry checks. The containment + RSS checks always run for
/// `Nominal` and `Degraded` — posture AUGMENTS, does not REPLACE, the
/// physical invariants.
///
// SAFETY: SG8 | REQ: posture-driven-profile-selection | TEST: nominal_posture_clean_trajectory_accepts,degraded_posture_caps_kinematics_to_mrc,locked_out_short_circuits_to_mrcfallback,degraded_with_corridor_breach_still_mrcs,nominal_behavior_matches_prior_default
/// (M1 closeout. Pairs with parko-kirra's `evaluate()` posture→profile
///  mapping so the AV slow-loop and the differential-drive bridge stay
///  consistent. M1b — wiring `current_posture` to the live verifier
///  stream — is tracked at the slow-loop call site in `node.rs`.)
pub fn validate_trajectory_slow(
    trajectory: &[TrajectoryPoint],
    corridor: &dyn CorridorSource,
    objects: &[PerceivedObject],
    config: &VehicleConfig,
    latest_odom: Option<&EgoOdom>,
    posture: FleetPosture,
) -> TrajectoryVerdict {
    // ----- Posture short-circuit (M1) ----------------------------------
    //
    // A LockedOut fleet must not be commanded — the safe response is to
    // refuse the trajectory and let the fast loop emit the MRC topic.
    // We do this BEFORE the geometry checks so a locked-out fleet doesn't
    // even spend CPU on the (now meaningless) acceptance question, and so
    // an integrator inspecting verdicts in a LockedOut state always sees
    // `MRCFallback` regardless of the trajectory shape.
    if posture == FleetPosture::LockedOut {
        return TrajectoryVerdict::MRCFallback;
    }

    // Reject empty / single-point trajectories outright (the per-pose
    // loop needs ≥ 2 points to compute deltas). Conservative MRC.
    if trajectory.len() < 2 {
        return TrajectoryVerdict::MRCFallback;
    }

    // ----- A) Containment (SG2) ----------------------------------------
    //
    // Materialize the kernel-side Corridor from the trait. The trait
    // returns adapter `Point`s; we need kernel `Point`s. The field
    // shapes are identical so the conversion is a 1-for-1 copy.
    let left_kernel:  Vec<KernelPoint> = corridor.left_boundary().iter()
        .map(adapter_to_kernel_point).collect();
    let right_kernel: Vec<KernelPoint> = corridor.right_boundary().iter()
        .map(adapter_to_kernel_point).collect();
    let kernel_corridor = Corridor {
        left:           &left_kernel,
        right:          &right_kernel,
        confidence:     corridor.confidence(),
        age_ms:         corridor.age_ms(),
        min_confidence: SLOW_LOOP_MIN_CORRIDOR_CONFIDENCE,
        max_age_ms:     SLOW_LOOP_MAX_CORRIDOR_AGE_MS,
    };
    let footprint = config.to_vehicle_footprint();
    let poses: Vec<KernelPose> = trajectory.iter().map(|p| adapter_to_kernel_pose(&p.pose)).collect();

    let containment_verdict = containment::validate_trajectory_containment(
        &poses, &kernel_corridor, &footprint,
    );
    if !matches!(containment_verdict, EnforceAction::Allow) {
        return TrajectoryVerdict::MRCFallback;
    }

    // ----- B) Per-pose kinematics (P0–P6) ------------------------------
    //
    // Phase 3 fix: the FIRST segment's `current_steering_angle_deg` is
    // estimated from the latest odom snapshot via the inverse bicycle
    // model — δ ≈ atan(ω · L / v_x). This is an approximation (yaw_rate
    // has latency vs. the actual rack position); a direct rack-position
    // sensor would be more accurate. Acceptable for the pilot. See the
    // commit message for the rationale.
    //
    // Subsequent segments use the prior trajectory pose's derived
    // steering as the "current" steering, so the kernel's P5b
    // steering-rate check sees the actual transition rather than always
    // measuring against 0.
    // Posture-driven kinematics contract:
    //   - Nominal  → integrator's full envelope (`to_kinematics_contract`)
    //   - Degraded → MRC-derated dynamic limits, same integrator geometry
    //                (`to_mrc_kinematics_contract`), used as the
    //                decel-trajectory bound for the Issue #70 stop-and-hold
    //                gate below.
    // LockedOut was short-circuited above; this match is exhaustive on
    // the remaining variants.
    let degraded = posture == FleetPosture::Degraded;
    let kinematics = match posture {
        FleetPosture::Nominal  => config.to_kinematics_contract(),
        FleetPosture::Degraded => config.to_mrc_kinematics_contract(),
        FleetPosture::LockedOut => unreachable!("handled by the posture short-circuit above"),
    };
    let initial_steering_deg = current_steering_deg_from_odom(latest_odom, config);
    let mut clamp_seen = false;
    let mut prev_steering_deg = initial_steering_deg;
    for i in 0..trajectory.len() - 1 {
        let cmd = pose_pair_to_command(
            &trajectory[i],
            &trajectory[i + 1],
            config,
            prev_steering_deg,
        );
        // Carry the segment's commanded steering forward so the next
        // segment's "current" steering = this segment's commanded steering.
        prev_steering_deg = cmd.steering_angle_deg;
        // Issue #70: in Degraded the trajectory must be a controlled
        // decel-to-stop — each segment non-increasing in speed and never
        // re-initiating motion from a stop. A planned re-acceleration or
        // pullover-from-stop segment → DenyBreach → MRCFallback (the
        // controlled stop). Nominal uses the full per-pose envelope.
        let verdict = if degraded {
            enforce_degraded_decel_to_stop(&cmd, &kinematics)
        } else {
            validate_vehicle_command(&cmd, &kinematics)
        };
        match verdict {
            EnforceAction::Allow => {}
            EnforceAction::ClampLinear(_) | EnforceAction::ClampSteering(_) => {
                clamp_seen = true;
            }
            EnforceAction::DenyBreach(_) => {
                return TrajectoryVerdict::MRCFallback;
            }
        }
    }

    // ----- C) RSS over horizon (SG1) -----------------------------------
    //
    // For each PerceivedObject, find the trajectory pose closest to it
    // and evaluate longitudinal + lateral RSS gaps. The lateral check
    // gates the longitudinal check: if the object is far enough off the
    // ego corridor laterally, containment handled it; longitudinal is
    // skipped to avoid spurious violations from objects in another lane.
    for obj in objects {
        for traj_point in trajectory {
            let dx = obj.pos.x_m - traj_point.pose.x_m;
            let dy = obj.pos.y_m - traj_point.pose.y_m;

            // Skip if behind ego pose (objects we've already passed).
            // Ego-frame: rotate world delta by -heading.
            let cos_h = traj_point.pose.heading_rad.cos();
            let sin_h = traj_point.pose.heading_rad.sin();
            let dx_ego =  cos_h * dx + sin_h * dy;     // longitudinal
            let dy_ego = -sin_h * dx + cos_h * dy;     // lateral

            // Behind ego — RSS does not apply (the object is no longer
            // a forward collision risk; containment + posture handle
            // rear-end concerns).
            if dx_ego <= 0.0 {
                continue;
            }
            // Lateral filter — object is in a different lane; let
            // containment cover it.
            if dy_ego.abs() > RSS_LATERAL_ALIGNMENT_TOLERANCE_M {
                continue;
            }

            // Longitudinal RSS — required forward gap.
            let lon_required = longitudinal_safe_distance(
                traj_point.velocity_mps,
                obj.velocity_mps,
                RSS_REACTION_TIME_S,
                config.max_accel_mps2,
                config.max_decel_mps2,
                config.max_decel_mps2,
            );
            if dx_ego < lon_required {
                return TrajectoryVerdict::MRCFallback;
            }

            // Lateral RSS — required side gap. Use the object's
            // lateral velocity component as the lateral-vel input.
            // (Phase 2A: assume objects' lateral velocity = 0 if
            // PerceivedObject does not carry per-axis velocity. The
            // longitudinal check is the dominant risk; lateral RSS is
            // defence in depth against an object cutting in.)
            let obj_lat_vel = obj.velocity_mps * (obj.heading_rad - traj_point.pose.heading_rad).sin();
            let ego_lat_vel = 0.0; // straight-following assumption per
                                   // §3 (the per-pose Pose.heading
                                   // captures any planned curvature).
            let lat_required = lateral_safe_distance(
                ego_lat_vel,
                obj_lat_vel,
                kinematics.max_lateral_accel_mps2,
                RSS_REACTION_TIME_S,
            );
            if dy_ego.abs() < lat_required {
                return TrajectoryVerdict::MRCFallback;
            }
        }
    }

    // ----- D) Aggregate ------------------------------------------------
    if clamp_seen {
        TrajectoryVerdict::Clamp
    } else {
        TrajectoryVerdict::Accept
    }
}

// ---------------------------------------------------------------------------
// Phase 3 — Fast-loop conformance
// ---------------------------------------------------------------------------

/// Per-cycle conformance verdict — the fast loop emits this for every
/// incoming control command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceVerdict {
    /// The command conforms to the currently-accepted trajectory.
    /// Republish on the gate's output topic.
    Accept,
    /// The command does NOT conform OR no fresh trajectory is available.
    /// Publish the MRC command instead.
    MRCFallback,
}

/// Velocity-bound tolerance for the fast-loop conformance check (m/s).
/// The kernel's per-pose check allows the planner some slack at each
/// pose; the fast loop allows the same slack vs. the nearest-pose
/// velocity so a clean trajectory + a clean conformance check don't
/// disagree on the boundary.
pub const VELOCITY_TOLERANCE_MPS: f64 = 0.5;

/// Minimal envelope over the incoming `autoware_control_msgs::Control`
/// fields. Built at the subscriber boundary (Phase 4 — when the typed
/// callback lands) so the conformance check stays ROS-free.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IncomingControl {
    pub velocity_mps:  f64,
    pub steering_rad:  f64,
    /// Message stamp in wall-clock ms. Phase 3 does not yet use this
    /// (the conformance check operates on `now_ms` directly), but it's
    /// carried for Phase 4 audit emission.
    pub stamp_ms:      u64,
}

// SAFETY: SG7 SG8 | REQ: fast-loop-trajectory-conformance | TEST: test_conforming_command_passes,test_overspeed_command_mrcs,test_stale_trajectory_mrcs,test_no_trajectory_mrcs
/// Per-cycle conformance check.
///
/// The fast loop calls this once per outgoing control command. Returns
/// `Accept` only when ALL of:
///   A. The trajectory is fresh (not stale; `is_stale(now_ms) == false`).
///   B. The current time-from-promotion falls inside the trajectory's
///      horizon — at least one pose with
///      `time_from_start_s >= now_ms - promoted_at_ms` exists.
///   C. `cmd.velocity_mps <= nearest.velocity_mps + VELOCITY_TOLERANCE_MPS`.
///   D. `cmd.steering_rad.abs() <= config.max_steering_rad`.
///
/// Anything else → `MRCFallback`. The `ego` argument is reserved for
/// Phase 4 extensions (per-axis lateral conformance + acceleration-
/// bound consistency); Phase 3 uses it only to keep the signature
/// stable across the planned extension surface.
pub fn check_command_conforms(
    cmd:        &IncomingControl,
    trajectory: &AcceptedTrajectory,
    _ego:       &EgoOdom,
    config:     &VehicleConfig,
    now_ms:     u64,
) -> ConformanceVerdict {
    // A. Staleness
    if trajectory.is_stale(now_ms) {
        return ConformanceVerdict::MRCFallback;
    }

    // B. Nearest pose by elapsed time-since-promotion. Saturate the
    // subtraction so a fast-loop call that lands BEFORE `promoted_at_ms`
    // (clock skew at promotion) treats elapsed = 0 — the first pose of
    // the trajectory.
    let elapsed_s = (now_ms.saturating_sub(trajectory.promoted_at_ms) as f64) / 1000.0;
    let nearest = trajectory.points.iter()
        .find(|p| p.time_from_start_s >= elapsed_s);
    let nearest = match nearest {
        Some(p) => p,
        // Trajectory exhausted — every pose's time_from_start_s is in
        // the past. The fast loop must MRC; the slow loop is expected
        // to have promoted a fresh trajectory by now.
        None => return ConformanceVerdict::MRCFallback,
    };

    // C. Velocity bound
    if cmd.velocity_mps > nearest.velocity_mps + VELOCITY_TOLERANCE_MPS {
        return ConformanceVerdict::MRCFallback;
    }

    // D. Steering bound
    if cmd.steering_rad.abs() > config.max_steering_rad {
        return ConformanceVerdict::MRCFallback;
    }

    ConformanceVerdict::Accept
}

// ---------------------------------------------------------------------------
// Conversions: adapter types ↔ kernel types
// ---------------------------------------------------------------------------

#[inline]
fn adapter_to_kernel_point(p: &Point) -> KernelPoint {
    KernelPoint { x_m: p.x_m, y_m: p.y_m }
}

#[inline]
fn adapter_to_kernel_pose(p: &Pose) -> KernelPose {
    KernelPose { x_m: p.x_m, y_m: p.y_m, heading_rad: p.heading_rad }
}

/// Map a consecutive pose pair to a kernel `ProposedVehicleCommand`.
/// The mapping derives:
///   - `delta_time_s`        = b.time_from_start_s - a.time_from_start_s
///   - `current_velocity_mps`= a.velocity_mps
///   - `linear_velocity_mps` = b.velocity_mps
///   - `current_steering_angle_deg` = `current_steering_deg` argument
///     (the FIRST segment passes the odom-derived estimate; subsequent
///     segments pass the prior segment's commanded steering)
///   - `steering_angle_deg`         = bicycle-model approx:
///        steering = atan2(δheading * wheelbase, velocity * δt) → degrees
/// The bicycle-model approximation matches the kernel's P6 (lateral-accel)
/// model and is the canonical pose-pair → steering-angle conversion.
/// Field names match `ProposedVehicleCommand` exactly (Step 0).
fn pose_pair_to_command(
    a: &TrajectoryPoint,
    b: &TrajectoryPoint,
    config: &VehicleConfig,
    current_steering_deg: f64,
) -> ProposedVehicleCommand {
    let delta_time_s = b.time_from_start_s - a.time_from_start_s;
    let delta_heading = b.pose.heading_rad - a.pose.heading_rad;
    // Average velocity over the segment; avoids dividing by ~0 when
    // velocity is small at one endpoint.
    let avg_velocity = 0.5 * (a.velocity_mps + b.velocity_mps);
    let denom = avg_velocity * delta_time_s;
    // Guard the bicycle-model denominator: at near-zero velocity or
    // near-zero dt the steering is undefined; report 0 (the P1
    // `delta_time_s <= 0.0` check in `validate_vehicle_command` will
    // catch genuinely-bad inputs).
    let steering_rad = if denom.abs() > 1e-6 {
        (delta_heading * config.wheelbase_m).atan2(denom)
    } else {
        0.0
    };
    ProposedVehicleCommand {
        linear_velocity_mps:        b.velocity_mps,
        current_velocity_mps:       a.velocity_mps,
        delta_time_s,
        steering_angle_deg:         steering_rad.to_degrees(),
        current_steering_angle_deg: current_steering_deg,
    }
}

/// Estimate the vehicle's current steering angle (degrees) from the
/// latest ego odometry snapshot using the inverse bicycle model
/// `δ ≈ atan(ω · L / v_x)`. At near-zero velocity the steering is
/// undetermined; we fall back to 0.0 (the P5b rate check still bounds
/// any subsequent change against this assumed neutral position).
///
/// This is the Phase 3 fix for the Phase 2A
/// `current_steering_angle_deg = 0.0` approximation. It is still an
/// approximation: `nav_msgs::Odometry` yaw_rate has latency vs. the
/// actual rack position. A direct vehicle-state steering sensor would
/// be more accurate; tracked as a Phase 4 follow-up.
fn current_steering_deg_from_odom(odom: Option<&EgoOdom>, config: &VehicleConfig) -> f64 {
    match odom {
        Some(o) if o.linear_x_mps.abs() > 0.1 => {
            (o.yaw_rate_rads * config.wheelbase_m / o.linear_x_mps)
                .atan()
                .to_degrees()
        }
        _ => 0.0,
    }
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use crate::state::Pose as AdapterPose;

    #[test]
    fn pose_pair_zero_delta_heading_produces_zero_steering() {
        let cfg = VehicleConfig::default_urban();
        let a = TrajectoryPoint {
            pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 10.0, time_from_start_s: 0.0,
        };
        let b = TrajectoryPoint {
            pose: AdapterPose { x_m: 1.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 10.0, time_from_start_s: 0.1,
        };
        let cmd = pose_pair_to_command(&a, &b, &cfg, 0.0);
        assert!((cmd.steering_angle_deg).abs() < 1e-9);
        assert_eq!(cmd.linear_velocity_mps, 10.0);
        assert_eq!(cmd.current_velocity_mps, 10.0);
        assert!((cmd.delta_time_s - 0.1).abs() < 1e-9);
    }

    #[test]
    fn pose_pair_curvature_produces_proportional_steering() {
        let cfg = VehicleConfig::default_urban();
        // 10° heading change over 0.5 s at 10 m/s → ~ atan2(0.1745*2.8,
        // 10*0.5) = atan2(0.4886, 5.0) ≈ 5.58° steering.
        let a = TrajectoryPoint {
            pose: AdapterPose { x_m: 0.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 10.0, time_from_start_s: 0.0,
        };
        let b = TrajectoryPoint {
            pose: AdapterPose { x_m: 5.0, y_m: 0.0, heading_rad: 10.0_f64.to_radians() },
            velocity_mps: 10.0, time_from_start_s: 0.5,
        };
        let cmd = pose_pair_to_command(&a, &b, &cfg, 0.0);
        assert!(cmd.steering_angle_deg > 4.0 && cmd.steering_angle_deg < 7.0,
            "expected ~5.6° steering, got {}", cmd.steering_angle_deg);
    }
}
