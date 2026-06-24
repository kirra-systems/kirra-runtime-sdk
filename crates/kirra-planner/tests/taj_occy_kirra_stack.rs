//! End-to-end robotics-stack integration: **scan → Taj → Occy → KIRRA**.
//!
//! The unit suites prove each new piece against the checker in isolation. This
//! wires them together for the first time and exercises the whole pipeline:
//!
//! 1. **Taj** (`kirra-taj`) turns a synthetic range scan into the perception
//!    contract — a `CorridorSource` + `PerceivedObject`s + health.
//! 2. **Occy** (`kirra-planner`) consumes Taj's corridor *directly*
//!    (`TajCorridor` IS the `CorridorSource` the planner takes) and proposes a
//!    trajectory.
//! 3. **KIRRA** (`validate_trajectory_slow`) judges Occy's proposal against
//!    Taj's corridor + objects.
//!
//! The load-bearing property: KIRRA is the sole safety authority. When Taj sees
//! a hazard, the stack fails closed *even though Occy still proposes motion* —
//! Occy is not trusted to stop; KIRRA stops it.

use kirra_planner::{
    EgoState, GeometricPlanner, Goal, PlanInput, Planner, ProposalKind,
};
// Re-exported by kirra-planner from the locked upstream contract.
use kirra_planner::{FleetPosture, Pose, TrajectoryVerdict};
use kirra_ros2_adapter::{validate_trajectory_slow, VehicleConfig};
use kirra_taj::{LaserScan, TajConfig, TajPhaseA};
use std::f64::consts::PI;

/// Forward 181-ray scan (`-π/2 .. +π/2`, 1° steps) from a per-ray range fn.
fn forward_scan<F: Fn(f64) -> Option<f64>>(range_max: f64, stamp_ms: u64, f: F) -> LaserScan {
    let n = 181usize;
    let angle_min = -PI / 2.0;
    let angle_inc = PI / (n as f64 - 1.0);
    let ranges = (0..n)
        .map(|i| {
            let theta = angle_min + i as f64 * angle_inc;
            match f(theta) {
                Some(r) if r >= 0.05 && r <= range_max => r as f32,
                _ => (range_max + 1.0) as f32, // no return
            }
        })
        .collect();
    LaserScan {
        angle_min_rad: angle_min,
        angle_increment_rad: angle_inc,
        range_min_m: 0.05,
        range_max_m: range_max,
        ranges,
        stamp_ms,
    }
}

/// Two walls at `y = ±half_width`, plus an optional in-path blob: rays within
/// `±blob_half_angle` return at `blob_x / cos θ` (a cluster near `(blob_x, 0)`).
fn corridor_scan(half_width: f64, blob: Option<(f64, f64)>) -> LaserScan {
    forward_scan(20.0, 1, |theta| {
        let s = theta.sin();
        let wall = if s.abs() < 1e-3 { f64::INFINITY } else { half_width / s.abs() };
        let blob_r = match blob {
            Some((bx, half_angle)) if theta.abs() < half_angle => bx / theta.cos(),
            _ => f64::INFINITY,
        };
        let r = wall.min(blob_r);
        if r.is_finite() {
            Some(r)
        } else {
            None
        }
    })
}

/// Run the full stack and return `(Occy's proposal kind, KIRRA's verdict)`.
fn run_stack(
    scan: &LaserScan,
    ego_x: f64,
    goal_x: f64,
    posture: FleetPosture,
) -> (ProposalKind, TrajectoryVerdict) {
    // 1) Perception. A 20 m forward horizon so the corridor extends well past
    //    the plan (the footprint's front near the goal must stay inside it).
    let taj = TajPhaseA::new(TajConfig { forward_extent_m: 20.0, ..Default::default() });
    let perception = taj.process(scan, 2);

    // 2) Planning — Occy consumes Taj's corridor AND objects directly.
    let input = PlanInput {
        ego: EgoState {
            pose: Pose { x_m: ego_x, y_m: 0.0, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal { target: Pose { x_m: goal_x, y_m: 0.0, heading_rad: 0.0 } },
        map: &perception.corridor,
        objects: &perception.objects,
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: posture.clone(),
        target_speed_mps: None,
    };
    let mut planner = GeometricPlanner::default();
    let plan = planner.plan(&input);

    // 3) Checking — KIRRA judges the proposal against Taj's corridor + objects.
    let verdict = validate_trajectory_slow(
        &plan.trajectory,
        &perception.corridor,
        &perception.objects,
        &VehicleConfig::default_urban(),
        None,
        posture,
    );
    (plan.kind, verdict)
}

#[test]
fn clear_corridor_nominal_stack_admits() {
    // Clear 5 m-half-width corridor, no obstacle: Taj reports a healthy wide
    // corridor, Occy proposes centerline motion, KIRRA admits it.
    let scan = corridor_scan(5.0, None);
    let (kind, verdict) = run_stack(&scan, 2.0, 6.0, FleetPosture::Nominal);

    assert_eq!(kind, ProposalKind::Motion, "clear path → Occy proposes motion");
    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "full stack admits a clear-corridor plan, got {verdict:?}"
    );
}

#[test]
fn obstacle_in_path_stack_fails_closed() {
    // Same corridor but a blob dead ahead at ~4 m. Defense in depth, both layers
    // active: Occy is now obstacle-aware so it brakes/HOLDs short of the object
    // (a controlled stop, not driving in), AND KIRRA independently MRCs the
    // lane-blocking object (it is the safety authority). The stack fails closed
    // end-to-end regardless of which layer you trust.
    let scan = corridor_scan(5.0, Some((4.0, 0.12)));
    let (_kind, verdict) = run_stack(&scan, 2.0, 6.0, FleetPosture::Nominal);

    assert_eq!(
        verdict,
        TrajectoryVerdict::MRCFallback,
        "lane-blocking hazard → KIRRA stops the plan, got {verdict:?}"
    );
}

#[test]
fn lockedout_posture_flows_through_stack() {
    // Posture propagates through the whole stack: in LockedOut, Occy may only
    // propose safe-stop, and KIRRA refuses all motion (the fast loop MRCs).
    let scan = corridor_scan(5.0, None);
    let (kind, verdict) = run_stack(&scan, 2.0, 6.0, FleetPosture::LockedOut);

    assert_eq!(kind, ProposalKind::SafeStop, "LockedOut → Occy proposes only safe-stop");
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback, "LockedOut → KIRRA refuses motion");
}

#[test]
fn route_around_in_corridor_object_admits() {
    // The #451 end-to-end win: a wide drivable corridor with an off-center object
    // detected in it (Phase-B-style: object separate from the free-space corridor)
    // → Occy routes AROUND it → KIRRA ADMITS the offset proposal. Contrast the
    // dead-center case (fails closed); a passable object yields progress.
    let taj = TajPhaseA::new(TajConfig { forward_extent_m: 40.0, ..Default::default() });
    let scan = corridor_scan(5.0, None); // clear 5 m corridor (walls only)
    let mut perception = taj.process(&scan, 2);
    // A detected obstacle off-center in the lane (not part of the corridor walls).
    perception.objects.push(kirra_ros2_adapter::state::PerceivedObject {
        id: 99,
        pos: kirra_ros2_adapter::corridor::Point { x_m: 20.0, y_m: 3.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
        vel: kirra_ros2_adapter::corridor::Point { x_m: 0.0, y_m: 0.0 },
    });

    let input = PlanInput {
        ego: EgoState {
            pose: Pose { x_m: 8.0, y_m: 0.0, heading_rad: 0.0 },
            linear_x_mps: 2.0,
            yaw_rate_rads: 0.0,
            stamp_ms: 0,
        },
        goal: Goal { target: Pose { x_m: 30.0, y_m: 0.0, heading_rad: 0.0 } },
        map: &perception.corridor,
        objects: &perception.objects,
        controls: &[],
        lane_boundaries: &[],
        motion: &[],
        predicted_paths: &[],
        cedes_to_ego_ids: &[],
        lane_change_to_m: None,
        no_overtake_ids: &[],
        drivable: None,
        posture: FleetPosture::Nominal,
        target_speed_mps: None,
    };
    let mut planner = GeometricPlanner::default();
    let plan = planner.plan(&input);
    let verdict = validate_trajectory_slow(
        &plan.trajectory,
        &perception.corridor,
        &perception.objects,
        &VehicleConfig::default_urban(),
        None,
        FleetPosture::Nominal,
    );

    let min_y = plan.trajectory.iter().map(|t| t.pose.y_m).fold(0.0, f64::min);
    assert_eq!(plan.kind, ProposalKind::Motion, "routes around, not stops");
    assert!(min_y <= -1.0, "path offsets around the object, got min_y {min_y}");
    assert!(
        matches!(verdict, TrajectoryVerdict::Accept | TrajectoryVerdict::Clamp),
        "stack admits the route-around, got {verdict:?}"
    );
}
