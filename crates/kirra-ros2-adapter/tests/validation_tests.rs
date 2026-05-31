// crates/kirra-ros2-adapter/tests/validation_tests.rs
//
// S131 Phase 2A — integration tests for the slow-loop validator.
//
// Each test exercises `validate_trajectory_slow` end-to-end through the
// kernel's `validate_trajectory_containment` + `validate_vehicle_command`
// + `parko_core::rss` modules with the Phase 1 `MockCorridorSource` as
// the corridor seam (Phase 2B replaces it with the real
// Lanelet2CorridorSource).

use kirra_ros2_adapter::{
    config::VehicleConfig,
    corridor::{MockCorridorSource, Point},
    state::{PerceivedObject, Pose, TrajectoryPoint, TrajectoryVerdict},
    validation::validate_trajectory_slow,
};

/// Build a straight n-pose trajectory along +X at uniform velocity.
///
/// Starts at `x_start` so the rear bumper (rear_x = -1.1 m on the default
/// urban vehicle) stays inside the corridor — `MockCorridorSource` starts
/// the corridor at x = 0 so we start ego poses at x = 5 m by default.
fn straight_trajectory(num_poses: usize, velocity_mps: f64, dt: f64) -> Vec<TrajectoryPoint> {
    let x_start = 5.0;
    (0..num_poses)
        .map(|i| TrajectoryPoint {
            pose: Pose {
                x_m: x_start + (i as f64) * velocity_mps * dt,
                y_m: 0.0,
                heading_rad: 0.0,
            },
            velocity_mps,
            time_from_start_s: (i as f64) * dt,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Clean trajectory → Accept
// ---------------------------------------------------------------------------

#[test]
fn test_clean_trajectory_accepts() {
    let trajectory = straight_trajectory(10, 5.0, 0.1);  // 5 m/s for 1 s
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let start = std::time::Instant::now();
    let verdict = validate_trajectory_slow(&trajectory, &corridor, &objects, &cfg, None);
    let elapsed_us = start.elapsed().as_micros();

    // Print the WCET for the report.
    eprintln!("clean_trajectory_accepts elapsed_us = {elapsed_us}");

    assert_eq!(verdict, TrajectoryVerdict::Accept,
        "10-pose straight trajectory at 5 m/s within a 5 m half-width corridor with no \
         objects must Accept; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 2. Corridor departure → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_corridor_departure_rejects() {
    let mut trajectory = straight_trajectory(10, 5.0, 0.1);
    // Pose 5 has the rear axle at y = 6 m → outside the 5 m half-width
    // corridor by ~1 m. The footprint extends another ~0.925 m → way
    // outside.
    trajectory[5].pose.y_m = 6.0;

    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(&trajectory, &corridor, &objects, &cfg, None);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "pose outside the corridor must MRC; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 3. RSS violation → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_rss_violation_rejects() {
    // 5-pose straight trajectory at 10 m/s.
    let trajectory = straight_trajectory(5, 10.0, 0.1);
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    // Stationary object 4 m directly ahead of the first pose (pose 0 is
    // at x = 5; object at x = 9 → 4 m ahead). The RSS longitudinal-
    // required gap at 10 m/s ego against a stopped lead is tens of
    // metres — 4 m must violate.
    let objects = vec![PerceivedObject {
        id: 1,
        pos: Point { x_m: 9.0, y_m: 0.0 },
        velocity_mps: 0.0,
        heading_rad: 0.0,
    }];
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(&trajectory, &corridor, &objects, &cfg, None);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "object 4 m ahead at ego 10 m/s must trigger RSS MRC; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 4. Kinematics DenyBreach → MRCFallback
// ---------------------------------------------------------------------------

#[test]
fn test_kinematic_deny_rejects() {
    // 2-pose trajectory with delta_time_s = 0 → P1 InvalidTimeDelta
    // (a DenyBreach) per kinematics_contract.rs. This is the cheapest
    // deterministic DenyBreach available from a finite trajectory; NaN
    // can't be JSON-encoded so we use the dt=0 path.
    let trajectory = vec![
        TrajectoryPoint {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 5.0,
            time_from_start_s: 0.0,
        },
        TrajectoryPoint {
            pose: Pose { x_m: 5.5, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 5.0,
            time_from_start_s: 0.0,  // dt = 0 → InvalidTimeDelta
        },
    ];
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(&trajectory, &corridor, &objects, &cfg, None);
    assert_eq!(verdict, TrajectoryVerdict::MRCFallback,
        "P1 InvalidTimeDelta DenyBreach must MRC; got {verdict:?}");
}

// ---------------------------------------------------------------------------
// 5. Clamp-only → Clamp
// ---------------------------------------------------------------------------

#[test]
fn test_clamp_returns_clamp() {
    // 2-pose trajectory designed to trigger ClampLinear on the per-pose
    // kinematics (P3 implied-accel) without violating containment or
    // RSS. Acceleration step:
    //   v0 = 5 m/s, v1 = 30 m/s, dt = 0.5 s
    //   implied_accel = (30-5)/0.5 = 50 m/s²  >> kernel default 2.5 m/s².
    // Containment: both poses inside the 5 m half-width corridor, no
    // departure. RSS: empty objects → no violation. So the only
    // intervention is per-pose ClampLinear → expect Clamp.
    let trajectory = vec![
        TrajectoryPoint {
            pose: Pose { x_m: 5.0, y_m: 0.0, heading_rad: 0.0 },
            velocity_mps: 5.0,
            time_from_start_s: 0.0,
        },
        TrajectoryPoint {
            pose: Pose { x_m: 13.75, y_m: 0.0, heading_rad: 0.0 },  // start + avg vel * dt
            velocity_mps: 30.0,
            time_from_start_s: 0.5,
        },
    ];
    let corridor = MockCorridorSource::straight_5m_half_width(200.0);
    let objects: Vec<PerceivedObject> = Vec::new();
    let cfg = VehicleConfig::default_urban();

    let verdict = validate_trajectory_slow(&trajectory, &corridor, &objects, &cfg, None);
    assert_eq!(verdict, TrajectoryVerdict::Clamp,
        "trajectory with per-pose ClampLinear (P3 accel ceiling) but containment + RSS \
         clean must produce Clamp; got {verdict:?}");
}
