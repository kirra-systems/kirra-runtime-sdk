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

use kirra_core::containment::{
    self as containment, Corridor, Pose as KernelPose,
    Point as KernelPoint,
};
use kirra_core::kinematics_contract::{
    enforce_degraded_decel_to_stop, validate_vehicle_command, EnforceAction, ProposedVehicleCommand,
};
use kirra_core::frame_integrity::FrameTrust;
use kirra_core::FleetPosture;
use parko_core::rss::{
    lateral_safe_distance, longitudinal_safe_distance, opposite_direction_safe_distance,
    RSS_LONGITUDINAL_CONFLICT_M, RSS_LONGITUDINAL_OVERLAP_M,
};

use crate::config::VehicleConfig;
use crate::corridor::{CorridorSource, Point};
use crate::state::{
    AcceptedTrajectory, EgoOdom, PerceivedObject, Pose, TrajectoryPoint,
    TrajectoryVerdict,
};

/// Minimum corridor confidence the slow loop accepts. Tracks the
/// `kirra_core::containment::Corridor::min_confidence`
/// gate; below this the kernel returns DrivableSpaceDeparture
/// regardless of geometry.
const SLOW_LOOP_MIN_CORRIDOR_CONFIDENCE: f32 = 0.5;

/// Max corridor age (ms). One planning cycle (~100 ms) + jitter.
const SLOW_LOOP_MAX_CORRIDOR_AGE_MS: u64 = 500;

/// RSS reaction time (s). Per IEEE 2846-2022 §5.1 the canonical value
/// is 0.5 s for SAE-Level-4 stacks; we use the conservative end.
const RSS_REACTION_TIME_S: f64 = 0.5;

/// Speed slack (m/s) on the RSS-Rule-4 assured-clear-distance bound, to avoid
/// rejecting a trajectory for float noise / a sub-decimetre overshoot of the cap.
const OCCLUSION_SPEED_TOL_MPS: f64 = 0.1;

// The RSS lateral-alignment band (distance below which two objects are laterally
// aligned and so subject to RSS longitudinal evaluation; beyond it, containment covers
// it) is now a PER-CLASS field on `VehicleConfig`
// (`rss_lateral_alignment_tolerance_m`), not a global constant — a robotaxi uses a
// lane-width-scale 4.0 m, a small robot a much tighter band. The robotaxi value lives in
// `config::DEFAULT_RSS_LATERAL_ALIGNMENT_TOLERANCE_M` (see docs/CONTRACT_PROFILES.md).

/// Object lateral-velocity magnitude (m/s) above which a same-lane object is treated as
/// **cutting in** (a genuine side-collision risk) rather than a straight-running lead or a
/// member of a stationary queue. The lateral (side) RSS check is the CONJUNCTION partner of
/// the longitudinal check: a side collision needs the pair ABREAST (longitudinally unsafe) OR
/// closing laterally. Below this threshold a *longitudinally-safe* object triggers no lateral
/// MRC — admitting a safe same-lane stop (a stopped queue / a stopped lead the ego halts
/// behind) that the reaction-time swerve term in `lateral_safe_distance` otherwise rejected
/// (`COMPETITIVE_PLANNER_ANALYSIS §4`'s over-rejection) — while any real lateral closing is
/// still caught. Small, so only genuine lateral stillness is admitted; fail-closed elsewhere.
const RSS_LATERAL_MOTION_EPS_MPS: f64 = 0.1;

/// One predicted future position of an object at a time, in the world frame. A
/// sequence of these forms one predicted **mode** (hypothesis); the inter-sample
/// motion supplies the predicted velocity (no separate speed field to keep stale).
#[derive(Debug, Clone, Copy)]
pub struct PredictedSample {
    pub pos: Point,
    pub time_from_start_s: f64,
}

/// One predicted **mode** for an object — e.g. lane-follow, a cut-in, or a turn. An
/// object may carry several modes; the predictive RSS pass requires the ego to be
/// safe against **every** mode (worst-case), so a single dangerous hypothesis is
/// enough to refuse. Modes are perception/prediction-supplied (`None` → the pass is a
/// no-op and the snapshot RSS is the sole bound, the back-compat behaviour).
pub struct PredictedMode<'a> {
    pub object_id: u64,
    pub samples: &'a [PredictedSample],
}

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
    // Back-compat delegate: no perception-derate cap (the M1 behaviour) and no
    // occlusion/visibility bound. The ROS2 slow loop calls
    // `validate_trajectory_slow_capped` with the resolved Track-C cap
    // (KIRRA-OCCY-PMON-003 slice-1).
    validate_trajectory_slow_capped(
        trajectory, corridor, objects, config, latest_odom, posture, None, None, None,
        // Convenience/doer-side wrapper: assert AOU-LOCALIZATION-001 (Trusted →
        // primary 0.40 m containment margin). The production slow loop passes a
        // resolved FrameTrust; see `validate_trajectory_slow_capped`.
        FrameTrust::Trusted,
    )
}

/// As [`validate_trajectory_slow`], plus the Track-C perception-derate cap
/// (KIRRA-OCCY-PMON-003 D3a). `effective_perception_cap` is the value resolved
/// by [`resolve_perception_cap`] at the call site (the adapter slow loop):
/// `None` when the monitor is disabled/absent (state 1 → no-op), `Some(0.0)`
/// MRC floor when an enabled monitor is stale/silent (state 3).
///
/// The cap is composed into the per-pose kinematics contract via the kernel's
/// `apply_perception_cap` (a `min` into `odd_speed_cap_mps`) — so
/// `validate_vehicle_command` stays byte-identical; this only tightens the
/// contract handed to it. Derate-only: `DenyCode` / the deny path are
/// untouched, and an MRC-floor (0.0) cap surfaces as the existing
/// `ClampLinear(0.0)` controlled stop.
///
/// `visibility_range_m` is the **assured-clear distance ahead** — how far into its
/// path the ego has actually observed (perception-supplied; `None` → no occlusion
/// bound, the back-compat path). When present it enforces **RSS Rule 4 (caution under
/// limited visibility)**: a trajectory that outruns its assured clear distance — i.e.
/// commands a speed from which the ego could not stop within what it can see, treating
/// unobserved space as potentially occupied — is refused (`MRCFallback`), exactly as
/// a containment or RSS breach is. Absent input → no-op, so the Nominal WCET path is
/// byte-identical.
// The slow-loop checker legitimately takes many distinct, non-groupable inputs
// (trajectory, corridor, objects, config, odom, posture, + two optional caps).
#[allow(clippy::too_many_arguments)]
pub fn validate_trajectory_slow_capped(
    trajectory: &[TrajectoryPoint],
    corridor: &dyn CorridorSource,
    objects: &[PerceivedObject],
    config: &VehicleConfig,
    latest_odom: Option<&EgoOdom>,
    posture: FleetPosture,
    effective_perception_cap: Option<f64>,
    visibility_range_m: Option<f64>,
    predicted_modes: Option<&[PredictedMode<'_>]>,
    frame_trust: FrameTrust,
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
        &poses, &kernel_corridor, &footprint, frame_trust,
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
    let base_kinematics = match posture {
        FleetPosture::Nominal  => config.to_kinematics_contract(),
        FleetPosture::Degraded => config.to_mrc_kinematics_contract(),
        FleetPosture::LockedOut => unreachable!("handled by the posture short-circuit above"),
    };
    // KIRRA-OCCY-PMON-003: compose the Track-C perception-derate cap (most-
    // conservative-wins `min` into `odd_speed_cap_mps`). Applied uniformly —
    // a no-op when `None` (state 1) or when the cap is above the posture
    // ceiling; an MRC-floor (0.0) cap tightens the ceiling to 0 → controlled
    // stop via the existing per-pose `ClampLinear`. `validate_vehicle_command`
    // is unchanged.
    let kinematics = kirra_core::perception_monitor::apply_perception_cap(
        &base_kinematics,
        effective_perception_cap,
    );
    let initial_steering_deg = current_steering_deg_from_odom(latest_odom, config);
    let mut clamp_seen = false;
    let mut prev_steering_deg = initial_steering_deg;
    // ADR-0029: the angular channel's "current" yaw rate for the Degraded
    // converge-to-stop-and-HOLD gate. Seeded from odometry (the vehicle's
    // actual yaw rate), then carried forward per segment as the prior
    // commanded ω — mirroring `prev_steering_deg`. `None` (no odom) → 0.0
    // (assume stopped → a first-segment rotation reads as re-initiation,
    // fail-closed). Only read under Degraded with a diff-drive `config.angular`.
    let mut prev_omega = latest_odom.map(|o| o.yaw_rate_rads).unwrap_or(0.0);
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

        // ----- Angular channel (ADR-0029) ------------------------------
        //
        // The bicycle steering term (`pose_pair_to_command`) is undefined at
        // v≈0 and falls back to steering=0 — so an in-place rotation is
        // silently passed as "stopped, straight". For a diff-drive class
        // (`config.angular = Some`) we bound the yaw rate directly with the
        // cited-copy diff-drive model: refuse `|ω| > ω_max(v)` (and any
        // non-finite ω). Ackermann profiles (`angular = None`) skip this
        // entirely → the per-pose path is byte-identical. Fail-closed: a
        // breach collapses the trajectory to the MRC, exactly like a
        // containment / per-pose breach.
        // SAFETY: SG3 SG8 | REQ: courier-angular-yaw-bound | TEST: courier_in_place_rotation_at_sane_yaw_is_admitted,courier_in_place_rotation_at_excessive_yaw_mrcs,ackermann_trajectory_has_no_angular_channel,courier_angular_bound_matches_parko_record
        if let Some(ab) = config.angular {
            let a = &trajectory[i];
            let b = &trajectory[i + 1];
            let dt = b.time_from_start_s - a.time_from_start_s;
            if dt > 0.0 {
                // Normalize Δheading to [-π, π] so a heading wrap is not read
                // as a huge yaw.
                let raw = b.pose.heading_rad - a.pose.heading_rad;
                let dheading =
                    raw - std::f64::consts::TAU * (raw / std::f64::consts::TAU).round();
                let omega = dheading / dt;
                // Conservative: the higher segment speed gives the tightest
                // (smallest) rollover ω_max.
                let v_seg = a.velocity_mps.abs().max(b.velocity_mps.abs());
                let posture_factor = if degraded { ab.mrc_posture_factor } else { 1.0 };
                if !omega.is_finite() || omega.abs() > ab.omega_max(v_seg, posture_factor) {
                    return TrajectoryVerdict::MRCFallback;
                }
                // Issue #70 / ADR-0029 — Degraded converge-to-stop-and-HOLD on
                // the ANGULAR channel. The magnitude bound above only caps |ω|;
                // it does NOT force the yaw axis to converge to zero and hold.
                // Under Degraded the courier must decel-to-stop on BOTH axes
                // (linear handled by `enforce_degraded_decel_to_stop`): no
                // angular re-initiation from a stop, no reversal through a stop,
                // non-increasing |ω|. A breach collapses to the MRC, exactly
                // like the linear gate. Mirrors parko-kirra's
                // `degraded_channel_violation` on the angular channel (cited
                // copy). Nominal carries no gate; Ackermann (`angular = None`)
                // never reaches this block → byte-identical.
                // SAFETY: SG8 | REQ: courier-angular-degraded-stop-and-hold | TEST: courier_degraded_angular_reinitiation_from_stop_mrcs,courier_degraded_angular_speed_increase_mrcs,courier_degraded_angular_converging_to_stop_is_admitted,courier_degraded_angular_gate_is_degraded_only,ackermann_degraded_has_no_angular_stop_gate
                if degraded && degraded_angular_violation(prev_omega, omega, ab.stop_epsilon_rad_s) {
                    return TrajectoryVerdict::MRCFallback;
                }
                prev_omega = omega;
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
            // containment cover it. Per-class band (robotaxi 4.0 m; robot tighter).
            if dy_ego.abs() > config.rss_lateral_alignment_tolerance_m {
                continue;
            }

            // RSS over the horizon, per object, as the CONJUNCTION of the two axes (IEEE 2846
            // §5; Shalev-Shwartz et al.): a collision needs the vehicles unsafe longitudinally
            // AND laterally at once. Compute the longitudinal safe distance once (direction
            // matters: an ONCOMING vehicle — velocity projects backward onto the ego's forward
            // axis — is a HEAD-ON closure needing the opposite-direction bound, the sum of both
            // stopping distances; a same-direction lead uses the rear-end bound, #408 Obs 3).
            let obj_lon_v =
                obj.velocity_mps * (obj.heading_rad - traj_point.pose.heading_rad).cos();
            let lon_required = if obj_lon_v < 0.0 {
                // Closing magnitudes; symmetric brake_min (both in their lanes).
                opposite_direction_safe_distance(
                    traj_point.velocity_mps,
                    obj_lon_v.abs(),
                    RSS_REACTION_TIME_S,
                    config.max_accel_mps2,
                    config.max_decel_mps2,
                    config.max_decel_mps2,
                )
            } else {
                longitudinal_safe_distance(
                    traj_point.velocity_mps,
                    obj.velocity_mps,
                    RSS_REACTION_TIME_S,
                    config.max_accel_mps2,
                    config.max_decel_mps2,
                    config.max_decel_mps2,
                )
            };
            let lon_unsafe = dx_ego < lon_required;

            // Longitudinal RSS (rear-end / head-on) — GATED ON LATERAL OVERLAP: a longitudinal
            // collision is only possible when the footprints laterally overlap (the object is in
            // the ego's path). Applying it to an object the ego is laterally CLEAR of — a vehicle
            // being passed, or oncoming traffic safely in the next lane — over-rejected (§4): it
            // was why a car centered in the ego lane could not be overtaken.
            if dy_ego.abs() < RSS_LONGITUDINAL_OVERLAP_M && lon_unsafe {
                return TrajectoryVerdict::MRCFallback;
            }

            // Lateral RSS — required side gap. A side collision needs the footprints ABREAST
            // (longitudinally unsafe — `lon_unsafe`) OR the object CLOSING LATERALLY (a cut-in:
            // its velocity has a lateral component). A longitudinally-SAFE, laterally-STATIONARY
            // object — a stopped queue member, or a stopped lead the ego halts behind — is
            // neither, so it is admitted instead of spuriously MRC'd by the reaction-time swerve
            // term in `lateral_safe_distance` (the §4 over-rejection of a safe same-lane stop).
            // This strictly NARROWS the lateral check (an added precondition), so it can only
            // admit longitudinally-safe + laterally-still objects — never a state with lateral
            // motion or abreast danger. RSS-GATED ON LONGITUDINAL PROXIMITY as before. (Object
            // lateral velocity = the component along the pose normal — Phase 2A assumes 0 if
            // perception lacks per-axis vel; the ego's own lateral motion is carried by the
            // trajectory poses, so containment + the abreast term cover an ego swerve.)
            let obj_lat_vel =
                obj.velocity_mps * (obj.heading_rad - traj_point.pose.heading_rad).sin();
            let lateral_cut_in = obj_lat_vel.abs() > RSS_LATERAL_MOTION_EPS_MPS;
            if dx_ego <= RSS_LONGITUDINAL_CONFLICT_M && (lon_unsafe || lateral_cut_in) {
                let ego_lat_vel = 0.0; // straight-following assumption per §3
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
    }

    // ----- C2) Multi-modal predictive RSS (space-time over modes) ------
    //
    // The snapshot RSS above extrapolates each object from its instantaneous
    // velocity (the CV mode). When perception supplies predicted MODES, also
    // require the ego to be safe against each one, time-matched: at the moment
    // the ego reaches a pose, where could the object be? A predicted cut-in /
    // turn that brings the object into the ego's path is caught here even though
    // the snapshot showed it laterally clear. Uses the SAME §4 lateral-alignment +
    // longitudinal-overlap gating, so a mode that stays in its own lane is skipped
    // (this generalizes §4, it does not regress it). Absent input → no-op.
    if let Some(modes) = predicted_modes {
        // Pass the SAME (posture-/perception-capped) lateral-accel budget the
        // snapshot lateral branch uses, so both passes agree on the side gap.
        if predictive_rss_breach(trajectory, modes, config, kinematics.max_lateral_accel_mps2) {
            return TrajectoryVerdict::MRCFallback;
        }
    }

    // ----- D) Limited-visibility / occlusion bound (RSS Rule 4) --------
    //
    // Gated on a perception-supplied assured-clear distance. A trajectory that
    // outruns it — commanding a speed from which the ego could not stop within
    // what it can see — is refused, treating unobserved space as a potential
    // stopped hazard. Absent input → skipped, so the Nominal path is unchanged.
    if let Some(vis) = visibility_range_m {
        if outruns_assured_clear_distance(trajectory, vis, config.max_decel_mps2) {
            return TrajectoryVerdict::MRCFallback;
        }
    }

    // ----- E) Aggregate ------------------------------------------------
    if clamp_seen {
        TrajectoryVerdict::Clamp
    } else {
        TrajectoryVerdict::Accept
    }
}

/// RSS Rule 4 — the **assured-clear-distance** speed bound. The ego must be able to
/// brake to a stop within the distance it can actually see (`remaining_m`), treating
/// unobserved space beyond as potentially occupied by a stopped hazard. Returns the
/// maximum admissible speed (m/s), including reaction distance (`RSS_REACTION_TIME_S`)
/// for consistency with the longitudinal RSS primitive.
///
/// Solves `v·t + v²/(2a) = remaining` for `v`:
/// `v = sqrt((a·t)² + 2·a·remaining) − a·t`, clamped at 0.
fn assured_clear_distance_speed_cap(remaining_m: f64, brake_decel_mps2: f64) -> f64 {
    let a = brake_decel_mps2.max(0.0);
    let rem = remaining_m.max(0.0);
    let t = RSS_REACTION_TIME_S;
    let v = ((a * t).powi(2) + 2.0 * a * rem).sqrt() - a * t;
    v.max(0.0)
}

/// True if any pose commands a speed above the assured-clear-distance cap for its
/// station along the trajectory (within a small tolerance). `visibility_m` is the
/// assured-clear distance from the trajectory start; as the ego advances, the
/// remaining visible distance shrinks by the arc length travelled — we do not assume
/// new space becomes visible mid-plan (fail-closed; the planner re-plans as it sees
/// further).
fn outruns_assured_clear_distance(
    trajectory: &[TrajectoryPoint],
    visibility_m: f64,
    brake_decel_mps2: f64,
) -> bool {
    let mut traveled = 0.0;
    let mut prev: Option<&TrajectoryPoint> = None;
    for p in trajectory {
        if let Some(pp) = prev {
            traveled += (p.pose.x_m - pp.pose.x_m).hypot(p.pose.y_m - pp.pose.y_m);
        }
        let remaining = visibility_m - traveled;
        let cap = assured_clear_distance_speed_cap(remaining, brake_decel_mps2);
        if p.velocity_mps > cap + OCCLUSION_SPEED_TOL_MPS {
            return true;
        }
        prev = Some(p);
    }
    false
}

/// Max time gap (s) between a predicted object sample and the ego pose it is
/// matched to. Beyond this, the ego's planned trajectory does not actually cover
/// that time (it is shorter than the prediction horizon, or the sample is past
/// the last pose), so the "time-matched ego pose" would be a near pose standing
/// in for a far-future object — a meaningless comparison. One predicted step.
const PREDICTIVE_TIME_MATCH_TOLERANCE_S: f64 = 0.5;

/// The trajectory pose closest in TIME to `t` (the ego's where-am-I-when index),
/// but ONLY if that pose is within `tolerance_s` of `t`. Returns `None` when the
/// nearest pose is further away — i.e. the ego trajectory does not span time `t`
/// — so the caller skips the sample instead of matching a far-future object to a
/// near ego pose (the snapshot RSS still bounds the real object).
fn nearest_in_time(
    trajectory: &[TrajectoryPoint],
    t: f64,
    tolerance_s: f64,
) -> Option<&TrajectoryPoint> {
    let nearest = trajectory
        .iter()
        .min_by(|a, b| (a.time_from_start_s - t).abs().total_cmp(&(b.time_from_start_s - t).abs()))?;
    if (nearest.time_from_start_s - t).abs() <= tolerance_s {
        Some(nearest)
    } else {
        None
    }
}

/// True if any predicted mode brings an object into an RSS shortfall with the
/// time-matched ego pose. Mirrors the snapshot RSS pass in full (same `dx_ego`/`dy_ego`
/// ego-frame projection, same §4 lateral-alignment gating, same same-/opposite-direction
/// longitudinal primitive, AND the same lateral side-RSS conjunction partner), but
/// evaluates the object at its PREDICTED position+velocity (derived from the inter-sample
/// motion) rather than its snapshot.
///
/// The lateral branch is the load-bearing reason this pass exists: a predicted cut-in /
/// turn-in that rolls the object into the ego's path in the mid lateral band
/// (`RSS_LONGITUDINAL_OVERLAP_M` ≤ |dy| ≤ `rss_lateral_alignment_tolerance_m`) is laterally
/// clear at the snapshot AND outside the longitudinal-overlap band, so neither the snapshot
/// nor a longitudinal-only predictive pass would catch it. The lateral conjunction (fire on
/// ABREAST `lon_unsafe` OR closing-laterally `lateral_cut_in`, gated on longitudinal
/// proximity) closes that gap. `max_lateral_accel_mps2` is the (posture-/perception-capped)
/// per-pose contract's lateral-accel bound, so the predictive lateral check uses the SAME
/// side-gap budget as the snapshot pass.
// SAFETY: SG1 | REQ: multi-modal-predictive-rss-bound | TEST: predictive_rss_catches_a_predicted_cut_in,predictive_rss_does_not_regress_a_lane_keeping_neighbor,predictive_rss_is_a_no_op_when_no_modes_are_supplied,rss_conjunction_still_rejects_a_lateral_cut_in_at_a_safe_longitudinal_distance,predictive_rss_catches_a_mid_band_lateral_cut_in,predictive_rss_fails_closed_on_modes_supplied_but_all_unevaluable_b3,predictive_rss_fails_closed_on_modes_with_no_evaluable_window_b3
fn predictive_rss_breach(
    trajectory: &[TrajectoryPoint],
    modes: &[PredictedMode<'_>],
    config: &VehicleConfig,
    max_lateral_accel_mps2: f64,
) -> bool {
    // B3: track whether ANY sample window was actually evaluable. A non-monotonic
    // `dt` or an out-of-span time match means "couldn't evaluate this sample" —
    // distinct from the geometric gates below, which ARE evaluated determinations
    // ("evaluated → not a threat"). If a non-empty mode set produces NOT ONE
    // evaluable window, the cut-in detector checked nothing; see the fail-closed
    // guard after the loop.
    let mut evaluated_any = false;
    for mode in modes {
        for pair in mode.samples.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let dt = b.time_from_start_s - a.time_from_start_s;
            if dt <= 0.0 {
                continue; // non-monotonic samples — unevaluable (see post-loop guard)
            }
            let ovx = (b.pos.x_m - a.pos.x_m) / dt;
            let ovy = (b.pos.y_m - a.pos.y_m) / dt;

            let Some(ego) =
                nearest_in_time(trajectory, a.time_from_start_s, PREDICTIVE_TIME_MATCH_TOLERANCE_S)
            else {
                continue; // no ego pose within tolerance at this time — unevaluable
            };
            // Past both unevaluable gates: this window WAS evaluated (whatever the
            // geometric verdict below).
            evaluated_any = true;
            let dx = a.pos.x_m - ego.pose.x_m;
            let dy = a.pos.y_m - ego.pose.y_m;
            let cos_h = ego.pose.heading_rad.cos();
            let sin_h = ego.pose.heading_rad.sin();
            let dx_ego = cos_h * dx + sin_h * dy; // longitudinal
            let dy_ego = -sin_h * dx + cos_h * dy; // lateral

            if dx_ego <= 0.0 {
                continue; // behind the ego at that time
            }
            if dy_ego.abs() > config.rss_lateral_alignment_tolerance_m {
                continue; // predicted to be in another corridor — containment covers it
            }

            // Longitudinal primitive — computed ONCE and used by both axes' gates
            // (mirrors the snapshot pass). Direction-aware: an oncoming predicted
            // closure (negative projected velocity) needs the opposite-direction
            // (sum-of-stopping-distances) bound, otherwise the rear-end bound.
            let obj_lon_v = ovx * cos_h + ovy * sin_h; // predicted closing component
            let lon_required = if obj_lon_v < 0.0 {
                opposite_direction_safe_distance(
                    ego.velocity_mps,
                    obj_lon_v.abs(),
                    RSS_REACTION_TIME_S,
                    config.max_accel_mps2,
                    config.max_decel_mps2,
                    config.max_decel_mps2,
                )
            } else {
                longitudinal_safe_distance(
                    ego.velocity_mps,
                    obj_lon_v,
                    RSS_REACTION_TIME_S,
                    config.max_accel_mps2,
                    config.max_decel_mps2,
                    config.max_decel_mps2,
                )
            };
            let lon_unsafe = dx_ego < lon_required;

            // Longitudinal RSS — gated on lateral footprint overlap (same as snapshot).
            if dy_ego.abs() < RSS_LONGITUDINAL_OVERLAP_M && lon_unsafe {
                return true;
            }

            // Lateral RSS — the §4 CONJUNCTION partner, mirroring the snapshot
            // lateral branch. The object's predicted lateral velocity is the
            // inter-sample motion projected onto the ego pose normal (same rotation
            // as `dy_ego`). Fire on ABREAST (`lon_unsafe`) OR a predicted lateral
            // closure (`lateral_cut_in`), gated on longitudinal proximity. This is
            // the branch that catches a mid-band cut-in the longitudinal-overlap
            // gate skips.
            let obj_lat_v = -ovx * sin_h + ovy * cos_h;
            let lateral_cut_in = obj_lat_v.abs() > RSS_LATERAL_MOTION_EPS_MPS;
            if dx_ego <= RSS_LONGITUDINAL_CONFLICT_M && (lon_unsafe || lateral_cut_in) {
                let lat_required = lateral_safe_distance(
                    0.0, // straight-following assumption per §3 (ego lateral vel)
                    obj_lat_v,
                    max_lateral_accel_mps2,
                    RSS_REACTION_TIME_S,
                );
                if dy_ego.abs() < lat_required {
                    return true;
                }
            }
        }
    }

    // B3 (fail-closed): the caller already treats `predicted_modes == None` /
    // an EMPTY slice as the legitimate "no prediction supplied" no-op. But a
    // NON-EMPTY mode set that produced ZERO evaluable windows — every sample
    // non-monotonic in time, or none within the ego trajectory's time span, or
    // no inter-sample window at all (a sub-`dt` horizon) — means the multi-modal
    // predictive pass, the ONLY layer catching a mid-band cut-in the snapshot
    // filters out, evaluated nothing. Returning `false` here would be a SILENT
    // FAIL-OPEN that a producer can trigger with equal timestamps or a too-short
    // horizon. Fail closed instead: derate to the MRC floor (the caller maps
    // `true` → `TrajectoryVerdict::MRCFallback`). Well-formed modes always have
    // at least one evaluable window (the t≈0 sample matches the ego start pose),
    // so the Nominal path is unaffected.
    if !modes.is_empty() && !evaluated_any {
        return true;
    }

    false
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
///     steering = atan2(δheading * wheelbase, velocity * δt) → degrees
///
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

/// Degraded converge-to-stop-and-HOLD gate for the ANGULAR channel (Issue #70 /
/// ADR-0029) — the yaw-rate analog of the linear `enforce_degraded_decel_to_stop`,
/// a cited copy of parko-kirra's `degraded_channel_violation` on its angular
/// channel. Returns `true` when the proposed segment yaw rate `proposed`
/// violates the decel-to-stop-and-HOLD invariant relative to the current yaw
/// rate `current`, given the angular stop floor `eps` (`STOP_EPSILON_RAD_S`):
///
///   (a) no re-initiation from an angular stop/hold (`|current| ≤ eps` and
///       `|proposed| > eps`);
///   (b) no reversal through a stop while rotating (sign flip with both
///       magnitudes above `eps`);
///   (c) non-increasing magnitude (`|proposed| > |current|`).
///
/// Fails closed on non-finite input. The `1e-9` slack on (c) tolerates
/// floating-point equality on a held-constant yaw rate.
fn degraded_angular_violation(current: f64, proposed: f64, eps: f64) -> bool {
    if !proposed.is_finite() || !current.is_finite() {
        return true;
    }
    let cur = current.abs();
    let prop = proposed.abs();
    // (a) re-initiation from a stop / hold.
    if cur <= eps && prop > eps {
        return true;
    }
    // (b) reversal through a stop while moving.
    if proposed.signum() != current.signum() && cur > eps && prop > eps {
        return true;
    }
    // (c) non-increasing magnitude.
    if prop > cur + 1e-9 {
        return true;
    }
    false
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

    #[test]
    fn nearest_in_time_rejects_a_time_beyond_the_trajectory_span() {
        // A trajectory spanning [0.0, 1.0] s.
        let traj: Vec<TrajectoryPoint> = (0..=10)
            .map(|i| TrajectoryPoint {
                pose: AdapterPose { x_m: i as f64, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 10.0,
                time_from_start_s: i as f64 * 0.1,
            })
            .collect();

        // A time WITHIN the span matches the closest pose.
        let m = nearest_in_time(&traj, 0.55, PREDICTIVE_TIME_MATCH_TOLERANCE_S)
            .expect("an in-span time matches");
        assert!((m.time_from_start_s - 0.5).abs() < 1e-9 || (m.time_from_start_s - 0.6).abs() < 1e-9);

        // A time just past the last pose, but within tolerance, still matches it.
        assert!(
            nearest_in_time(&traj, 1.4, PREDICTIVE_TIME_MATCH_TOLERANCE_S).is_some(),
            "t=1.4 is within 0.5 s of the last pose (1.0) → matched"
        );

        // A FAR-future time (a predicted object beyond the ego's planned horizon)
        // has NO ego pose within tolerance → None, so the predictive pass skips it
        // rather than matching it to the near last pose (A1).
        assert!(
            nearest_in_time(&traj, 2.5, PREDICTIVE_TIME_MATCH_TOLERANCE_S).is_none(),
            "t=2.5 is >0.5 s past the last pose (1.0) → unevaluable, must be None"
        );
    }

    // ----- RSS Rule 4: assured-clear-distance speed bound ------------------

    #[test]
    fn acda_cap_is_zero_at_zero_visibility() {
        // Nothing visible ahead → the ego must already be stopped.
        assert_eq!(assured_clear_distance_speed_cap(0.0, 4.5), 0.0);
        assert_eq!(assured_clear_distance_speed_cap(-5.0, 4.5), 0.0); // past the horizon
    }

    #[test]
    fn acda_cap_is_monotonic_in_visible_distance() {
        let a = 4.5;
        let near = assured_clear_distance_speed_cap(5.0, a);
        let far = assured_clear_distance_speed_cap(30.0, a);
        assert!(far > near, "more visibility ⇒ higher admissible speed: {near} vs {far}");
        // Sanity: the ego must be able to stop within what it sees, incl. reaction.
        let stop_dist = near * RSS_REACTION_TIME_S + near * near / (2.0 * a);
        assert!(stop_dist <= 5.0 + 1e-6, "stopping distance {stop_dist} must fit in 5 m");
    }

    #[test]
    fn outruns_flags_constant_speed_but_not_a_stop_within_visibility() {
        let dt = 0.1;
        // Constant 10 m/s into 5 m → outruns.
        let fast: Vec<TrajectoryPoint> = (0..20)
            .map(|i| TrajectoryPoint {
                pose: AdapterPose { x_m: i as f64 * 10.0 * dt, y_m: 0.0, heading_rad: 0.0 },
                velocity_mps: 10.0,
                time_from_start_s: i as f64 * dt,
            })
            .collect();
        assert!(outruns_assured_clear_distance(&fast, 5.0, 4.5));

        // A 2 m/s decel-to-stop within ~20 m of visibility → does not outrun.
        let mut v = 2.0;
        let mut x = 0.0;
        let stop: Vec<TrajectoryPoint> = (0..30)
            .map(|i| {
                let p = TrajectoryPoint {
                    pose: AdapterPose { x_m: x, y_m: 0.0, heading_rad: 0.0 },
                    velocity_mps: v,
                    time_from_start_s: i as f64 * dt,
                };
                x += v * dt;
                v = (v - 1.5 * dt).max(0.0);
                p
            })
            .collect();
        assert!(!outruns_assured_clear_distance(&stop, 20.0, 4.5));
    }
}

#[cfg(test)]
mod degraded_angular_gate_tests {
    use super::degraded_angular_violation;

    const EPS: f64 = 0.02; // STOP_EPSILON_RAD_S

    #[test]
    fn holding_at_zero_is_not_a_violation() {
        assert!(!degraded_angular_violation(0.0, 0.0, EPS));
        // Below the stop floor either way → still a hold.
        assert!(!degraded_angular_violation(0.01, -0.01, EPS));
    }

    #[test]
    fn reinitiation_from_stop_is_a_violation() {
        // current ~stopped, proposed above the floor → re-initiation.
        assert!(degraded_angular_violation(0.0, 0.3, EPS));
        assert!(degraded_angular_violation(0.01, 0.3, EPS));
        // Sign-independent.
        assert!(degraded_angular_violation(0.0, -0.3, EPS));
    }

    #[test]
    fn speed_increase_is_a_violation() {
        assert!(degraded_angular_violation(0.10, 0.30, EPS));
        assert!(degraded_angular_violation(-0.10, -0.30, EPS));
    }

    #[test]
    fn reversal_through_a_stop_is_a_violation() {
        // Both magnitudes above the floor but opposite sign → reversal.
        assert!(degraded_angular_violation(0.20, -0.10, EPS));
        assert!(degraded_angular_violation(-0.20, 0.10, EPS));
    }

    #[test]
    fn converging_or_constant_is_admitted() {
        assert!(!degraded_angular_violation(0.30, 0.20, EPS)); // decreasing
        assert!(!degraded_angular_violation(0.30, 0.30, EPS)); // constant
        assert!(!degraded_angular_violation(0.30, 0.0, EPS));  // decel to stop
        // Decreasing magnitude on the negative side.
        assert!(!degraded_angular_violation(-0.30, -0.10, EPS));
    }

    #[test]
    fn non_finite_fails_closed() {
        assert!(degraded_angular_violation(0.1, f64::NAN, EPS));
        assert!(degraded_angular_violation(f64::NAN, 0.1, EPS));
        assert!(degraded_angular_violation(0.1, f64::INFINITY, EPS));
    }
}
